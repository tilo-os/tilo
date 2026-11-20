//! Compatibility vocabulary shared by the UI and future `tilo-compatd` client.
use serde::{Deserialize, Serialize};
use ui::tilo_resource::{self, Model};

/// How an application is run.
#[derive(Model, Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    Wine,
    Proton,
    Vm,
}
impl Route {
    pub const ALL: [Self; 3] = [Self::Wine, Self::Proton, Self::Vm];
    /// What a program in a private capsule opens with: its files are a Wine prefix, which Wine
    /// and Proton both open, while the Windows appliance has programs of its own.
    pub const CAPSULE: [Self; 2] = [Self::Wine, Self::Proton];
    pub const fn short(self) -> &'static str {
        match self {
            Self::Wine => "WINE",
            Self::Proton => "PROTON",
            Self::Vm => "VM",
        }
    }
    /// Fluent key for a route-selection control.
    pub const fn button_key(self) -> &'static str {
        match self {
            Self::Wine => "windows-route-wine",
            Self::Proton => "windows-route-proton",
            Self::Vm => "windows-route-vm",
        }
    }
    /// Fluent key naming how an app on this route is kept apart from the rest of the
    /// computer.
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
    /// What the setup that installed it is called, as its installer named it.
    pub setup_name: Option<String>,
    pub catalogue_source: String,
    pub confidence: Confidence,
    pub reason: String,
    pub status: AppStatus,
    pub grants: Vec<Grant>,
    pub fallback_detail: String,
    pub launch_available: bool,
    pub launch_unavailable_reason: Option<String>,
    pub portal_file_open_available: bool,
    pub portal_file_open_unavailable_reason: Option<String>,
    pub route_override_available: bool,
    pub route_override_unavailable_reason: Option<String>,
    pub can_retry_install: bool,
    pub can_escalate_install_to_vm: bool,
    pub can_remove_launcher: bool,
    pub can_vendor_uninstall: bool,
    pub can_delete_data: bool,
    pub can_publish_vm_candidate: bool,
    pub installation_detail: Option<String>,
    pub vm_fallback_available: bool,
    pub vm_fallback_unavailable_reason: Option<String>,
}

impl WindowsApp {
    pub fn uses_override(&self) -> bool {
        self.route != self.recommended_route
    }
    /// The routes a person may choose between for it.
    pub fn route_choices(&self) -> &'static [Route] {
        if self.route == Route::Vm || self.recommended_route == Route::Vm {
            &Route::ALL
        } else {
            &Route::CAPSULE
        }
    }
    /// Its program is open in a private capsule, which the manager can end.
    pub fn stop_available(&self) -> bool {
        self.status == AppStatus::Running && self.route != Route::Vm
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplianceState {
    pub ready: bool,
    pub detail: String,
    pub running_apps: usize,
    /// This is management status, never client-supplied policy.
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

/// The capsule a catalogue app reports while its setup is still running, which keeps no data
/// of its own yet.
pub const PENDING_CAPSULE: &str = "Pending private capsule";

/// What one setup put on this computer: the private capsule every program of it shares, and
/// every program that goes when the capsule's data goes. They came from one installer and
/// keep one set of files, so the manager acts on them as a group.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setup {
    /// What the setup is called, as its installer named it; empty when the service reports
    /// no name.
    pub name: String,
    /// Every program of the setup, in the order the manager lists them.
    pub programs: Vec<String>,
}

impl Setup {
    /// Whether the setup holds more than one program: the point at which an operation on one
    /// program reaches the others.
    pub fn holds_several(&self) -> bool {
        self.programs.len() > 1
    }
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
    /// The setup `app` belongs to, when its data can be deleted now: its private capsule with
    /// every program that shares it. `None` for a program the Windows appliance shows, which
    /// keeps no capsule of its own, and while a setup is still changing.
    pub fn setup_of(&self, app: &WindowsApp) -> Option<Setup> {
        if app.route == Route::Vm || app.capsule == PENDING_CAPSULE {
            return None;
        }
        let programs: Vec<&WindowsApp> = self
            .apps
            .iter()
            .filter(|other| other.route != Route::Vm && other.capsule == app.capsule)
            .collect();
        if !programs.iter().any(|other| other.can_delete_data) {
            return None;
        }
        Some(Setup {
            name: programs
                .iter()
                .find_map(|other| other.setup_name.clone())
                .unwrap_or_default(),
            programs: programs.iter().map(|other| other.name.clone()).collect(),
        })
    }
}
