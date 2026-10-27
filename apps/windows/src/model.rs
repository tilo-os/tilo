//! Compatibility vocabulary shared by the UI and future `tilo-compatd` client.
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Model};

/// How an application is run.
#[derive(Model, Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Route {
    Wine,
    Proton,
    Vm,
}
impl Route {
    pub const ALL: [Self; 3] = [Self::Wine, Self::Proton, Self::Vm];
    pub const fn short(self) -> &'static str {
        match self {
            Self::Wine => "WINE",
            Self::Proton => "PROTON",
            Self::Vm => "VM",
        }
    }
    /// Fluent key for a route-selection control. Model consumers that display
    /// a route resolve this key through their own localized surface.
    pub const fn button_key(self) -> &'static str {
        match self {
            Self::Wine => "windows-route-wine",
            Self::Proton => "windows-route-proton",
            Self::Vm => "windows-route-vm",
        }
    }
    /// Fluent key naming how an app on this route is kept apart from the
    /// rest of the computer.
    pub const fn security_key(self) -> &'static str {
        match self {
            Self::Wine | Self::Proton => "windows-route-capsule-security",
            Self::Vm => "windows-route-vm-security",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    Verified,
    Community,
    Heuristic,
    Fallback,
}
impl Confidence {
    /// Fluent key for how well this computer's catalogue knows the app runs.
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Verified => "windows-confidence-verified",
            Self::Community => "windows-confidence-community",
            Self::Heuristic => "windows-confidence-best-match",
            Self::Fallback => "windows-confidence-untested",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AppStatus {
    Ready,
    Running,
    NeedsAttention,
    Installing,
    Uninstalling,
    Failed,
    Uninstalled,
}
impl AppStatus {
    /// Fluent key for where an app's installation stands.
    pub const fn label_key(self) -> &'static str {
        match self {
            Self::Ready => "windows-app-status-ready",
            Self::Running => "windows-app-status-running",
            Self::NeedsAttention => "windows-app-status-needs-attention",
            Self::Installing => "windows-app-status-installing",
            Self::Uninstalling => "windows-app-status-uninstalling",
            Self::Failed => "windows-app-status-failed",
            Self::Uninstalled => "windows-app-status-uninstalled",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Grant {
    pub label: String,
    pub detail: String,
    pub enabled: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "this read-only view model mirrors independent server-authorized UI actions"
)]
pub struct WindowsApp {
    pub id: String,
    pub name: String,
    pub publisher: String,
    pub route: Route,
    pub recommended_route: Route,
    pub runner: String,
    pub capsule: String,
    pub catalogue_source: String,
    pub confidence: Confidence,
    pub reason: String,
    pub status: AppStatus,
    pub grants: Vec<Grant>,
    pub fallback_detail: String,
    pub launch_available: bool,
    pub launch_unavailable_reason: Option<String>,
    /// Explicit presentation capability for opening a portal-selected file.
    /// A Windows VM route alone is insufficient: focused SPICE has no file
    /// bridge, while native RAIL does.
    pub portal_file_open_available: bool,
    pub portal_file_open_unavailable_reason: Option<String>,
    pub route_override_available: bool,
    pub route_override_unavailable_reason: Option<String>,
    pub can_retry_install: bool,
    /// The retained, portal-managed installer can be explicitly replayed in
    /// the one Windows appliance. This is only offered for an installation
    /// state the service says can safely leave its private capsule.
    pub can_escalate_install_to_vm: bool,
    pub can_remove_launcher: bool,
    pub can_vendor_uninstall: bool,
    pub can_delete_data: bool,
    /// This row is a discovered, not-yet-published application from the
    /// managed installer operation. Its `id` remains server-owned.
    pub can_publish_vm_candidate: bool,
    pub installation_detail: Option<String>,
    pub vm_fallback_available: bool,
    pub vm_fallback_unavailable_reason: Option<String>,
}

impl WindowsApp {
    pub fn uses_override(&self) -> bool {
        self.route != self.recommended_route
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplianceState {
    pub ready: bool,
    pub detail: String,
    pub running_apps: usize,
    /// Server-derived RDP/RAIL redirect union for the singleton Windows user
    /// session. This is management status, never client-supplied policy.
    pub shared_session_active: bool,
    pub shared_session_grants: Vec<String>,
    pub shared_session_contributors: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Catalogue {
    pub apps: Vec<WindowsApp>,
    pub vm: ApplianceState,
    pub vm_install_confirmation: Option<VmInstallConfirmation>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VmInstallConfirmation {
    pub display_name: String,
    pub detail: String,
    pub recommended_route: Route,
}

impl Catalogue {
    pub fn app(&self, id: &str) -> Option<&WindowsApp> {
        self.apps.iter().find(|app| app.id == id)
    }
    pub fn app_mut(&mut self, id: &str) -> Option<&mut WindowsApp> {
        self.apps.iter_mut().find(|app| app.id == id)
    }
}
