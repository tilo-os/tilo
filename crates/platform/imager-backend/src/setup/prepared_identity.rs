//! Durable installer identity authority. Unsupported hosts never mint custody.
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
#[cfg(unix)]
pub use tilo_identity_host::enrolled_transport::{
    CurrentDeviceDeclaration, CurrentDeviceReference, EnrolledDevice, TransportReceipt,
};
#[cfg(not(unix))]
#[path = "prepared_identity/unsupported.rs"]
mod unavailable;
#[cfg(not(unix))]
pub use unavailable::PreparedIdentity;

/// Provider selection remains a backend decision.
pub fn storage_for_session() -> tilo_identity_host::provisioning::IdentityStorage {
    #[cfg(unix)]
    {
        tilo_identity_host::provisioning::IdentityStorage::for_session()
    }
    #[cfg(not(unix))]
    {
        tilo_identity_host::provisioning::IdentityStorage::Platform
    }
}

#[cfg(not(unix))]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Installer identity custody is unavailable on this host",
    )
}

/// Refuse before publishing restart records on hosts without identity custody.
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
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CurrentDeviceDeclaration {
    schema: u32,
    user_public_key: [u8; 32],
    device_public_key: [u8; 32],
    record_name: String,
    record_sha256: [u8; 32],
    certificate_sha256: [u8; 32],
    fleet_sha256: [u8; 32],
}
#[cfg(not(unix))]
impl CurrentDeviceDeclaration {
    pub fn public_key(&self) -> [u8; 32] {
        self.user_public_key
    }
    pub fn device_public_key(&self) -> [u8; 32] {
        self.device_public_key
    }
    pub fn record_name(&self) -> &str {
        &self.record_name
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
#[cfg(not(unix))]
pub struct TransportReceipt {
    pub user_public_key: [u8; 32],
    pub device_public_key: [u8; 32],
    pub sealed_bytes: u64,
}

/// A held restart generation lock, not a path-based grant.
pub use advisory_lock::FileLock as DraftLock;

#[cfg(all(test, not(unix)))]
#[path = "prepared_identity/unsupported_tests.rs"]
mod unsupported_tests;
