// SPDX-FileCopyrightText: 2026 Igor Santos
// SPDX-License-Identifier: Apache-2.0

//! Free disk space, read behind a trait so tests set the value. On a platform
//! without `statvfs` the space is unknown and the pressure rule is skipped.

use std::io;
use std::path::Path;

const GIB: u64 = 1024 * 1024 * 1024;

/// Free and total bytes of the volume that holds a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Space {
    pub free_bytes: u64,
    pub total_bytes: u64,
}

pub trait SpaceProbe {
    fn space(&self, path: &Path) -> io::Result<Space>;
}

/// The volume the path sits on, as the operating system reports it.
pub struct RealProbe;

impl SpaceProbe for RealProbe {
    #[cfg(unix)]
    fn space(&self, path: &Path) -> io::Result<Space> {
        let stat = rustix::fs::statvfs(path)?;
        Ok(Space {
            free_bytes: stat.f_bavail.saturating_mul(stat.f_frsize),
            total_bytes: stat.f_blocks.saturating_mul(stat.f_frsize),
        })
    }

    #[cfg(not(unix))]
    fn space(&self, _path: &Path) -> io::Result<Space> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "free space is not read on this platform",
        ))
    }
}

/// Whether `space` is under either limit: fewer than `warn_gib` GiB free, or
/// free space under `warn_percent` percent of the volume. Exactly at a limit
/// is not under it.
pub fn under_pressure(space: &Space, warn_gib: u32, warn_percent: u32) -> bool {
    let under_gib = space.free_bytes < u64::from(warn_gib) * GIB;
    let under_percent = u128::from(space.free_bytes) * 100
        < u128::from(space.total_bytes) * u128::from(warn_percent);
    under_gib || under_percent
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOTAL: u64 = 1000 * GIB;

    fn space(free: u64) -> Space {
        Space {
            free_bytes: free,
            total_bytes: TOTAL,
        }
    }

    #[test]
    fn pressure_exact_gib_limit() {
        assert!(!under_pressure(&space(10 * GIB), 10, 1));
        assert!(under_pressure(&space(10 * GIB - 1), 10, 1));
    }

    #[test]
    fn pressure_exact_percent_limit() {
        let five_percent = TOTAL / 20;
        assert!(!under_pressure(&space(five_percent), 1, 5));
        assert!(under_pressure(&space(five_percent - 1), 1, 5));
    }

    #[test]
    fn pressure_triggers_on_either_key() {
        // Under the GiB limit only, then under the percent limit only.
        assert!(under_pressure(&space(9 * GIB), 10, 0));
        assert!(under_pressure(&space(40 * GIB), 1, 5));
        assert!(!under_pressure(&space(400 * GIB), 10, 5));
    }

    #[cfg(unix)]
    #[test]
    fn the_real_probe_reads_the_temp_volume() {
        let read = RealProbe.space(&std::env::temp_dir()).unwrap();
        assert!(read.total_bytes > 0 && read.free_bytes <= read.total_bytes);
    }
}
