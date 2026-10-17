//! Immutable transport records, never source admission or person proof.
//! Callers independently retain and revalidate their live source authority.
use std::{fs::File, io};

#[cfg(target_os = "linux")]
mod native;

/// Existing fixed native handoff record roles.
#[derive(Clone, Copy)]
pub enum HandoffRole {
    Pending,
    Read,
}

/// Descriptive identity of the exact held file, not a grant of source access.
/// # Errors
/// Returns an error if file metadata cannot be read or the host is unsupported.
pub fn file_identity(file: &File) -> io::Result<[u64; 2]> {
    #[cfg(target_os = "linux")]
    {
        native::file_identity(file)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        Err(super::Unsupported.into())
    }
}

/// Seal an already admitted record without granting any authority to its bytes.
/// # Errors
/// Returns an error for non-root callers, empty records or records over 4096 bytes,
/// failed file creation, writing, sealing, validation or rewinding, or unsupported
/// hosts.
pub fn seal(bytes: &[u8], role: HandoffRole) -> io::Result<File> {
    #[cfg(target_os = "linux")]
    {
        native::seal(bytes, role)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (bytes, role);
        Err(super::Unsupported.into())
    }
}

#[cfg(all(test, not(target_os = "linux")))]
mod tests {
    use super::*;

    #[test]
    fn unsupported_records_refuse_without_touching_the_file() {
        let file = tempfile::tempfile().unwrap();
        for error in [
            file_identity(&file).unwrap_err(),
            seal(b"record", HandoffRole::Read).unwrap_err(),
            seal(b"record", HandoffRole::Pending).unwrap_err(),
            seal(&[], HandoffRole::Read).unwrap_err(),
        ] {
            assert_eq!(error.kind(), io::ErrorKind::Unsupported);
            assert!(error.get_ref().unwrap().is::<super::super::Unsupported>());
        }
        assert_eq!(file.metadata().unwrap().len(), 0);
    }
}
