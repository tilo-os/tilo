//! The native helper command and progress shared with its caller. Cancelling
//! a local request still requires stopping the helper's process group.

use std::io::{ErrorKind, Read as _};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
pub use tilo_server_protocol::ProcessLease;

use crate::{WindowClosed, restore_owner_from_parent};

/// Build the command for a native caller whose exact process must stay live.
/// The caller owns the helper's process group and its progress path.
pub fn pair_device_command(
    executable: &Path,
    identity_root: &Path,
    identity_dir: &Path,
    label: &str,
    timeout: u64,
    status_path: &Path,
    requester: ProcessLease,
) -> Command {
    let mut command = Command::new(executable);
    command
        .arg("pair-device")
        .arg("--identity-root")
        .arg(identity_root)
        .arg("--identity-dir")
        .arg(identity_dir)
        .args(["--label", label, "--timeout", &timeout.to_string()])
        .arg("--status-file")
        .arg(status_path)
        .arg("--requester-pid")
        .arg(requester.pid().to_string())
        .arg("--requester-start-time")
        .arg(requester.start_time().to_string());
    command
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingCause {
    PermissionDenied,
    StorageFull,
    ReadOnly,
    Unreachable,
    TimedOut,
    InvalidIdentity,
    NotStarted,
    Ended,
    #[serde(other)]
    Unknown,
}

impl PairingCause {
    /// The cause carried by an operating-system error.
    #[must_use]
    pub fn from_io(kind: ErrorKind) -> Option<Self> {
        match kind {
            ErrorKind::PermissionDenied => Some(Self::PermissionDenied),
            ErrorKind::StorageFull | ErrorKind::QuotaExceeded => Some(Self::StorageFull),
            ErrorKind::ReadOnlyFilesystem => Some(Self::ReadOnly),
            ErrorKind::NetworkUnreachable
            | ErrorKind::HostUnreachable
            | ErrorKind::NetworkDown
            | ErrorKind::AddrNotAvailable
            | ErrorKind::ConnectionRefused => Some(Self::Unreachable),
            ErrorKind::TimedOut => Some(Self::TimedOut),
            _ => None,
        }
    }

    /// Recover the cause when only the helper's stderr text reached its caller.
    #[must_use]
    pub fn from_text(text: &str) -> Self {
        text.rsplit_once("(os error ")
            .and_then(|(_, code)| code.strip_suffix(')'))
            .and_then(|code| code.parse::<i32>().ok())
            .and_then(|code| Self::from_io(std::io::Error::from_raw_os_error(code).kind()))
            .unwrap_or(Self::Unknown)
    }

    fn of_error(error: &anyhow::Error) -> Self {
        if error
            .chain()
            .any(<dyn std::error::Error>::is::<WindowClosed>)
        {
            return Self::TimedOut;
        }
        error
            .chain()
            .filter_map(|cause| cause.downcast_ref::<std::io::Error>())
            .find_map(|io| Self::from_io(io.kind()))
            .unwrap_or(Self::Unknown)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairingState {
    WaitingForApproval,
    Complete,
    Failed,
    #[default]
    #[serde(other)]
    Starting,
}

/// The helper's public progress. A completed status still has to be checked
/// against the installed identity by the caller.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct PairingStatus {
    #[serde(default)]
    pub state: PairingState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub record_path: Option<PathBuf>,
    // Older helpers called this resource_path. Publish both names for callers
    // that still use either spelling.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub resource_path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<PairingCause>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl PairingStatus {
    #[must_use]
    pub fn waiting(code: &str, label: &str) -> Self {
        Self {
            state: PairingState::WaitingForApproval,
            verification_code: Some(code.to_owned()),
            label: Some(label.to_owned()),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn complete(code: String, identity: String, record_path: PathBuf) -> Self {
        Self {
            state: PairingState::Complete,
            verification_code: Some(code),
            identity: Some(identity),
            resource_path: Some(record_path.clone()),
            record_path: Some(record_path),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn failed(error: &anyhow::Error) -> Self {
        Self {
            state: PairingState::Failed,
            cause: Some(PairingCause::of_error(error)),
            error: Some(format!("{error:#}")),
            ..Self::default()
        }
    }

    /// Read a small status snapshot. Call from a worker because the path can
    /// block on I/O.
    #[must_use]
    pub fn read(path: &Path) -> Option<Self> {
        const LIMIT: usize = 64 * 1024;
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .ok()?
            .take(1 + LIMIT as u64)
            .read_to_end(&mut bytes)
            .ok()?;
        if bytes.len() > LIMIT {
            return None;
        }
        serde_json::from_slice(&bytes).ok()
    }

    /// Publish a snapshot atomically with the helper's existing permissions.
    ///
    /// # Errors
    /// The snapshot cannot be encoded or written with the target ownership.
    pub fn write(&self, path: &Path) -> anyhow::Result<()> {
        tilo_files::native::atomic::write_with_mode(path, &serde_json::to_vec(self)?, 0o644)?;
        restore_owner_from_parent(path)
    }
}
