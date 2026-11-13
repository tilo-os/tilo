//! Credential-free cloud Files access: credentials stay in the storage service.
//! Unsupported targets return an explicit error rather than falling back to a
//! pathname.
use std::path::{Path, PathBuf};

mod consent;
pub use consent::{ImportAuthority, ImportConsent};

#[cfg(unix)]
pub use runtime_fs::cloud;
#[cfg(not(unix))]
pub mod cloud;

#[cfg(unix)]
mod native;
#[cfg(unix)]
pub use native::{compensate, execute, execute_at, pin_destination, restore};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DestinationIdentity {
    device: u64,
    inode: u64,
    #[serde(default)]
    modified: Option<(i64, i64, u64)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransferProgress {
    pub phase: String,
    pub completed: u64,
    pub bytes: u64,
    pub failed: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum TransferReceipt {
    CloudImported {
        from: String,
        to: String,
        destination: DestinationIdentity,
        output: DestinationIdentity,
    },
    CloudTrashed {
        to: String,
        entry: String,
        destination: DestinationIdentity,
        output: DestinationIdentity,
    },
}

/// Completed roots remain available even if a subsequent root fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransferOutcome {
    Done(Vec<TransferReceipt>),
    Failed {
        reason: String,
        partial: Vec<TransferReceipt>,
    },
}

pub type MoveInto = fn(&Path, &Path) -> Result<PathBuf, String>;

pub fn valid_name(name: &str) -> Result<(), String> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains(['/', '\\', '\0'])
        || name.chars().any(char::is_control)
    {
        Err("cloud entry has an invalid local name".into())
    } else {
        Ok(())
    }
}

/// Make only a newly staged copy owner-writable, never the provider source.
pub fn owner_writable(mut permissions: std::fs::Permissions) -> std::fs::Permissions {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        permissions.set_mode(permissions.mode() | 0o200);
    }
    #[cfg(not(unix))]
    permissions.set_readonly(false);
    permissions
}

#[cfg(any(not(unix), test))]
mod unsupported;
#[cfg(not(unix))]
pub use unsupported::{compensate, execute, execute_at, pin_destination, restore};

#[cfg(test)]
mod tests;
#[cfg(all(unix, test))]
#[path = "cloud.rs"]
pub mod unsupported_cloud;
