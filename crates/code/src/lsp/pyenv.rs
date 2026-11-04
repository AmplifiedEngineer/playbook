// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Finds the Python interpreter Pyright should use, from files alone. No tool
//! runs, so a repo cannot steer the answer by putting a program on the path.

use std::path::{Path, PathBuf};

/// Where the interpreter was named, highest priority first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Flag,
    Config,
    VirtualEnv,
    DotVenv,
    Venv,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Interpreter {
    Found {
        path: PathBuf,
        origin: Origin,
        /// The project file that points at this layout, when one exists.
        marker: Option<&'static str>,
    },
    /// More than one environment sits at the same level and nothing chooses.
    Ambiguous {
        candidates: Vec<PathBuf>,
    },
    Missing {
        hint: String,
    },
}

#[derive(Clone, Copy)]
pub struct Inputs<'a> {
    pub root: &'a Path,
    /// `--python`.
    pub flag: Option<&'a Path>,
    /// `code.python.interpreter`.
    pub configured: Option<&'a Path>,
    /// The `VIRTUAL_ENV` variable.
    pub virtual_env: Option<&'a Path>,
}

/// Project files that mark a Python layout, and the environment they imply.
const MARKERS: [&str; 3] = ["uv.lock", "poetry.lock", "pyproject.toml"];

pub fn discover(inputs: &Inputs<'_>) -> Interpreter {
    for (explicit, origin) in [
        (inputs.flag, Origin::Flag),
        (inputs.configured, Origin::Config),
    ] {
        if let Some(path) = explicit {
            return if path.is_file() {
                found(path.to_path_buf(), origin, inputs.root)
            } else {
                Interpreter::Missing {
                    hint: format!("{} is not a file; fix the setting", path.display()),
                }
            };
        }
    }
    if let Some(env) = inputs.virtual_env {
        if let Some(python) = python_in(env) {
            return found(python, Origin::VirtualEnv, inputs.root);
        }
    }
    let in_repo: Vec<(PathBuf, Origin)> = [(".venv", Origin::DotVenv), ("venv", Origin::Venv)]
        .into_iter()
        .filter_map(|(dir, origin)| Some((python_in(&inputs.root.join(dir))?, origin)))
        .collect();
    match in_repo.as_slice() {
        [(path, origin)] => found(path.clone(), *origin, inputs.root),
        [] => Interpreter::Missing {
            hint: missing_hint(inputs.root),
        },
        many => Interpreter::Ambiguous {
            candidates: many.iter().map(|(path, _)| path.clone()).collect(),
        },
    }
}

fn found(path: PathBuf, origin: Origin, root: &Path) -> Interpreter {
    Interpreter::Found {
        path,
        origin,
        marker: MARKERS.into_iter().find(|m| root.join(m).is_file()),
    }
}

/// The interpreter inside a virtual environment directory.
fn python_in(env: &Path) -> Option<PathBuf> {
    ["bin/python", "Scripts/python.exe"]
        .into_iter()
        .map(|rel| env.join(rel))
        .find(|candidate| candidate.is_file())
}

fn missing_hint(root: &Path) -> String {
    match MARKERS.into_iter().find(|m| root.join(m).is_file()) {
        Some(marker) => format!(
            "found {marker} but no .venv or venv; create one (for example `python -m venv .venv`) \
             or set code.python.interpreter"
        ),
        None => "no Python project files found; set code.python.interpreter".to_string(),
    }
}

/// What a command says about an environment it could not settle, if anything.
pub fn warning(interpreter: &Interpreter) -> Option<String> {
    match interpreter {
        Interpreter::Found { .. } => None,
        Interpreter::Ambiguous { candidates } => {
            let list: Vec<String> = candidates.iter().map(|p| p.display().to_string()).collect();
            Some(format!(
                "python environment is ambiguous ({}); pass --python or set code.python.interpreter",
                list.join(", ")
            ))
        }
        Interpreter::Missing { hint } => Some(format!("python environment missing: {hint}")),
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::lsp::testkit::scratch;

    fn python(dir: &Path) -> PathBuf {
        let path = dir.join("bin/python");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "").unwrap();
        path
    }

    fn inputs<'a>(root: &'a Path) -> Inputs<'a> {
        Inputs {
            root,
            flag: None,
            configured: None,
            virtual_env: None,
        }
    }

    fn path_of(found: Interpreter) -> (PathBuf, Origin) {
        match found {
            Interpreter::Found { path, origin, .. } => (path, origin),
            other => panic!("expected an interpreter, got {other:?}"),
        }
    }

    #[test]
    fn each_level_beats_the_one_below_it() {
        // Arrange
        let root = scratch("py-prec");
        let dotvenv = python(&root.join(".venv"));
        let outside = scratch("py-prec-out");
        let active = outside.join("active");
        let virtual_env = python(&active);
        let configured = python(&outside.join("configured"));
        let flag = python(&outside.join("flagged"));

        // Act
        let with_all = Inputs {
            flag: Some(&flag),
            configured: Some(&configured),
            virtual_env: Some(&active),
            ..inputs(&root)
        };
        let no_flag = Inputs {
            flag: None,
            ..with_all
        };
        let env_and_dotvenv = Inputs {
            configured: None,
            ..no_flag
        };
        let dotvenv_only = Inputs {
            virtual_env: None,
            ..env_and_dotvenv
        };

        // Assert
        assert_eq!(path_of(discover(&with_all)), (flag.clone(), Origin::Flag));
        assert_eq!(
            path_of(discover(&no_flag)),
            (configured.clone(), Origin::Config)
        );
        assert_eq!(
            path_of(discover(&env_and_dotvenv)),
            (virtual_env.clone(), Origin::VirtualEnv)
        );
        assert_eq!(path_of(discover(&dotvenv_only)), (dotvenv, Origin::DotVenv));
    }

    #[test]
    fn a_venv_directory_alone_resolves_and_both_together_are_ambiguous() {
        // Arrange
        let root = scratch("py-ambiguous");
        let plain = python(&root.join("venv"));
        let alone = discover(&inputs(&root));
        let dot = python(&root.join(".venv"));

        // Act
        let both = discover(&inputs(&root));

        // Assert
        assert_eq!(path_of(alone), (plain.clone(), Origin::Venv));
        let Interpreter::Ambiguous { candidates } = both else {
            panic!("expected ambiguous");
        };
        assert_eq!(candidates, [dot, plain]);
    }

    #[test]
    fn only_a_project_file_is_missing_with_a_hint_naming_it() {
        // Arrange
        let root = scratch("py-missing");
        std::fs::write(root.join("pyproject.toml"), "").unwrap();

        // Act
        let got = discover(&inputs(&root));

        // Assert
        let Interpreter::Missing { hint } = got else {
            panic!("expected missing");
        };
        assert!(
            hint.contains("pyproject.toml") && hint.contains(".venv"),
            "{hint}"
        );
    }

    #[test]
    fn uv_and_poetry_layouts_resolve_to_dot_venv_and_name_their_marker() {
        for marker in ["uv.lock", "poetry.lock"] {
            // Arrange
            let root = scratch(&format!("py-{marker}"));
            std::fs::write(root.join(marker), "").unwrap();
            std::fs::write(root.join("pyproject.toml"), "").unwrap();
            let expected = python(&root.join(".venv"));

            // Act
            let got = discover(&inputs(&root));

            // Assert
            let Interpreter::Found {
                path,
                origin,
                marker: seen,
            } = got
            else {
                panic!("expected an interpreter");
            };
            assert_eq!(
                (path, origin, seen),
                (expected, Origin::DotVenv, Some(marker))
            );
        }
    }

    #[test]
    fn an_explicit_path_that_is_not_a_file_is_missing_not_replaced_by_a_lower_level() {
        // Arrange
        let root = scratch("py-explicit-bad");
        python(&root.join(".venv"));
        let missing = root.join("nope/python");

        // Act
        let got = discover(&Inputs {
            configured: Some(&missing),
            ..inputs(&root)
        });

        // Assert
        let Interpreter::Missing { hint } = got else {
            panic!("expected missing");
        };
        assert!(hint.contains("not a file"), "{hint}");
    }

    #[test]
    fn warnings_exist_only_for_ambiguous_and_missing() {
        let ambiguous = Interpreter::Ambiguous {
            candidates: vec![
                PathBuf::from("/a/.venv/bin/python"),
                PathBuf::from("/a/venv/bin/python"),
            ],
        };
        let text = warning(&ambiguous).unwrap();
        assert!(
            text.contains("ambiguous") && text.contains("/a/venv/bin/python"),
            "{text}"
        );
        assert!(warning(&Interpreter::Missing { hint: "h".into() })
            .unwrap()
            .contains("missing"));
        let found = Interpreter::Found {
            path: PathBuf::from("/p"),
            origin: Origin::Flag,
            marker: None,
        };
        assert_eq!(warning(&found), None);
    }
}
