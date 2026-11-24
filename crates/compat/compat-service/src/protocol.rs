use compat_catalogue::ApplicationQuery;
use serde::{Deserialize, Serialize};

use crate::{
    ExplainStatus, InstallReceipt, InstallationStatus, LaunchReceipt, RouteKind, VmAvailability,
    VmInstallConfirmation, VmInstallation, VmPresentation, VmUninstallation,
};

// Version 10 adds `SetOpensWith`, and an application tells how it opens unless
// someone chooses otherwise and whether this manager may choose.
pub const PROTOCOL_VERSION: u16 = 10;

/// The code of a launch refused because a program of the same capsule is open with the other
/// runner: the answer names which program and which runner, and the manager keeps the refusal
/// only as long as that program stays open.
pub const OPEN_WITH_OTHER_RUNNER: &str = "open_with_other_runner";

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
    /// End every program of the entry this manager's workspace started that
    /// is still open.
    Stop {
        version: u16,
        entry_id: String,
    },
    /// Opens exactly one portal-selected document in a published Windows app.
    /// The descriptor accompanies this frame via `SCM_RIGHTS`; no path crosses.
    LaunchVmWithPortalFile {
        version: u16,
        entry_id: String,
    },
    Install {
        version: u16,
        source_name: String,
    },
    InstallInVm {
        version: u16,
        source_name: String,
    },
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
    /// Open a program of a private capsule with Wine or Proton from now on, or
    /// as its capsule opens (`None`). Only an administrator of the computer
    /// may; the server checks, never the client.
    SetOpensWith {
        version: u16,
        entry_id: String,
        route: Option<RouteKind>,
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
    /// Presentation capability for the read-only portal-file action. This is
    /// explicit because app-focused SPICE has no portal file bridge.
    pub presentation: Option<VmPresentation>,
    pub portal_file_open_available: bool,
    pub portal_file_open_unavailable_reason: Option<String>,
    pub audio_output_available: bool,
    pub audio_unavailable_reason: Option<String>,
    pub installation_status: Option<InstallationStatus>,
    pub remove_launcher_available: bool,
    pub vendor_uninstall_available: bool,
    /// A program of this entry started in the manager's workspace is still open.
    pub running: bool,
    /// How it opens unless someone chooses otherwise; `explanation.route` is
    /// how it opens now.
    pub recommended_route: RouteKind,
    /// Whether this manager may choose how it opens, and why not when not.
    pub opens_with_choice_available: bool,
    pub opens_with_unavailable_reason: Option<String>,
}

/// A program of the manager's workspace that ended, reported once to each
/// manager connected when it ended or connecting later.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProgramEnd {
    pub entry_id: String,
    pub display_name: String,
    pub success: bool,
    pub exit_code: Option<i32>,
    /// Why it never ran, when it did not.
    pub error: Option<String>,
    /// The last of what it wrote on its error stream, when it stopped in failure and wrote
    /// anything: a line or two, bounded.
    pub said: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_file_launch_has_no_client_controlled_writeback_bit() {
        let request = ProtocolRequest::LaunchVmWithPortalFile {
            version: PROTOCOL_VERSION,
            entry_id: "windows-entry".to_string(),
        };
        let encoded = serde_json::to_string(&request).unwrap();
        assert!(!encoded.contains("writable"));
        assert!(
            serde_json::from_str::<ProtocolRequest>(
                r#"{"method":"launch_vm_with_portal_file","version":10,"entry_id":"windows-entry","writable":true}"#,
            )
            .is_err()
        );
    }
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

/// On the wire, `result` names the variant beside its fields
/// ([`tilo_actions::tagged_through_json_value!`]).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(remote = "Self", rename_all = "snake_case")]
pub enum ProtocolResponse {
    Listed {
        applications: Vec<ManagedApplication>,
        installations: Vec<ManagedInstallation>,
        /// Appliance readiness is global session state, not a property inferred
        /// from whichever application rows happen to be published.
        vm_availability: VmAvailability,
        vm_installation: Option<VmInstallation>,
        vm_uninstallation: Option<VmUninstallation>,
        /// The workspace's programs that ended since this connection last
        /// listed, oldest first.
        ended: Vec<ProgramEnd>,
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
    Stopped {
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
    OpensWithSet {
        entry_id: String,
        route: Option<RouteKind>,
    },
    Error {
        error: ProtocolError,
    },
}

tilo_actions::tagged_through_json_value!(ProtocolResponse, "result");

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
