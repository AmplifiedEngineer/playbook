// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The model boundary. The real adapter calls the installed `claude` CLI
//! through the injected process runner, so no client library is added.

use super::{Arm, E2eTask};
use crate::process::ProcessRunner;
use serde::Deserialize;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::path::PathBuf;

const PROGRAM: &str = "claude";
const CENTS_PER_DOLLAR: f64 = 100.0;
const READ_TOOLS: &str = "Read,Grep,Glob";
const INDEX_TOOL: &str = "Bash(playbook code:*)";
const INDEX_HINT: &str = " Use `playbook code symbol` and `playbook code refs` to look names up.";
const NO_INDEX_HINT: &str = " Search the files with the tools you have.";

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("model call failed: {0}")]
pub struct ModelError(pub String);

/// One call. The snapshot and temperature travel with it so a stub can
/// assert them.
pub struct Request<'a> {
    pub snapshot: &'a str,
    pub temperature: u32,
    pub arm: Arm,
    pub task: &'a E2eTask,
}

pub struct Reply {
    pub text: String,
    pub tokens: u64,
}

pub trait Model {
    fn answer(&self, request: &Request<'_>) -> Result<Reply, ModelError>;
}

/// Total spend so far, in integer cents.
pub trait CostMeter {
    fn spent_cents(&self) -> u64;
}

#[derive(Deserialize)]
struct Usage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
}

#[derive(Deserialize)]
struct CliOutput {
    result: String,
    #[serde(default)]
    total_cost_usd: f64,
    usage: Usage,
}

/// The `claude` CLI as a model, and the meter of what its calls cost.
pub struct ClaudeCli<R: ProcessRunner> {
    pub runner: R,
    /// Checkout of each repo by name; the call runs inside it.
    pub checkouts: BTreeMap<String, PathBuf>,
    spent: Cell<u64>,
}

impl<R: ProcessRunner> ClaudeCli<R> {
    pub fn new(runner: R, checkouts: BTreeMap<String, PathBuf>) -> Self {
        Self {
            runner,
            checkouts,
            spent: Cell::new(0),
        }
    }
}

impl<R: ProcessRunner> CostMeter for ClaudeCli<R> {
    fn spent_cents(&self) -> u64 {
        self.spent.get()
    }
}

impl<R: ProcessRunner> Model for ClaudeCli<R> {
    fn answer(&self, request: &Request<'_>) -> Result<Reply, ModelError> {
        let cwd = self
            .checkouts
            .get(&request.task.repo)
            .ok_or_else(|| ModelError(format!("no checkout for {}", request.task.repo)))?;
        let (tools, hint) = match request.arm {
            Arm::With => (format!("{READ_TOOLS},{INDEX_TOOL}"), INDEX_HINT),
            Arm::Without => (READ_TOOLS.to_string(), NO_INDEX_HINT),
        };
        let args: Vec<String> = [
            "-p",
            &format!("{}{hint}", request.task.question),
            "--model",
            request.snapshot,
            "--output-format",
            "json",
            "--allowedTools",
            &tools,
        ]
        .map(String::from)
        .to_vec();
        let out = self
            .runner
            .run(PROGRAM, &args, cwd)
            .map_err(|e| ModelError(format!("{e:?}")))?;
        let parsed: CliOutput =
            serde_json::from_str(&out).map_err(|e| ModelError(e.to_string()))?;
        let cents = (parsed.total_cost_usd * CENTS_PER_DOLLAR).ceil() as u64;
        self.spent.set(self.spent.get() + cents);
        let u = parsed.usage;
        Ok(Reply {
            text: parsed.result,
            tokens: u.input_tokens
                + u.output_tokens
                + u.cache_creation_input_tokens
                + u.cache_read_input_tokens,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::e2e::{draw, MODEL_SNAPSHOT, TEMPERATURE};
    use crate::process::RunError;
    use crate::tasks::TaskSet;
    use std::path::Path;
    use std::sync::Mutex;

    struct Fake(Mutex<Vec<Vec<String>>>);

    impl ProcessRunner for Fake {
        fn run(&self, program: &str, args: &[String], _: &Path) -> Result<String, RunError> {
            assert_eq!(program, PROGRAM);
            self.0.lock().unwrap().push(args.to_vec());
            Ok(r#"{"result":"[]","total_cost_usd":0.011,"usage":{"input_tokens":10,"output_tokens":5,"cache_read_input_tokens":2}}"#.into())
        }
    }

    #[test]
    fn the_cli_adapter_pins_the_snapshot_meters_cost_and_scopes_tools_by_arm() {
        let task = draw(&TaskSet::bundled().unwrap(), 1).remove(0);
        let checkouts = BTreeMap::from([(task.repo.clone(), PathBuf::from("."))]);
        let cli = ClaudeCli::new(Fake(Mutex::default()), checkouts);
        let mut tokens = 0;
        for arm in [Arm::Without, Arm::With] {
            let request = Request {
                snapshot: MODEL_SNAPSHOT,
                temperature: TEMPERATURE,
                arm,
                task: &task,
            };
            tokens = cli.answer(&request).unwrap().tokens;
        }
        assert_eq!(tokens, 17);
        assert_eq!(cli.spent_cents(), 4);
        let calls = cli.runner.0.lock().unwrap();
        assert!(calls
            .iter()
            .all(|a| a.contains(&MODEL_SNAPSHOT.to_string())));
        assert!(!calls[0].iter().any(|a| a.contains(INDEX_TOOL)));
        assert!(calls[1].iter().any(|a| a.contains(INDEX_TOOL)));
    }

    #[test]
    fn a_missing_checkout_is_an_error() {
        let task = draw(&TaskSet::bundled().unwrap(), 1).remove(0);
        let cli = ClaudeCli::new(Fake(Mutex::default()), BTreeMap::new());
        let request = Request {
            snapshot: MODEL_SNAPSHOT,
            temperature: TEMPERATURE,
            arm: Arm::With,
            task: &task,
        };
        assert!(cli.answer(&request).is_err());
    }
}
