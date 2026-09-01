use compat_catalogue::ApplicationQuery;
use serde::{Deserialize, Serialize};

use crate::{
    ExplainStatus, InstallReceipt, InstallationStatus, LaunchReceipt, RouteKind, VmAvailability,
    VmInstallConfirmation, VmInstallation, VmUninstallation,
};

pub const PROTOCOL_VERSION: u16 = 4;

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
    /// A second explicit confirmation streams the same portal-selected file
    /// to the catalogued singleton Windows appliance. No path, command, or
    /// guest operation ID crosses the protocol.
    InstallInVm {
        version: u16,
        source_name: String,
    },
    /// Runs the separately confirmed Wine/Proton installation using the same
    /// portal descriptor and server-side digest binding.
    InstallInCapsule {
        version: u16,
        source_name: String,
    },
    /// Publish one candidate returned by the active managed installer. The
    /// client submits only the server-owned candidate entry ID.
    PublishVmCandidate {
        version: u16,
        entry_id: String,
    },
    /// Explicitly polls the singleton installer owned by this authenticated
    /// session. There is no client-provided guest operation handle.
    RefreshVmInstallation {
        version: u16,
    },
    RefreshVmUninstallation {
        version: u16,
    },
    RetryInstall {
        version: u16,
        capsule_id: String,
    },
    /// Escalate the exact digest-pinned installer already retained by a
    /// portal-managed capsule. No second file selection or client path is
    /// accepted; the server reopens and revalidates its own managed copy.
    EscalateInstallToVm {
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
    /// Guest publication ID only; the persistent guest catalogue selects the
    /// typed MSI removal record and never accepts an uninstaller command.
    VendorUninstallVm {
        version: u16,
        entry_id: String,
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
#[allow(
    clippy::struct_excessive_bools,
    reason = "the wire DTO keeps independent availability facts explicit rather than encoding policy in a state enum"
)]
pub struct ManagedApplication {
    pub id: String,
    pub app_id: String,
    pub display_name: String,
    pub publisher: String,
    /// Present only for Wine/Proton private capsules. A published guest app
    /// belongs to the shared Windows appliance and therefore has no capsule.
    pub capsule_id: Option<String>,
    pub program_id: String,
    pub explanation: ExplainStatus,
    pub vm_launch_available: bool,
    pub vm_unavailable_reason: Option<String>,
    pub audio_output_available: bool,
    pub audio_unavailable_reason: Option<String>,
    pub installation_status: Option<InstallationStatus>,
    pub remove_launcher_available: bool,
    pub vendor_uninstall_available: bool,
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
        /// Appliance readiness is global session state, not a property inferred
        /// from whichever application rows happen to be published.
        vm_availability: VmAvailability,
        /// The singleton installer operation, when one was started through
        /// the qOS portal. Candidate IDs remain opaque.
        vm_installation: Option<VmInstallation>,
        vm_uninstallation: Option<VmUninstallation>,
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
    VmInstallConfirmation {
        confirmation: VmInstallConfirmation,
    },
    VmInstallStarted {
        installation: VmInstallation,
    },
    VmCandidatePublished {
        installation: VmInstallation,
    },
    VmInstallationRefreshed {
        installation: Option<VmInstallation>,
    },
    VmUninstallationRefreshed {
        uninstallation: Option<VmUninstallation>,
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
    VmVendorUninstallation {
        uninstallation: VmUninstallation,
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
