//! Credential-free cloud Files access. Credentials remain in the storage service.
//!
//! Native imports retain directory descriptors and output identities through
//! publication and compensation. No pathname fallback is used on unsupported
//! targets: those targets return an explicit unsupported error.
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

/// The directory admitted for an import, not merely its mutable pathname.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DestinationIdentity {
    device: u64,
    inode: u64,
    /// Output metadata distinguishes reused inodes and changed copies. Directory
    /// admission deliberately excludes it: importing changes directory metadata.
    #[serde(default)]
    modified: Option<(i64, i64, u64)>,
}

/// Cumulative progress, delivered on the worker thread, not the UI thread.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TransferProgress {
    pub phase: String,
    pub completed: u64,
    pub bytes: u64,
    pub failed: Vec<String>,
}

/// A verified import or compensation receipt, independent of any app document.
/// Files maps these fields to its existing persisted change variants.
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

/// The caller's shared copy/cut algorithm, also used outside cloud imports.
/// Compensation supplies descriptor-rooted paths and checks identities around
/// each invocation. This callback grants no additional filesystem authority.
pub type MoveInto = fn(&Path, &Path) -> Result<PathBuf, String>;

/// Validate a portable local leaf name before any staging is created.
///
/// # Errors
/// Rejects empty names, traversal, separators, NUL and control characters.
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
