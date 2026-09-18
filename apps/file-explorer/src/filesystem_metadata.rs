//! Metadata and control attributes exposed by the Tilo filesystem projection.
//!
//! A host without that projection must not acknowledge a control command by
//! storing an unrelated local attribute. Native Windows currently has no Tilo
//! mount provider, so these operations report `Unsupported` there.

#[cfg(unix)]
pub use xattr::{get, remove, set};

#[cfg(not(unix))]
use std::{ffi::OsStr, io, path::Path};

/// Read an attribute supplied by the filesystem projection.
///
/// # Errors
/// Returns `Unsupported` when this host has no projection provider.
#[cfg(not(unix))]
pub fn get(_path: impl AsRef<Path>, _name: impl AsRef<OsStr>) -> io::Result<Option<Vec<u8>>> {
    Err(unavailable())
}

/// Submit a control attribute to the filesystem projection.
///
/// # Errors
/// Returns `Unsupported` without modifying the file when no provider exists.
#[cfg(not(unix))]
pub fn set(_path: impl AsRef<Path>, _name: impl AsRef<OsStr>, _value: &[u8]) -> io::Result<()> {
    Err(unavailable())
}

/// Remove an attribute through the filesystem projection.
///
/// # Errors
/// Returns `Unsupported` without modifying the file when no provider exists.
#[cfg(not(unix))]
pub fn remove(_path: impl AsRef<Path>, _name: impl AsRef<OsStr>) -> io::Result<()> {
    Err(unavailable())
}

#[cfg(not(unix))]
fn unavailable() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Tilo filesystem projection is unavailable on this host",
    )
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn unavailable_projection_never_reports_a_successful_control_write() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("document.txt");
        std::fs::write(&path, b"local document").unwrap();
        assert_eq!(
            get(&path, crate::model::SYNC_XATTR).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            set(&path, crate::model::AVAILABILITY_CONTROL_XATTR, b"evict")
                .unwrap_err()
                .kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(
            remove(&path, crate::model::PIN_XATTR).unwrap_err().kind(),
            io::ErrorKind::Unsupported
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"local document");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        assert_eq!(crate::model::read_sync_state(&path), None);
        let history = crate::framework_history::MountedFileHistory::open(&path);
        assert!(matches!(
            history,
            Err(error) if error.kind() == std::io::ErrorKind::Unsupported
        ));
        assert!(crate::received_shares::admit_link("unavailable-service".into()).is_err());
        assert!(
            crate::sharing::create_one_use_view_link(&path, collab_types::FileRecordId::new())
                .is_err()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"local document");
    }
}
