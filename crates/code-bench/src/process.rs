// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The injected process runner shared by the tool adapters. Adapters only
//! call tools that are already installed and never download anything.

use std::path::Path;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunError {
    /// The program is not installed.
    NotFound,
    /// The program ran and failed, or could not start.
    Failed(String),
}

/// Runs `program` in `cwd` and returns its standard output.
pub trait ProcessRunner {
    fn run(&self, program: &str, args: &[String], cwd: &Path) -> Result<String, RunError>;
}

pub struct SystemRunner;

impl ProcessRunner for SystemRunner {
    fn run(&self, program: &str, args: &[String], cwd: &Path) -> Result<String, RunError> {
        let out = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .output()
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => RunError::NotFound,
                _ => RunError::Failed(e.to_string()),
            })?;
        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr);
            return Err(RunError::Failed(stderr.trim().to_string()));
        }
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}
