//! How full a disk is: the filesystem holding a path, as the kernel counts
//! it.

use std::path::Path;

/// The size of the filesystem holding a path, and what of it is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskCapacity {
    /// Every byte the filesystem holds.
    pub total_bytes: u64,
    /// The bytes the person can still write: what is free to an ordinary
    /// account, without the blocks kept for the system.
    pub available_bytes: u64,
}

impl DiskCapacity {
    /// The bytes taken: everything not left to write.
    #[must_use]
    pub fn used_bytes(self) -> u64 {
        self.total_bytes.saturating_sub(self.available_bytes)
    }

    /// How full the disk is, from 0 to 1.
    #[must_use]
    pub fn used_fraction(self) -> f32 {
        if self.total_bytes == 0 {
            return 0.0;
        }
        #[expect(
            clippy::cast_precision_loss,
            reason = "a fill fraction needs no more than f64's precision"
        )]
        let fraction = self.used_bytes() as f64 / self.total_bytes as f64;
        #[expect(clippy::cast_possible_truncation, reason = "a fraction in 0..=1")]
        let fraction = fraction.clamp(0.0, 1.0) as f32;
        fraction
    }
}

/// The capacity of the filesystem holding `path`.
pub fn disk_capacity(path: &Path) -> Result<DiskCapacity, String> {
    let stat = rustix::fs::statvfs(path).map_err(|error| error.to_string())?;
    let block = stat.f_frsize.max(1);
    Ok(DiskCapacity {
        total_bytes: stat.f_blocks.saturating_mul(block),
        available_bytes: stat.f_bavail.saturating_mul(block),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The filesystem under a directory that exists has a size, and no more
    /// is free than it holds.
    #[test]
    fn a_real_directory_reports_its_disk() {
        let dir = tempfile::tempdir().unwrap();
        let capacity = disk_capacity(dir.path()).unwrap();
        assert!(capacity.total_bytes > 0);
        assert!(capacity.available_bytes <= capacity.total_bytes);
        assert!((0.0..=1.0).contains(&capacity.used_fraction()));
        assert!(disk_capacity(Path::new("/nonexistent/disk")).is_err());
    }
}
