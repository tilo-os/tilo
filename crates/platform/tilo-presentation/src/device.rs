//! The identity of a DRM device node.
//!
//! A GPU is named to the kernel by the numbers of its device node
//! (`/dev/dri/renderD128`); that is the identity a compositor and a process
//! handing it frames agree on.

use std::path::Path;

/// The (major, minor) of the device node at `path`, as the filesystem
/// reports it; `None` where the path is no device node.
#[cfg(target_os = "linux")]
#[must_use]
pub fn device_numbers(path: &Path) -> Option<(u32, u32)> {
    use std::os::unix::fs::MetadataExt as _;
    let device = std::fs::metadata(path).ok()?.rdev();
    // A file that is not a device node reports 0, which names no GPU.
    if 0 == device {
        return None;
    }
    Some((libc::major(device), libc::minor(device)))
}

#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn device_numbers(_path: &Path) -> Option<(u32, u32)> {
    None
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// `device_numbers` of a path: `(row, path, numbers)`. Each row was a test of its own, the
    /// second with two paths, and a failure names its row and path.
    #[test]
    fn a_path_names_its_devices_numbers_or_no_gpu() {
        let file = std::env::current_exe().expect("this test's own binary");
        let rows = [
            (
                "a_device_node_is_named_by_its_numbers",
                Path::new("/dev/null"),
                Some((1, 3)),
            ),
            (
                "a_file_with_no_device_behind_it_names_no_gpu",
                file.as_path(),
                None,
            ),
            (
                "a_file_with_no_device_behind_it_names_no_gpu",
                Path::new("/nonexistent/renderD128"),
                None,
            ),
        ];
        for (row, path, numbers) in rows {
            assert_eq!(device_numbers(path), numbers, "{row}: {}", path.display());
        }
    }
}
