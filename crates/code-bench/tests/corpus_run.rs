// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Clones the pilot repos over the network. Run with `--run-ignored only`.

use code_bench::corpus::{fetch, GitTransport, Manifest, ThreadSleeper, Tier};

const CACHE_ENV: &str = "CODE_BENCH_CACHE";

#[test]
#[ignore = "clones real repos over the network"]
fn pilot_repos_fetch_at_their_pins() {
    let cache = std::env::var(CACHE_ENV).expect("set CODE_BENCH_CACHE to a directory");
    let cache = std::path::PathBuf::from(cache);
    std::fs::create_dir_all(&cache).unwrap();
    let manifest = Manifest::bundled().unwrap();
    for repo in manifest.tier(Tier::Pilot) {
        let dir = fetch(repo, &cache, &GitTransport, &ThreadSleeper).unwrap();
        assert!(dir.is_dir(), "{}", repo.name);
    }
}
