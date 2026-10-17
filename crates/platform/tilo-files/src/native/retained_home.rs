//! Names in the launcher's retained-Home projection. These are routing hints,
//! not grants: opening requires independently retained file/parent objects and
//! admission by the host under the original invocation. This module opens no
//! files and never constructs a filesystem root from an app-supplied path.
#[cfg(unix)]
use std::path::Component;
use std::path::Path;

#[cfg(unix)]
const ROOT: &str = "/run/tilo/retained-home";

/// Whether a path can be represented in the retained-Home protocol.
/// This conveys no access authority, including for symbolic links.
pub fn contains(path: &Path) -> bool {
    components(path).is_some()
}

/// Lossless relative names for a host request, rejecting parent traversal.
/// No file is opened and no path is canonicalized.
#[cfg(unix)]
pub fn components(path: &Path) -> Option<Vec<Vec<u8>>> {
    use std::os::unix::ffi::OsStrExt as _;
    path.strip_prefix(ROOT)
        .ok()?
        .components()
        .map(|part| match part {
            Component::Normal(name) => Some(name.as_bytes().to_vec()),
            _ => None,
        })
        .collect()
}

/// The retained-Home protocol requires POSIX byte names.
#[cfg(not(unix))]
pub fn components(_path: &Path) -> Option<Vec<Vec<u8>>> {
    None
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn components_preserve_non_utf8_names_and_refuse_parent_escape() {
        use std::os::unix::ffi::OsStrExt as _;
        let name = std::ffi::OsStr::from_bytes(b"/run/tilo/retained-home/folder/\xff.txt");
        assert_eq!(
            components(Path::new(name)).expect("POSIX names"),
            vec![b"folder".to_vec(), b"\xff.txt".to_vec()]
        );
        assert!(components(Path::new("/elsewhere/file.txt")).is_none());
        assert!(components(Path::new("/run/tilo/retained-home/../file.txt")).is_none());
        assert!(!contains(Path::new("/run/tilo/retained-home-other/file")));
    }
}
