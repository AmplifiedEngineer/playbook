// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Where data lives. Every playbook tool keeps its databases in one folder,
//! `~/.config/playbook/data`, whichever tool runs; the file name says which
//! feature owns it. This is the only place that names the folder.

use std::path::{Path, PathBuf};

/// The data folder for the user at `home`. It is created (owner-only) when a
/// database is opened in it.
pub fn data_dir(home: &Path) -> PathBuf {
    crate::config::root(home).join("data")
}

/// The file name of the usage database inside the data folder.
pub const USAGE_DB_FILE: &str = "usage.db";

/// The usage database for the user at `home`.
pub fn usage_db(home: &Path) -> PathBuf {
    data_dir(home).join(USAGE_DB_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_lives_in_one_folder_under_the_playbook_root() {
        let home = Path::new("/h");
        assert_eq!(data_dir(home), Path::new("/h/.config/playbook/data"));
        assert_eq!(
            usage_db(home),
            Path::new("/h/.config/playbook/data/usage.db")
        );
    }
}
