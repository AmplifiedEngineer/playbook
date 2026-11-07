// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! The runner. Each trial runs both arms on the same task, so the arms
//! always hold the same tasks and repeat count. The dollar cap is checked
//! before every trial and a run cut short is marked partial.

use super::model::{CostMeter, Model, ModelError, Request};
use super::report::{build, Report, Trial};
use super::{grade, Arm, E2eTask, COST_CAP_CENTS, MODEL_SNAPSHOT, TEMPERATURE};

fn ask(model: &dyn Model, task: &E2eTask, arm: Arm) -> Result<(bool, u64), ModelError> {
    let request = Request {
        snapshot: MODEL_SNAPSHOT,
        temperature: TEMPERATURE,
        arm,
        task,
    };
    let reply = model.answer(&request)?;
    let right = task
        .expected
        .as_ref()
        .is_some_and(|expected| grade(expected, &reply.text));
    Ok((right, reply.tokens))
}

pub fn run(
    model: &dyn Model,
    meter: &dyn CostMeter,
    tasks: &[E2eTask],
    repeats: u32,
    source: &str,
) -> Result<Report, ModelError> {
    let excluded = tasks.iter().filter(|t| t.expected.is_none()).count();
    let mut trials = Vec::new();
    let mut complete = true;
    'tasks: for (index, task) in tasks
        .iter()
        .enumerate()
        .filter(|(_, t)| t.expected.is_some())
    {
        for _ in 0..repeats {
            if meter.spent_cents() > COST_CAP_CENTS {
                complete = false;
                break 'tasks;
            }
            let (without_correct, without_tokens) = ask(model, task, Arm::Without)?;
            let (with_correct, with_tokens) = ask(model, task, Arm::With)?;
            trials.push(Trial {
                task: index,
                kind: task.kind,
                with_correct,
                without_correct,
                with_tokens,
                without_tokens,
            });
        }
    }
    Ok(build(
        source,
        MODEL_SNAPSHOT,
        repeats,
        excluded as u32,
        complete,
        meter.spent_cents(),
        &trials,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::e2e::model::Reply;
    use crate::e2e::{Answer, Kind, TASK_COUNT};
    use std::cell::Cell;

    struct Stub {
        right_with: bool,
        right_without: bool,
        calls: Cell<u32>,
    }

    fn stub(right_with: bool, right_without: bool) -> Stub {
        Stub {
            right_with,
            right_without,
            calls: Cell::new(0),
        }
    }

    impl Model for Stub {
        fn answer(&self, r: &Request<'_>) -> Result<Reply, ModelError> {
            assert_eq!(r.snapshot, MODEL_SNAPSHOT);
            assert_eq!(r.temperature, TEMPERATURE);
            self.calls.set(self.calls.get() + 1);
            let right = if r.arm == Arm::With {
                self.right_with
            } else {
                self.right_without
            };
            let a = r.task.expected.clone().unwrap();
            let line = if right { a.line } else { a.line + 100 };
            let tokens = if r.arm == Arm::With { 80 } else { 100 };
            Ok(Reply {
                text: serde_json::to_string(&[Answer { path: a.path, line }]).unwrap(),
                tokens,
            })
        }
    }

    struct Spend(u64);

    impl CostMeter for Spend {
        fn spent_cents(&self) -> u64 {
            self.0
        }
    }

    fn tasks() -> Vec<E2eTask> {
        (0..TASK_COUNT)
            .map(|i| E2eTask {
                id: format!("t{i}"),
                repo: "r".into(),
                kind: Kind::Symbol,
                question: String::new(),
                expected: Some(Answer {
                    path: format!("f{i}.rs"),
                    line: 10,
                }),
            })
            .collect()
    }

    fn go(s: &Stub, spent: u64, tasks: &[E2eTask]) -> Report {
        run(s, &Spend(spent), tasks, 2, "test").unwrap()
    }

    #[test]
    fn right_only_with_the_index_measures_ten_thousand_basis_points() {
        let r = go(&stub(true, false), 0, &tasks());
        assert_eq!(
            (r.difference_bp, r.lower_bp, r.upper_bp),
            (10_000, 10_000, 10_000)
        );
        assert_eq!((r.with_accuracy_bp, r.without_accuracy_bp), (10_000, 0));
        assert_eq!(
            (r.with_tokens_per_task, r.without_tokens_per_task),
            (80, 100)
        );
        assert!(r.complete);
    }

    #[test]
    fn a_model_always_right_gives_zero_difference() {
        assert_eq!(go(&stub(true, true), 0, &tasks()).difference_bp, 0);
    }

    #[test]
    fn a_model_always_wrong_scores_zero_in_both_arms() {
        let r = go(&stub(false, false), 0, &tasks());
        assert_eq!((r.with_accuracy_bp, r.without_accuracy_bp), (0, 0));
    }

    #[test]
    fn the_cap_stops_only_above_five_thousand_cents() {
        for (spent, complete) in [
            (COST_CAP_CENTS - 1, true),
            (COST_CAP_CENTS, true),
            (COST_CAP_CENTS + 1, false),
        ] {
            let s = stub(true, true);
            let r = go(&s, spent, &tasks());
            assert_eq!(r.complete, complete, "{spent}");
            assert_eq!(s.calls.get() == 0, !complete, "{spent}");
        }
    }

    #[test]
    fn a_task_without_an_oracle_answer_is_excluded_and_counted() {
        let mut list = tasks();
        list[0].expected = None;
        let r = go(&stub(true, true), 0, &list);
        assert_eq!((r.excluded, r.tasks), (1, TASK_COUNT as u32 - 1));
    }
}
