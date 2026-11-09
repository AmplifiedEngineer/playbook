// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Shared helpers ported from the retired shell originals.
//! Every hook in `src/hooks` uses this module instead of re-deriving these
//! primitives, so there is exactly one implementation to keep correct.

pub mod account;
pub mod attribution;
pub mod cli_opts;
pub mod config_hash;
pub mod counter;
pub mod emit;
pub mod headless;
pub mod inline_args;
pub mod mode;
pub mod par;
pub mod paths;
pub use playbook_common::{
    atomic, git, gitfacts, hash, org, org_state, payload, proc, repo, session, sqlite,
};
pub use playbook_data as data;
pub mod plugin_state;
pub mod prose;
pub mod prose_ground;
pub mod prose_lint;
pub mod re;
pub mod real_home;
pub mod shell;
pub mod slug;
pub mod stdin;
pub mod time;

pub use atomic::atomic_append;
pub use config_hash::config_hash;
pub use counter::incr_counter;
pub use emit::{
    emit_block, emit_pre_context, emit_pre_deny, emit_pre_updated_input, emit_prompt_context,
    emit_system_message,
};
pub use paths::{
    cc_state_dir, memory_dir, playbook_root, repo_scoped_dir, runtime_root, usage_db_dir, RepoScope,
};
pub use payload::Payload;
pub use proc::{run_with_input, run_with_timeout};
pub use repo::repo_slug;
pub use session::{abspath, home_dir, home_error, session_dir, session_id};

#[cfg(test)]
pub(crate) use playbook_common::test_support;
