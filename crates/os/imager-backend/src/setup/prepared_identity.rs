//! Durable installer identity authority. Unsupported hosts never mint ownership.
use std::io;
#[path = "prepared_identity/files.rs"]
mod files;
pub use files::{ensure_private_tree, read_avatar};
#[path = "prepared_identity/restart_storage.rs"]
mod restart_storage;
pub use restart_storage::RestartStorage;

#[cfg(unix)]
#[path = "prepared_identity/native.rs"]
mod native;
#[cfg(unix)]
pub use native::PreparedIdentity;
pub use tilo_identity::enrolled_transport::{CurrentDeviceDeclaration, TransportReceipt};
#[cfg(unix)]
pub use tilo_identity::enrolled_transport::{CurrentDeviceReference, EnrolledDevice};
#[cfg(not(unix))]
#[path = "prepared_identity/unsupported.rs"]
mod unavailable;
#[cfg(not(unix))]
pub use unavailable::PreparedIdentity;

/// Provider selection remains a backend decision.
pub fn storage_for_session() -> tilo_identity::provisioning::IdentityStorage {
    #[cfg(unix)]
    {
        tilo_identity::provisioning::IdentityStorage::for_session()
    }
    #[cfg(not(unix))]
    {
        tilo_identity::provisioning::IdentityStorage::Platform
    }
}

#[cfg(not(unix))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Installer identity storage is unavailable on this host",
    )
}

/// Refuse before publishing restart records on hosts without identity storage.
pub fn require_supported() -> io::Result<()> {
    #[cfg(unix)]
    {
        Ok(())
    }
    #[cfg(not(unix))]
    {
        Err(unsupported())
    }
}

#[cfg(not(unix))]
enum Never {}
#[cfg(not(unix))]
pub struct CurrentDeviceReference {
    _never: Never,
}
#[cfg(not(unix))]
pub struct EnrolledDevice {
    _never: Never,
}

/// A held restart generation lock, not a path-based grant.
pub use advisory_lock::FileLock as DraftLock;

#[cfg(test)]
#[cfg(not(unix))]
#[path = "prepared_identity/unsupported_tests.rs"]
mod unsupported_tests;
