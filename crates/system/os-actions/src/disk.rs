//! How full a disk is: the filesystem holding a path, as the kernel counts it.

use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskCapacity {
    pub total_bytes: u64,
    /// What an ordinary account can still write, without the blocks kept for the system.
    pub available_bytes: u64,
}

impl DiskCapacity {
    #[must_use]
    pub fn used_bytes(self) -> u64 {
        self.total_bytes.saturating_sub(self.available_bytes)
    }

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
