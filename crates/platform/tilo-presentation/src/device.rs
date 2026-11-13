//! The identity of a DRM device node.
//!
//! A GPU is named to the kernel by the numbers of its device node
//! (`/dev/dri/renderD128`), and that is the identity a compositor and a
//! process handing it frames agree on: one GPU's memory is not another's.

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

/// No DRM device nodes here, so no path names a GPU.
#[cfg(not(target_os = "linux"))]
#[must_use]
pub fn device_numbers(_path: &Path) -> Option<(u32, u32)> {
    None
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn a_device_node_is_named_by_its_numbers() {
        assert_eq!(device_numbers(Path::new("/dev/null")), Some((1, 3)));
    }

    #[test]
    fn a_file_with_no_device_behind_it_names_no_gpu() {
        let file = std::env::current_exe().expect("this test's own binary");
        assert_eq!(device_numbers(&file), None);
        assert_eq!(device_numbers(Path::new("/nonexistent/renderD128")), None);
    }
}
