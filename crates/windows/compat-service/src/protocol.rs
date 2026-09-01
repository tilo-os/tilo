use compat_catalogue::ApplicationQuery;
use serde::{Deserialize, Serialize};

use crate::{ExplainStatus, InstallReceipt, InstallationStatus, LaunchReceipt, RouteKind};

pub const PROTOCOL_VERSION: u16 = 1;

/// The inherited session service accepts only opaque server-owned entry IDs
/// for launch. A query is preview-only and cannot select a launch plan.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "method", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProtocolRequest {
    List {
        version: u16,
    },
    Resolve(Box<ResolveRequest>),
    Launch {
        version: u16,
        entry_id: String,
    },
    /// A distinct manager action is required before the singleton Windows
    /// fallback may be selected.  The server still supplies the entry,
    /// workspace, capability snapshot, and published application id.
    LaunchVmFallback {
        version: u16,
        entry_id: String,
    },
    /// The portal-selected descriptor accompanies this frame via `SCM_RIGHTS`.
    Install {
        version: u16,
        source_name: String,
    },
    RetryInstall {
        version: u16,
        capsule_id: String,
    },
    RemoveLauncher {
        version: u16,
        entry_id: String,
    },
    VendorUninstall {
        version: u16,
        capsule_id: String,
    },
    DeleteData {
        version: u16,
        capsule_id: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResolveRequest {
    pub version: u16,
    pub query: ApplicationQuery,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagedApplication {
    pub id: String,
    pub app_id: String,
    pub display_name: String,
    pub publisher: String,
    pub capsule_id: String,
    pub program_id: String,
    pub explanation: ExplainStatus,
    pub vm_launch_available: bool,
    pub vm_unavailable_reason: Option<String>,
    pub audio_output_available: bool,
    pub audio_unavailable_reason: Option<String>,
    pub installation_status: Option<InstallationStatus>,
    pub remove_launcher_available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ManagedInstallation {
    pub capsule_id: String,
    pub display_name: String,
    pub route: RouteKind,
    pub runner: String,
    pub status: InstallationStatus,
    pub program_count: usize,
    pub can_retry: bool,
    pub can_vendor_uninstall: bool,
    pub can_delete_data: bool,
    pub detail: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub enum ProtocolResponse {
    Listed {
        applications: Vec<ManagedApplication>,
        installations: Vec<ManagedInstallation>,
    },
    Resolved {
        explanation: ExplainStatus,
        /// Always true: preview queries are never retained for launch.
        preview_only: bool,
    },
    Launched {
        receipt: LaunchReceipt,
    },
    Installed {
        receipt: InstallReceipt,
    },
    Retried {
        receipt: InstallReceipt,
    },
    LauncherRemoved {
        entry_id: String,
    },
    VendorUninstalled {
        receipt: InstallReceipt,
    },
    DataDeleted {
        capsule_id: String,
    },
    Error {
        error: ProtocolError,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: String,
    pub message: String,
}

impl ProtocolError {
    #[must_use]
    pub fn unsupported_version(received: u16) -> Self {
        Self {
            code: "unsupported_version".to_string(),
            message: format!(
                "compatibility protocol {received} is unsupported; expected {PROTOCOL_VERSION}"
            ),
        }
    }

    #[must_use]
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }
}
