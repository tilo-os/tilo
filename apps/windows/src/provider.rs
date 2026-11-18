//! Provider boundary for the session compatibility service.
use std::fs::File;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::time::Duration;

use compat_service::{
    InstallationStatus, ManagedApplication, ManagedInstallation, MatchReason, PROTOCOL_VERSION,
    ProgramEnd, ProtocolRequest, ProtocolResponse, RouteKind, SessionClient, SessionError,
    VmAvailability, VmInstallation, VmUninstallation,
};

use crate::model::{
    AppStatus, ApplianceState, Catalogue, Confidence, Grant, Route, VmInstallConfirmation,
    WindowsApp,
};
use crate::strings::{self, format_all};

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

pub trait CompatibilityProvider: Send {
    /// Whether the session's compatibility service answers.
    fn connected(&self) -> bool {
        true
    }
    /// Live UI controls emit inert resource/action addresses so the host can re-authorize,
    /// resource, and dispatch them.
    fn action_native_controls(&self) -> bool {
        false
    }
    /// Whether a successful method return is also the terminal outcome.
    fn operations_complete_on_return(&self) -> bool {
        false
    }
    fn catalogue(&mut self) -> Catalogue;
    /// Queue a local launch of a server-owned managed entry.
    fn launch(&mut self, app_id: &str) -> Result<(), String>;
    /// Queue the separately authorized VM fallback action.
    fn launch_vm_fallback(&mut self, app_id: &str) -> Result<(), String>;
    /// Open one portal-selected document in a published VM application.
    fn launch_vm_with_file(&mut self, _app_id: &str, _source: File) -> Result<(), String> {
        Err("Opening portal files is unavailable for this Windows route.".to_string())
    }
    /// Transfer a portal-open installer descriptor to the service.
    fn install(&mut self, source: File, source_name: &str) -> Result<(), String>;
    fn confirm_capsule_install(&mut self) -> Result<(), String>;
    fn confirm_vm_install(&mut self) -> Result<(), String>;
    fn publish_vm_candidate(&mut self, entry_id: &str) -> Result<(), String>;
    fn vendor_uninstall_vm(&mut self, entry_id: &str) -> Result<(), String>;
    /// Retry setup from the retained digest-pinned installer.
    fn retry_install(&mut self, capsule_id: &str) -> Result<(), String>;
    /// Explicitly replays a retained, digest-pinned portal installer in the one Windows
    /// appliance.
    ///
    /// # Errors
    ///
    /// Returns an error when the retained setup is in a state that cannot be escalated, or
    /// the appliance is unavailable.
    fn escalate_install_to_vm(&mut self, capsule_id: &str) -> Result<(), String>;
    /// Remove one launcher while retaining its installation.
    fn remove_launcher(&mut self, app_id: &str) -> Result<(), String>;
    /// End the running program of a private capsule app.
    fn stop(&mut self, app_id: &str) -> Result<(), String>;
    /// Run the discovered vendor uninstaller inside its capsule.
    fn vendor_uninstall(&mut self, capsule_id: &str) -> Result<(), String>;
    /// Delete a managed installation and its private capsule data.
    fn delete_data(&mut self, capsule_id: &str) -> Result<(), String>;
    fn take_notice(&mut self) -> Option<String> {
        None
    }
    /// Whether the notice [`Self::take_notice`] gave is about something that went wrong.
    fn notice_is_error(&self) -> bool {
        false
    }
    fn take_operation_result(&mut self) -> Option<Result<String, String>> {
        None
    }
    /// Stage what a capture's `scenario` shows in the sample catalogue: a program that ended, a
    /// program of the appliance, a name longer than a row holds. The service behind a window
    /// stages nothing.
    fn stage(&mut self, _scenario: &str) {}
    fn set_route_override(&mut self, app_id: &str, route: Route) -> Result<(), String>;
    /// Removes a user route override.
    fn reset_route_override(&mut self, app_id: &str) -> Result<(), String>;
}

/// Safe live fallback until the compatibility service endpoint is available. A
/// control-plane UI must never make fixture software look installed.
pub struct EmptyProvider {
    detail: String,
}

impl EmptyProvider {
    #[must_use]
    pub fn with_detail(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

impl Default for EmptyProvider {
    fn default() -> Self {
        Self::with_detail("The Tilo compatibility service is unavailable in this session.")
    }
}

impl CompatibilityProvider for EmptyProvider {
    fn connected(&self) -> bool {
        false
    }

    fn catalogue(&mut self) -> Catalogue {
        Catalogue {
            apps: Vec::new(),
            vm: ApplianceState {
                ready: false,
                detail: self.detail.clone(),
                running_apps: 0,
                shared_session_active: false,
                shared_session_grants: Vec::new(),
                shared_session_contributors: Vec::new(),
            },
            vm_install_confirmation: None,
        }
    }

    fn launch(&mut self, _app_id: &str) -> Result<(), String> {
        Err(self.detail.clone())
    }

    fn launch_vm_fallback(&mut self, _app_id: &str) -> Result<(), String> {
        Err(self.detail.clone())
    }

    fn install(&mut self, _source: File, _source_name: &str) -> Result<(), String> {
        Err(self.detail.clone())
    }

    fn confirm_capsule_install(&mut self) -> Result<(), String> {
        Err(self.detail.clone())
    }

    fn confirm_vm_install(&mut self) -> Result<(), String> {
        Err(self.detail.clone())
    }

    fn publish_vm_candidate(&mut self, _entry_id: &str) -> Result<(), String> {
        Err(self.detail.clone())
    }

    fn vendor_uninstall_vm(&mut self, _entry_id: &str) -> Result<(), String> {
        Err(self.detail.clone())
    }

    fn retry_install(&mut self, _capsule_id: &str) -> Result<(), String> {
        Err(self.detail.clone())
    }

    fn escalate_install_to_vm(&mut self, _capsule_id: &str) -> Result<(), String> {
        Err(self.detail.clone())
    }

    fn stop(&mut self, _app_id: &str) -> Result<(), String> {
        Err(self.detail.clone())
    }

    fn remove_launcher(&mut self, _app_id: &str) -> Result<(), String> {
        Err(self.detail.clone())
    }

    fn vendor_uninstall(&mut self, _capsule_id: &str) -> Result<(), String> {
        Err(self.detail.clone())
    }

    fn delete_data(&mut self, _capsule_id: &str) -> Result<(), String> {
        Err(self.detail.clone())
    }

    fn set_route_override(&mut self, _app_id: &str, _route: Route) -> Result<(), String> {
        Err("The Tilo compatibility service is not connected.".into())
    }

    fn reset_route_override(&mut self, _app_id: &str) -> Result<(), String> {
        Err("The Tilo compatibility service is not connected.".into())
    }
}

/// Predictable fixture. It deliberately looks like the service response, not a mock of
/// Wine: code depending on it cannot accidentally gain host execution.
#[derive(Clone)]
pub struct SampleProvider {
    catalogue: Catalogue,
}
impl SampleProvider {
    pub fn new() -> Self {
        let grants = |items: &[(&str, &str, bool)]| {
            items
                .iter()
                .map(|(label, detail, enabled)| Grant {
                    label: (*label).to_string(),
                    detail: (*detail).to_string(),
                    enabled: *enabled,
                })
                .collect()
        };
        Self { catalogue: Catalogue {
            apps: vec![
                WindowsApp { id: "adobe-reader".into(), name: "Adobe Acrobat Reader".into(), publisher: "Adobe".into(), route: Route::Wine, recommended_route: Route::Wine, runner: "Wine 11.0".into(), capsule: "Adobe Reader".into(), catalogue_source: "Tilo compatibility catalogue".into(), confidence: Confidence::Verified, reason: "A verified recipe runs Reader in Wine, the lightest way to open it in isolation.".into(), status: AppStatus::Ready, grants: grants(&[("Downloads", "Read and write", true), ("Clipboard", "On demand", true), ("Camera", "Off", false)]), fallback_detail: "Reader sees only the folders and devices listed above.".into(), launch_available: true, launch_unavailable_reason: None, portal_file_open_available: false, portal_file_open_unavailable_reason: Some("Open file is available only for apps in the Windows appliance.".into()), route_override_available: true, route_override_unavailable_reason: None, can_retry_install: false, can_escalate_install_to_vm: false, can_remove_launcher: true, can_vendor_uninstall: true, can_delete_data: true, can_publish_vm_candidate: false, installation_detail: Some("Installed 12 June 2026".into()), vm_fallback_available: false, vm_fallback_unavailable_reason: Some("Open in Windows VM is available only for apps in the Windows appliance.".into()) },
                WindowsApp { id: "cyberpunk-2077".into(), name: "Cyberpunk 2077".into(), publisher: "CD PROJEKT RED".into(), route: Route::Proton, recommended_route: Route::Proton, runner: "Proton 10.0".into(), capsule: "Steam library".into(), catalogue_source: "Steam + ProtonDB recipe".into(), confidence: Confidence::Community, reason: "Games run best in Proton with the Steam Runtime, which includes the fixes this game is known to need.".into(), status: AppStatus::Running, grants: grants(&[("Games library", "Read and write", true), ("Controller", "Connected devices", true), ("Microphone", "Off", false)]), fallback_detail: "The game sees only its library and connected controllers.".into(), launch_available: true, launch_unavailable_reason: None, portal_file_open_available: false, portal_file_open_unavailable_reason: Some("Open file is available only for apps in the Windows appliance.".into()), route_override_available: true, route_override_unavailable_reason: None, can_retry_install: false, can_escalate_install_to_vm: false, can_remove_launcher: true, can_vendor_uninstall: true, can_delete_data: true, can_publish_vm_candidate: false, installation_detail: Some("Installed 3 May 2026".into()), vm_fallback_available: false, vm_fallback_unavailable_reason: Some("Open in Windows VM is available only for apps in the Windows appliance.".into()) },
                WindowsApp { id: "visual-studio".into(), name: "Visual Studio".into(), publisher: "Microsoft".into(), route: Route::Vm, recommended_route: Route::Vm, runner: "Windows 11".into(), capsule: "Shared Windows appliance".into(), catalogue_source: "Tilo compatibility catalogue".into(), confidence: Confidence::Verified, reason: "Visual Studio needs Windows services that Wine can't provide, so it runs in the Windows appliance.".into(), status: AppStatus::Ready, grants: grants(&[("Audio playback", "On", true), ("Files on this computer", "Off", false), ("Clipboard", "Off", false), ("Microphone and network", "Off", false)]), fallback_detail: "Files reach Visual Studio only when you open them with it.".into(), launch_available: true, launch_unavailable_reason: None, portal_file_open_available: false, portal_file_open_unavailable_reason: Some("This app window can't receive files from this computer yet.".into()), route_override_available: false, route_override_unavailable_reason: Some("Only an administrator can change how this app opens.".into()), can_retry_install: false, can_escalate_install_to_vm: false, can_remove_launcher: true, can_vendor_uninstall: true, can_delete_data: false, can_publish_vm_candidate: false, installation_detail: Some("Installed 21 April 2026".into()), vm_fallback_available: true, vm_fallback_unavailable_reason: None },
            ], vm: ApplianceState { ready: true, detail: "The Windows appliance is ready.".into(), running_apps: 0, shared_session_active: false, shared_session_grants: Vec::new(), shared_session_contributors: Vec::new() }, vm_install_confirmation: None } }
    }

    /// An inspected installer waiting for the person to choose where it goes.
    pub(crate) fn seed_install_confirmation(&mut self) {
        self.catalogue.vm_install_confirmation = Some(crate::model::VmInstallConfirmation {
            display_name: "Notepad++ 8.6.4".into(),
            detail:
                "A signed installer from Don Ho. The Tilo catalogue has a verified recipe for it."
                    .into(),
            recommended_route: Route::Wine,
        });
    }

    pub(crate) fn seed_setup_failure(&mut self) {
        if let Some(first) = self.catalogue.apps.first_mut() {
            first.status = AppStatus::Failed;
            first.launch_available = false;
            first.launch_unavailable_reason =
                Some("Setup has to finish before Reader can open.".into());
            first.can_retry_install = true;
            first.can_escalate_install_to_vm = true;
            first.installation_detail = Some(
                "The installer stopped before Reader was ready. Its data is kept, so you can retry or continue in the Windows appliance."
                    .into(),
            );
        }
    }
}
impl Default for SampleProvider {
    fn default() -> Self {
        Self::new()
    }
}
impl CompatibilityProvider for SampleProvider {
    fn operations_complete_on_return(&self) -> bool {
        true
    }
    fn catalogue(&mut self) -> Catalogue {
        self.catalogue.clone()
    }
    fn launch(&mut self, app_id: &str) -> Result<(), String> {
        let app = self
            .catalogue
            .app_mut(app_id)
            .ok_or_else(|| "Unknown Windows app".to_string())?;
        app.status = AppStatus::Running;
        Ok(())
    }
    fn launch_vm_fallback(&mut self, app_id: &str) -> Result<(), String> {
        self.launch(app_id)
    }
    fn install(&mut self, _source: File, source_name: &str) -> Result<(), String> {
        let name = source_name
            .strip_suffix(".exe")
            .or_else(|| source_name.strip_suffix(".msi"))
            .unwrap_or(source_name);
        self.catalogue.apps.push(WindowsApp {
            id: "imported-sample".into(),
            name: name.into(),
            publisher: "Imported application".into(),
            route: Route::Wine,
            recommended_route: Route::Wine,
            runner: "Wine 11.0".into(),
            capsule: "imported-sample".into(),

            catalogue_source: "Default settings".into(),
            confidence: Confidence::Heuristic,
            reason: "Tilo is checking the installer and running it in a private capsule.".into(),
            status: AppStatus::Installing,
            grants: Vec::new(),
            fallback_detail: "Setup can't reach anything else on this computer.".into(),
            launch_available: false,
            launch_unavailable_reason: Some("Setup is still running.".into()),
            portal_file_open_available: false,
            portal_file_open_unavailable_reason: Some("Setup is still running.".into()),
            route_override_available: false,
            route_override_unavailable_reason: None,
            can_retry_install: false,
            can_escalate_install_to_vm: false,
            can_remove_launcher: false,
            can_vendor_uninstall: false,
            can_delete_data: false,
            can_publish_vm_candidate: false,
            installation_detail: Some("Installing…".into()),
            vm_fallback_available: false,
            vm_fallback_unavailable_reason: Some("Setup is still running.".into()),
        });
        Ok(())
    }
    fn confirm_capsule_install(&mut self) -> Result<(), String> {
        Err("Choose an installer first.".into())
    }
    fn confirm_vm_install(&mut self) -> Result<(), String> {
        Err("Choose an installer first.".into())
    }
    fn publish_vm_candidate(&mut self, _entry_id: &str) -> Result<(), String> {
        Err("No managed Windows installation is awaiting publication.".into())
    }
    fn vendor_uninstall_vm(&mut self, app_id: &str) -> Result<(), String> {
        self.catalogue.apps.retain(|app| app.id != app_id);
        Ok(())
    }
    fn retry_install(&mut self, capsule_id: &str) -> Result<(), String> {
        let app = self
            .catalogue
            .apps
            .iter_mut()
            .find(|app| app.capsule == capsule_id)
            .ok_or_else(|| "Unknown Windows app".to_string())?;
        app.status = AppStatus::Installing;
        Ok(())
    }
    fn escalate_install_to_vm(&mut self, capsule_id: &str) -> Result<(), String> {
        let app = self
            .catalogue
            .apps
            .iter_mut()
            .find(|app| app.capsule == capsule_id)
            .ok_or_else(|| "Unknown Windows app".to_string())?;
        app.status = AppStatus::Installing;
        app.fallback_detail = "The installer is opening in the Windows appliance.".into();
        app.can_escalate_install_to_vm = false;
        Ok(())
    }
    fn stop(&mut self, app_id: &str) -> Result<(), String> {
        let app = self
            .catalogue
            .app_mut(app_id)
            .ok_or_else(|| "Unknown Windows app".to_string())?;
        app.status = AppStatus::Ready;
        Ok(())
    }
    fn stage(&mut self, scenario: &str) {
        match scenario {
            "closed" | "ended-with-error" | "ended-overview" | "could-not-open" => {
                if let Some(app) = self.catalogue.app_mut("cyberpunk-2077") {
                    app.status = AppStatus::Ready;
                }
            }
            "vm-running" => {
                if let Some(app) = self.catalogue.app_mut("visual-studio") {
                    app.status = AppStatus::Running;
                }
            }
            "long-name" => {
                if let Some(app) = self.catalogue.app_mut("adobe-reader") {
                    app.name = "Adobe Acrobat Reader (continuous release) 64-bit".into();
                }
            }
            _ => {}
        }
    }

    fn remove_launcher(&mut self, app_id: &str) -> Result<(), String> {
        self.catalogue.apps.retain(|app| app.id != app_id);
        Ok(())
    }
    fn vendor_uninstall(&mut self, capsule_id: &str) -> Result<(), String> {
        let app = self
            .catalogue
            .apps
            .iter_mut()
            .find(|app| app.capsule == capsule_id)
            .ok_or_else(|| "Unknown Windows app".to_string())?;
        app.status = AppStatus::Uninstalled;
        app.launch_available = false;
        Ok(())
    }
    fn delete_data(&mut self, capsule_id: &str) -> Result<(), String> {
        self.catalogue.apps.retain(|app| app.capsule != capsule_id);
        Ok(())
    }
    fn set_route_override(&mut self, app_id: &str, route: Route) -> Result<(), String> {
        let app = self
            .catalogue
            .app_mut(app_id)
            .ok_or_else(|| "Unknown Windows app".to_string())?;
        app.route = route;
        app.runner = match route {
            Route::Wine => "Wine 11.0",
            Route::Proton => "Proton 10.0",
            Route::Vm => "Windows 11",
        }
        .into();
        Ok(())
    }
    fn reset_route_override(&mut self, app_id: &str) -> Result<(), String> {
        let app = self
            .catalogue
            .app_mut(app_id)
            .ok_or_else(|| "Unknown Windows app".to_string())?;
        app.route = app.recommended_route;
        Ok(())
    }
}

enum WorkerCommand {
    Refresh,
    Launch(String),
    LaunchVmFallback(String),
    LaunchVmWithFile { entry_id: String, source: File },
    Stop(String),
    Install { source: File, source_name: String },
    InstallInCapsule { source: File, source_name: String },
    InstallInVm { source: File, source_name: String },
    PublishVmCandidate(String),
    RetryInstall(String),
    EscalateInstallToVm(String),
    RemoveLauncher(String),
    VendorUninstall(String),
    VendorUninstallVm(String),
    DeleteData(String),
}

/// A list the service answered.
#[derive(Clone, PartialEq)]
struct Listed {
    applications: Vec<ManagedApplication>,
    installations: Vec<ManagedInstallation>,
    vm_availability: VmAvailability,
    vm_installation: Option<VmInstallation>,
    vm_uninstallation: Option<VmUninstallation>,
    ended: Vec<ProgramEnd>,
}

enum WorkerEvent {
    Listed(Box<Listed>),
    VmInstallConfirmation {
        source: File,
        source_name: String,
        confirmation: compat_service::VmInstallConfirmation,
    },
    Unavailable(String),
    Notice(String),
    Operation(Result<String, String>),
}

pub struct LiveProvider {
    cached: Catalogue,
    commands: Sender<WorkerCommand>,
    events: Receiver<WorkerEvent>,
    notice: Option<String>,
    /// Whether the notice the last list brought is about something that went wrong.
    notice_error: bool,
    /// The entry the person ended here: its end is what they asked for, and needs no sentence.
    stopping: Option<String>,
    operation_result: Option<Result<String, String>>,
    pending_vm_install: Option<(File, String)>,
    pending_vm_install_confirmation: Option<VmInstallConfirmation>,
}

impl LiveProvider {
    /// Connect to the launcher-inherited descriptor and move all blocking I/O onto a
    /// dedicated worker thread.
    ///
    /// # Errors
    ///
    /// Returns an error when no verified descriptor was inherited or the bounded worker
    /// thread cannot be started.
    pub fn connect(wake: ui::WakeHandle) -> Result<Self, String> {
        // SAFETY: the app creates exactly one live provider, which becomes the
        // inherited descriptor's sole owner and transfers it immediately to
        // `SessionClient`.
        let owned = unsafe { runtime_portal::take_service_fd("compat") }
            .ok_or_else(|| "The Tilo compatibility service is unavailable.".to_string())?;
        Self::over(SessionClient::from_owned_fd(owned), wake)
    }

    /// Talk to the service over `client` on a worker thread, which wakes the window with `wake`
    /// whenever it has news: an answer, or a list that changed since the last one.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker thread cannot be started.
    pub(crate) fn over(client: SessionClient, wake: ui::WakeHandle) -> Result<Self, String> {
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("tilo-windows-service".to_string())
            .spawn(move || {
                worker(
                    client,
                    &command_rx,
                    &Events {
                        sender: event_tx,
                        wake,
                    },
                );
            })
            .map_err(|error| format!("Could not start compatibility worker: {error}"))?;
        command_tx
            .send(WorkerCommand::Refresh)
            .map_err(|_| "The compatibility worker stopped before startup.".to_string())?;
        Ok(Self {
            cached: unavailable_catalogue("Connecting to the Tilo compatibility service."),
            commands: command_tx,
            events: event_rx,
            notice: None,
            notice_error: false,
            stopping: None,
            operation_result: None,
            pending_vm_install: None,
            pending_vm_install_confirmation: None,
        })
    }

    fn drain(&mut self) {
        loop {
            let event = match self.events.try_recv() {
                Ok(event) => event,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.cached = unavailable_catalogue("The compatibility service disconnected.");
                    return;
                }
            };
            match event {
                WorkerEvent::Listed(listed) => {
                    let Listed {
                        applications,
                        installations,
                        vm_availability,
                        vm_installation,
                        vm_uninstallation,
                        ended,
                    } = *listed;
                    // The person's own Quit is shown by the app no longer being open, not by a
                    // sentence about what they just did.
                    let asked = self
                        .stopping
                        .as_deref()
                        .and_then(|stopping| ended.iter().find(|end| end.entry_id == stopping));
                    if asked.is_some() {
                        self.stopping = None;
                    }
                    if let Some(end) = ended
                        .iter()
                        .rev()
                        .find(|end| self.stopping.as_deref() != Some(end.entry_id.as_str()))
                        && let Some(notice) = ended_notice(end)
                    {
                        self.notice = Some(notice);
                        self.notice_error = true;
                    }
                    self.cached = catalogue_from_managed(
                        applications,
                        &installations,
                        vm_availability,
                        vm_installation,
                        vm_uninstallation.as_ref(),
                    );
                    self.cached.vm_install_confirmation =
                        self.pending_vm_install_confirmation.clone();
                }
                WorkerEvent::Unavailable(detail) => {
                    // A malformed, mismatched, or disconnected response never leaves stale
                    // installed applications painted as live.
                    self.cached = unavailable_catalogue(&detail);
                    self.notice = Some(detail);
                    self.notice_error = false;
                }
                WorkerEvent::Notice(detail) => {
                    self.notice = Some(detail);
                    self.notice_error = false;
                }
                WorkerEvent::Operation(result) => {
                    let said = match &result {
                        Ok(detail) | Err(detail) => detail.clone(),
                    };
                    // An operation that says nothing is one the window already shows: a program
                    // the person ended is no longer open, which is the whole answer, and the line
                    // that said it was still being ended goes with it.
                    if said.is_empty() {
                        self.notice = None;
                        self.notice_error = false;
                    } else {
                        self.notice = Some(said);
                        self.notice_error = result.is_err();
                    }
                    self.operation_result = Some(result);
                }
                WorkerEvent::VmInstallConfirmation {
                    source,
                    source_name,
                    confirmation,
                } => {
                    self.pending_vm_install = Some((source, source_name));
                    self.notice = Some(confirmation.detail.clone());
                    self.notice_error = false;
                    // The installer was taken and waits for the person to choose where it
                    // installs: that answers the install, and the choice is an operation of its
                    // own. Left unanswered, the install held the window busy and refused the
                    // choice the person then approved.
                    self.operation_result = Some(Ok(confirmation.detail.clone()));
                    let confirmation = VmInstallConfirmation {
                        display_name: confirmation.display_name,
                        detail: confirmation.detail,
                        recommended_route: route_from_kind(confirmation.recommended_route),
                    };
                    self.pending_vm_install_confirmation = Some(confirmation.clone());
                    self.cached.vm_install_confirmation = Some(confirmation);
                }
            }
        }
    }

    fn send(&self, command: WorkerCommand) -> Result<(), String> {
        self.commands
            .send(command)
            .map_err(|_| "The compatibility service worker has stopped.".to_string())
    }

    /// The current entry for `app_id`, after taking in the worker's news.
    fn managed_app(&mut self, app_id: &str) -> Result<&WindowsApp, String> {
        self.drain();
        self.cached
            .app(app_id)
            .ok_or_else(|| "That managed Windows application is not in this session.".to_string())
    }

    /// The installer waiting for a destination, which the choice consumes.
    fn take_pending_install(&mut self) -> Result<(File, String), String> {
        let pending = self
            .pending_vm_install
            .take()
            .ok_or_else(|| "Choose an installer before confirming a destination.".to_string())?;
        self.cached.vm_install_confirmation = None;
        self.pending_vm_install_confirmation = None;
        Ok(pending)
    }
}

impl CompatibilityProvider for LiveProvider {
    fn action_native_controls(&self) -> bool {
        true
    }
    fn catalogue(&mut self) -> Catalogue {
        self.drain();
        self.cached.clone()
    }

    fn launch(&mut self, app_id: &str) -> Result<(), String> {
        let application = self.managed_app(app_id)?;
        if !application.launch_available {
            return Err(application
                .launch_unavailable_reason
                .clone()
                .unwrap_or_else(|| "This launch route is unavailable.".to_string()));
        }
        self.send(WorkerCommand::Launch(app_id.to_string()))
    }

    fn launch_vm_fallback(&mut self, app_id: &str) -> Result<(), String> {
        let application = self.managed_app(app_id)?;
        if !application.vm_fallback_available {
            return Err(application
                .vm_fallback_unavailable_reason
                .clone()
                .unwrap_or_else(|| "The Windows fallback is unavailable.".to_string()));
        }
        self.send(WorkerCommand::LaunchVmFallback(app_id.to_string()))
    }

    fn launch_vm_with_file(&mut self, app_id: &str, source: File) -> Result<(), String> {
        let application = self.managed_app(app_id)?;
        if application.route != Route::Vm {
            return Err(
                "Open file is available only for a published Windows VM application.".to_string(),
            );
        }
        self.send(WorkerCommand::LaunchVmWithFile {
            entry_id: app_id.to_string(),
            source,
        })
    }

    fn install(&mut self, source: File, source_name: &str) -> Result<(), String> {
        let name = source_name
            .strip_suffix(".exe")
            .or_else(|| source_name.strip_suffix(".msi"))
            .unwrap_or(source_name);
        self.cached.apps.push(WindowsApp {
            id: format!("installing-{}", self.cached.apps.len()),
            name: name.replace(['_', '-'], " "),
            publisher: "Imported application".into(),
            route: Route::Wine,
            recommended_route: Route::Wine,
            runner: "Choosing…".into(),
            capsule: "Pending private capsule".into(),

            catalogue_source: "Checking the installer…".into(),
            confidence: Confidence::Heuristic,
            reason: "Tilo is checking the installer and choosing how to run it.".into(),
            status: AppStatus::Installing,
            grants: Vec::new(),
            fallback_detail: "Setup can't reach anything else on this computer.".into(),
            launch_available: false,
            launch_unavailable_reason: Some("Setup is still running.".into()),
            portal_file_open_available: false,
            portal_file_open_unavailable_reason: Some("Setup is still running.".into()),
            route_override_available: false,
            route_override_unavailable_reason: None,
            can_retry_install: false,
            can_escalate_install_to_vm: false,
            can_remove_launcher: false,
            can_vendor_uninstall: false,
            can_delete_data: false,
            can_publish_vm_candidate: false,
            installation_detail: Some("Installing…".into()),
            vm_fallback_available: false,
            vm_fallback_unavailable_reason: Some("Setup is still running.".into()),
        });
        self.send(WorkerCommand::Install {
            source,
            source_name: source_name.to_string(),
        })
    }

    fn confirm_capsule_install(&mut self) -> Result<(), String> {
        let (source, source_name) = self.take_pending_install()?;
        self.send(WorkerCommand::InstallInCapsule {
            source,
            source_name,
        })
    }

    fn confirm_vm_install(&mut self) -> Result<(), String> {
        let (source, source_name) = self.take_pending_install()?;
        self.send(WorkerCommand::InstallInVm {
            source,
            source_name,
        })
    }

    fn publish_vm_candidate(&mut self, entry_id: &str) -> Result<(), String> {
        self.send(WorkerCommand::PublishVmCandidate(entry_id.to_string()))
    }

    fn vendor_uninstall_vm(&mut self, entry_id: &str) -> Result<(), String> {
        self.send(WorkerCommand::VendorUninstallVm(entry_id.to_string()))
    }

    fn retry_install(&mut self, capsule_id: &str) -> Result<(), String> {
        self.send(WorkerCommand::RetryInstall(capsule_id.to_string()))
    }

    fn escalate_install_to_vm(&mut self, capsule_id: &str) -> Result<(), String> {
        self.drain();
        let installation = self
            .cached
            .apps
            .iter()
            .find(|application| application.capsule == capsule_id)
            .ok_or_else(|| "That retained Windows setup is not in this session.".to_string())?;
        if !installation.can_escalate_install_to_vm {
            return Err(
                "This retained setup cannot be continued in the Windows appliance now.".into(),
            );
        }
        self.send(WorkerCommand::EscalateInstallToVm(capsule_id.to_string()))
    }

    fn stop(&mut self, app_id: &str) -> Result<(), String> {
        let application = self.managed_app(app_id)?;
        if application.status != AppStatus::Running {
            return Err("This application is not running.".to_string());
        }
        self.send(WorkerCommand::Stop(app_id.to_string()))?;
        self.stopping = Some(app_id.to_owned());
        Ok(())
    }

    fn remove_launcher(&mut self, app_id: &str) -> Result<(), String> {
        self.send(WorkerCommand::RemoveLauncher(app_id.to_string()))
    }

    fn vendor_uninstall(&mut self, capsule_id: &str) -> Result<(), String> {
        self.send(WorkerCommand::VendorUninstall(capsule_id.to_string()))
    }

    fn delete_data(&mut self, capsule_id: &str) -> Result<(), String> {
        self.send(WorkerCommand::DeleteData(capsule_id.to_string()))
    }

    fn take_notice(&mut self) -> Option<String> {
        self.drain();
        self.notice.take()
    }

    fn notice_is_error(&self) -> bool {
        self.notice_error
    }

    fn take_operation_result(&mut self) -> Option<Result<String, String>> {
        self.drain();
        self.operation_result.take()
    }

    fn set_route_override(&mut self, _app_id: &str, _route: Route) -> Result<(), String> {
        Err("Only an administrator can change how this app opens.".into())
    }

    fn reset_route_override(&mut self, _app_id: &str) -> Result<(), String> {
        Err("Only an administrator can change how this app opens.".into())
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the worker serializes one bounded protocol stream and its exhaustive commands"
)]
fn worker(mut client: SessionClient, commands: &Receiver<WorkerCommand>, events: &Events) {
    let mut last_listed = None;
    loop {
        let command = match commands.recv_timeout(REFRESH_INTERVAL) {
            Ok(command) => command,
            // Nothing asked for a while: look whether a program or setup moved on, and wake the
            // window only when the list changed, so a quiet manager draws nothing.
            Err(RecvTimeoutError::Timeout) => {
                let listed = list(&mut client);
                let changed = match (&listed, &last_listed) {
                    (WorkerEvent::Listed(now), Some(before)) => **now != *before,
                    _ => true,
                };
                if let WorkerEvent::Listed(now) = &listed {
                    last_listed = Some(now.as_ref().clone());
                }
                if changed && !events.send(listed) {
                    return;
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        };
        let (event, refresh) = match command {
            WorkerCommand::Refresh => {
                // Refreshing the server-owned singleton status before listing prevents
                // discovery rows from being permanently stale; the client never supplies an
                // operation ID.
                for request in [
                    ProtocolRequest::RefreshVmInstallation {
                        version: PROTOCOL_VERSION,
                    },
                    ProtocolRequest::RefreshVmUninstallation {
                        version: PROTOCOL_VERSION,
                    },
                ] {
                    let notice = match client.request(&request) {
                        Ok(
                            ProtocolResponse::VmInstallationRefreshed { .. }
                            | ProtocolResponse::VmUninstallationRefreshed { .. },
                        ) => continue,
                        Ok(ProtocolResponse::Error { error }) => error.message,
                        Ok(_) => UNEXPECTED_RESPONSE.into(),
                        Err(error) => error.to_string(),
                    };
                    let _ = events.send(WorkerEvent::Notice(notice));
                }
                (list(&mut client), false)
            }
            WorkerCommand::Launch(entry_id) => (
                launched(
                    client.request(&ProtocolRequest::Launch {
                        version: PROTOCOL_VERSION,
                        entry_id,
                    }),
                    "The Windows application started.",
                ),
                true,
            ),
            WorkerCommand::LaunchVmFallback(entry_id) => (
                launched(
                    client.request(&ProtocolRequest::LaunchVmFallback {
                        version: PROTOCOL_VERSION,
                        entry_id,
                    }),
                    "Opening the application in the Windows appliance.",
                ),
                true,
            ),
            WorkerCommand::LaunchVmWithFile { entry_id, source } => (
                launched(
                    client.request_with_file(
                        &ProtocolRequest::LaunchVmWithPortalFile {
                            version: PROTOCOL_VERSION,
                            entry_id,
                        },
                        &source,
                    ),
                    "Opening the portal-selected file in Windows.",
                ),
                true,
            ),
            WorkerCommand::Stop(entry_id) => (
                operation(client.request(&ProtocolRequest::Stop {
                    version: PROTOCOL_VERSION,
                    entry_id,
                })),
                true,
            ),
            WorkerCommand::Install {
                source,
                source_name,
            } => match client.request_with_file(
                &ProtocolRequest::Install {
                    version: PROTOCOL_VERSION,
                    source_name: source_name.clone(),
                },
                &source,
            ) {
                Ok(ProtocolResponse::VmInstallConfirmation { confirmation }) => (
                    WorkerEvent::VmInstallConfirmation {
                        source,
                        source_name,
                        confirmation,
                    },
                    false,
                ),
                response => (operation(response), true),
            },
            WorkerCommand::InstallInCapsule {
                source,
                source_name,
            } => (
                operation(client.request_with_file(
                    &ProtocolRequest::InstallInCapsule {
                        version: PROTOCOL_VERSION,
                        source_name,
                    },
                    &source,
                )),
                true,
            ),
            WorkerCommand::InstallInVm {
                source,
                source_name,
            } => (
                operation(client.request_with_file(
                    &ProtocolRequest::InstallInVm {
                        version: PROTOCOL_VERSION,
                        source_name,
                    },
                    &source,
                )),
                true,
            ),
            WorkerCommand::PublishVmCandidate(entry_id) => (
                operation(client.request(&ProtocolRequest::PublishVmCandidate {
                    version: PROTOCOL_VERSION,
                    entry_id,
                })),
                true,
            ),
            WorkerCommand::RetryInstall(capsule_id) => (
                operation(client.request(&ProtocolRequest::RetryInstall {
                    version: PROTOCOL_VERSION,
                    capsule_id,
                })),
                true,
            ),
            WorkerCommand::EscalateInstallToVm(capsule_id) => (
                operation(client.request(&ProtocolRequest::EscalateInstallToVm {
                    version: PROTOCOL_VERSION,
                    capsule_id,
                })),
                true,
            ),
            WorkerCommand::RemoveLauncher(entry_id) => (
                operation(client.request(&ProtocolRequest::RemoveLauncher {
                    version: PROTOCOL_VERSION,
                    entry_id,
                })),
                true,
            ),
            WorkerCommand::VendorUninstall(capsule_id) => (
                operation(client.request(&ProtocolRequest::VendorUninstall {
                    version: PROTOCOL_VERSION,
                    capsule_id,
                })),
                true,
            ),
            WorkerCommand::VendorUninstallVm(entry_id) => (
                operation(client.request(&ProtocolRequest::VendorUninstallVm {
                    version: PROTOCOL_VERSION,
                    entry_id,
                })),
                true,
            ),
            WorkerCommand::DeleteData(capsule_id) => (
                operation(client.request(&ProtocolRequest::DeleteData {
                    version: PROTOCOL_VERSION,
                    capsule_id,
                })),
                true,
            ),
        };
        if let WorkerEvent::Listed(now) = &event {
            last_listed = Some(now.as_ref().clone());
        }
        if !events.send(event) {
            return;
        }
        if refresh {
            let listed = list(&mut client);
            if let WorkerEvent::Listed(now) = &listed {
                last_listed = Some(now.as_ref().clone());
            }
            if !events.send(listed) {
                return;
            }
        }
    }
}

/// Where the worker's news goes: the window's queue, and the wake that has the window take it
/// in now rather than at the next input.
struct Events {
    sender: Sender<WorkerEvent>,
    wake: ui::WakeHandle,
}

impl Events {
    /// Hand `event` to the window and wake it: `false` once the window is gone.
    fn send(&self, event: WorkerEvent) -> bool {
        if self.sender.send(event).is_err() {
            return false;
        }
        self.wake.wake();
        true
    }
}

const UNEXPECTED_RESPONSE: &str = "The compatibility service returned an unexpected response.";

fn list(client: &mut SessionClient) -> WorkerEvent {
    match client.request(&ProtocolRequest::List {
        version: PROTOCOL_VERSION,
    }) {
        Ok(ProtocolResponse::Listed {
            applications,
            installations,
            vm_availability,
            vm_installation,
            vm_uninstallation,
            ended,
        }) => WorkerEvent::Listed(Box::new(Listed {
            applications,
            installations,
            vm_availability,
            vm_installation,
            vm_uninstallation,
            ended,
        })),
        Ok(ProtocolResponse::Error { error }) => WorkerEvent::Unavailable(error.message),
        Ok(_) => WorkerEvent::Unavailable(UNEXPECTED_RESPONSE.into()),
        Err(error) => WorkerEvent::Unavailable(error.to_string()),
    }
}

/// Reports a launch that started: its end comes later, with a list ([`ended_notice`]).
fn launched(response: Result<ProtocolResponse, SessionError>, started: &str) -> WorkerEvent {
    match response {
        Ok(ProtocolResponse::Launched { .. }) => WorkerEvent::Operation(Ok(started.into())),
        response => operation(response),
    }
}

/// What the manager says of a program that ended, when it is worth a sentence: a program that
/// opened and closed says nothing, since the list already shows it is no longer open. A program
/// that stopped by itself says what happened, with the status the person can quote and the last
/// of what it or its sandbox said; one that never opened says why in the program's own words.
pub(crate) fn ended_notice(end: &ProgramEnd) -> Option<String> {
    let name = end.display_name.clone();
    let said = end.said.clone();
    match (&end.error, end.success, end.exit_code) {
        (Some(error), _, _) => Some(crate::strings::format_all(
            "windows-ended-could-not-open",
            &[("app", name), ("reason", error.clone())],
        )),
        (None, false, Some(code)) => Some(match said {
            Some(said) => crate::strings::format_all(
                "windows-ended-with-error-saying",
                &[("app", name), ("status", code.to_string()), ("said", said)],
            ),
            None => crate::strings::format_all(
                "windows-ended-with-error",
                &[("app", name), ("status", code.to_string())],
            ),
        }),
        (None, false, None) => Some(match said {
            Some(said) => crate::strings::format_all(
                "windows-ended-stopped-saying",
                &[("app", name), ("said", said)],
            ),
            None => crate::strings::format_all("windows-ended-stopped", &[("app", name)]),
        }),
        (None, true, _) => None,
    }
}

/// Reports an install or management operation through the detail the service returned.
fn operation(response: Result<ProtocolResponse, SessionError>) -> WorkerEvent {
    WorkerEvent::Operation(match response {
        Ok(
            ProtocolResponse::Installed { receipt }
            | ProtocolResponse::Retried { receipt }
            | ProtocolResponse::VendorUninstalled { receipt },
        ) => Ok(receipt.detail),
        Ok(
            ProtocolResponse::VmInstallStarted { installation }
            | ProtocolResponse::VmCandidatePublished { installation },
        ) => Ok(installation.detail),
        Ok(ProtocolResponse::VmVendorUninstallation { uninstallation }) => {
            Ok(uninstallation.detail)
        }
        Ok(ProtocolResponse::Stopped { .. }) => Ok(String::new()),
        Ok(ProtocolResponse::LauncherRemoved { .. }) => {
            Ok("Launcher removed; the installed application and its data were kept.".into())
        }
        Ok(ProtocolResponse::DataDeleted { .. }) => {
            Ok("Application and all of its private capsule data were deleted.".into())
        }
        Ok(ProtocolResponse::Error { error }) => Err(error.message),
        Ok(_) => Err(UNEXPECTED_RESPONSE.into()),
        Err(error) => Err(error.to_string()),
    })
}

fn unavailable_catalogue(detail: &str) -> Catalogue {
    Catalogue {
        apps: Vec::new(),
        vm: ApplianceState {
            ready: false,
            detail: detail.to_string(),
            running_apps: 0,
            shared_session_active: false,
            shared_session_grants: Vec::new(),
            shared_session_contributors: Vec::new(),
        },
        vm_install_confirmation: None,
    }
}

fn catalogue_from_managed(
    applications: Vec<ManagedApplication>,
    installations: &[ManagedInstallation],
    vm_availability: VmAvailability,
    vm_installation: Option<VmInstallation>,
    vm_uninstallation: Option<&VmUninstallation>,
) -> Catalogue {
    let mut apps = applications
        .into_iter()
        .map(|application| {
            let installation = installations
                .iter()
                .find(|item| application.capsule_id.as_deref() == Some(item.capsule_id.as_str()));
            app_from_managed(application, installation, vm_uninstallation)
        })
        .collect::<Vec<_>>();
    for installation in installations {
        if !apps
            .iter()
            .any(|application| application.capsule == installation.capsule_id)
        {
            apps.push(app_from_installation(installation));
        }
    }
    if let Some(installation) = vm_installation {
        apps.extend(installation.candidates.iter().map(app_from_vm_candidate));
    }
    let shared_session_active = vm_availability.shared_session.active;
    let shared_session_grants = vm_availability
        .shared_session
        .effective_grants
        .iter()
        .copied()
        .map(shared_session_grant_label)
        .collect::<Vec<_>>();
    let shared_session_contributors = vm_availability
        .shared_session
        .contributors
        .iter()
        .map(|contributor| contributor.display_name.clone())
        .collect::<Vec<_>>();
    Catalogue {
        apps,
        vm: ApplianceState {
            ready: vm_availability.available,
            detail: vm_availability.detail,
            running_apps: shared_session_contributors.len(),
            shared_session_active,
            shared_session_grants,
            shared_session_contributors,
        },
        vm_install_confirmation: None,
    }
}

/// A discovery row is deliberately not a launcher.
fn app_from_vm_candidate(candidate: &compat_service::VmInstallCandidate) -> WindowsApp {
    WindowsApp {
        id: candidate.entry_id.clone(),
        name: candidate.display_name.clone(),
        publisher: candidate
            .publisher
            .clone()
            .unwrap_or_else(|| "Unknown publisher".into()),
        route: Route::Vm,
        recommended_route: Route::Vm,
        runner: "Windows".into(),
        capsule: "Shared Windows appliance".into(),
        catalogue_source: "Found by Windows setup".into(),
        confidence: Confidence::Verified,
        reason: candidate.version.as_ref().map_or_else(
            || "Windows setup found this app. Add it to Windows Apps to open it from here.".into(),
            |version| format!("Windows setup found version {version} of this app. Add it to Windows Apps to open it from here."),
        ),
        status: AppStatus::Ready,
        grants: Vec::new(),
        fallback_detail: String::new(),
        launch_available: false,
        launch_unavailable_reason: Some("Add this app to Windows Apps first.".into()),
        portal_file_open_available: false,
        portal_file_open_unavailable_reason: Some(
            "Add this app to Windows Apps first.".into(),
        ),
        route_override_available: false,
        route_override_unavailable_reason: None,
        can_retry_install: false,
        can_escalate_install_to_vm: false,
        can_remove_launcher: false,
        can_vendor_uninstall: false,
        can_delete_data: false,
        can_publish_vm_candidate: true,
        installation_detail: Some("Found during Windows setup".into()),
        vm_fallback_available: false,
        vm_fallback_unavailable_reason: Some("Add this app to Windows Apps first.".into()),
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the mapper keeps every manager field visibly tied to the authenticated DTO"
)]
fn app_from_managed(
    application: ManagedApplication,
    installation: Option<&ManagedInstallation>,
    vm_uninstallation: Option<&VmUninstallation>,
) -> WindowsApp {
    let explanation = application.explanation;
    let route = match explanation.route {
        RouteKind::Wine => Route::Wine,
        RouteKind::Proton => Route::Proton,
        RouteKind::Vm => Route::Vm,
    };
    let vm_uninstallation = vm_uninstallation.filter(|status| status.entry_id == application.id);
    let active_vm_uninstall = vm_uninstallation.filter(|status| {
        matches!(
            status.phase,
            compat_service::VmInstallationPhase::Starting
                | compat_service::VmInstallationPhase::Running
        )
    });
    let failed_vm_uninstall = vm_uninstallation
        .filter(|status| status.phase == compat_service::VmInstallationPhase::Failed);
    let launch_available =
        (route != Route::Vm || application.vm_launch_available) && active_vm_uninstall.is_none();
    let launch_unavailable_reason = active_vm_uninstall
        .map(|status| status.detail.clone())
        .or_else(|| {
            (!launch_available)
                .then(|| application.vm_unavailable_reason.clone())
                .flatten()
        });
    let portal_file_open_available =
        application.portal_file_open_available && active_vm_uninstall.is_none();
    let portal_file_open_unavailable_reason = active_vm_uninstall
        .map(|status| status.detail.clone())
        .or_else(|| {
            if application.presentation == Some(compat_service::VmPresentation::AppFocusedSpice) {
                Some(files_cannot_reach_window())
            } else {
                application.portal_file_open_unavailable_reason.clone()
            }
        });
    let grants = vec![
        Grant {
            label: "Files it can read".into(),
            detail: locations(explanation.access.files_read_only),
            enabled: explanation.access.files_read_only > 0,
        },
        Grant {
            label: "Files it can change".into(),
            detail: locations(explanation.access.files_read_write),
            enabled: explanation.access.files_read_write > 0,
        },
        Grant {
            label: "Network".into(),
            detail: network_label(&explanation.access.network),
            enabled: explanation.access.network != "none",
        },
        Grant {
            label: "Microphone".into(),
            detail: on_off(explanation.access.microphone),
            enabled: explanation.access.microphone,
        },
        Grant {
            label: "Sound".into(),
            detail: on_off(application.audio_output_available),
            enabled: application.audio_output_available,
        },
    ];
    WindowsApp {
        id: application.id,
        name: application.display_name,
        publisher: application.publisher,
        route,
        recommended_route: route,
        runner: explanation.runner,
        capsule: if route == Route::Vm {
            "Shared Windows appliance".into()
        } else {
            application
                .capsule_id
                .clone()
                .unwrap_or_else(|| "Unavailable private capsule".into())
        },
        catalogue_source: if route == Route::Vm {
            "Installed in Windows".into()
        } else if explanation.catalogue_record.is_some() {
            "Tilo compatibility catalogue".into()
        } else {
            "Default settings".into()
        },
        confidence: match explanation.match_reason {
            MatchReason::DefaultWine => Confidence::Fallback,
            MatchReason::Executable | MatchReason::ProductPublisher => Confidence::Heuristic,
            MatchReason::ExecutableDigest
            | MatchReason::GameId
            | MatchReason::StoreProduct
            | MatchReason::AppId
            | MatchReason::PackageId => Confidence::Verified,
        },
        reason: explanation.summary,
        status: active_vm_uninstall.map_or_else(
            || {
                failed_vm_uninstall.map_or_else(
                    || {
                        if application.running {
                            AppStatus::Running
                        } else {
                            application
                                .installation_status
                                .map_or(AppStatus::Ready, app_status)
                        }
                    },
                    |_| AppStatus::Failed,
                )
            },
            |_| AppStatus::Uninstalling,
        ),
        grants,
        fallback_detail: active_vm_uninstall
            .or(failed_vm_uninstall)
            .map(|status| status.detail.clone())
            .unwrap_or_default(),
        launch_available,
        launch_unavailable_reason,
        portal_file_open_available,
        portal_file_open_unavailable_reason,
        route_override_available: false,
        route_override_unavailable_reason: Some(
            "Only an administrator can change how this app opens.".into(),
        ),
        can_retry_install: installation.is_some_and(|item| item.can_retry),
        can_escalate_install_to_vm: installation.is_some_and(can_escalate_install_to_vm),
        can_remove_launcher: application.remove_launcher_available,
        can_vendor_uninstall: active_vm_uninstall.is_none()
            && (application.vendor_uninstall_available
                || installation.is_some_and(|item| item.can_vendor_uninstall)),
        can_delete_data: installation.is_some_and(|item| item.can_delete_data),
        can_publish_vm_candidate: false,
        installation_detail: installation.map(|item| item.detail.clone()),
        vm_fallback_available: application.vm_launch_available,
        vm_fallback_unavailable_reason: application.vm_unavailable_reason,
    }
}

fn app_from_installation(installation: &ManagedInstallation) -> WindowsApp {
    let route = route_from_kind(installation.route);
    WindowsApp {
        id: format!("installation-{}", installation.capsule_id),
        name: installation.display_name.clone(),
        publisher: "Imported application".into(),
        route,
        recommended_route: route,
        runner: installation.runner.clone(),
        capsule: installation.capsule_id.clone(),

        catalogue_source: "Added from a file".into(),
        confidence: Confidence::Heuristic,
        reason: "Tilo kept this setup so you can retry it or remove it safely.".into(),
        status: app_status(installation.status),
        grants: Vec::new(),
        fallback_detail: installation.detail.clone(),
        launch_available: false,
        launch_unavailable_reason: Some("This setup hasn't added an app to open yet.".into()),
        portal_file_open_available: false,
        portal_file_open_unavailable_reason: Some(
            "This setup hasn't added an app to open yet.".into(),
        ),
        route_override_available: false,
        route_override_unavailable_reason: None,
        can_retry_install: installation.can_retry,
        can_escalate_install_to_vm: can_escalate_install_to_vm(installation),
        can_remove_launcher: false,
        can_vendor_uninstall: installation.can_vendor_uninstall,
        can_delete_data: installation.can_delete_data,
        can_publish_vm_candidate: false,
        installation_detail: Some(installation.detail.clone()),
        vm_fallback_available: false,
        vm_fallback_unavailable_reason: Some("This setup hasn't added an app to open yet.".into()),
    }
}

/// Mirror the server's coarse lifecycle check in the view model.
fn can_escalate_install_to_vm(installation: &ManagedInstallation) -> bool {
    matches!(installation.route, RouteKind::Wine | RouteKind::Proton)
        && !matches!(
            installation.status,
            InstallationStatus::Installing
                | InstallationStatus::Uninstalling
                | InstallationStatus::Uninstalled
        )
}

fn shared_session_grant_label(grant: compat_service::VmSharedSessionGrantKind) -> String {
    strings::t(match grant {
        compat_service::VmSharedSessionGrantKind::AudioPlayback => "windows-grant-audio-playback",
        compat_service::VmSharedSessionGrantKind::Clipboard => "windows-grant-clipboard",
        compat_service::VmSharedSessionGrantKind::Microphone => "windows-grant-microphone",
        compat_service::VmSharedSessionGrantKind::NetworkEgress => "windows-grant-network",
        compat_service::VmSharedSessionGrantKind::PortalFilesReadOnly => {
            "windows-grant-read-only-files"
        }
        compat_service::VmSharedSessionGrantKind::PortalFilesReadWrite => {
            "windows-grant-writable-files"
        }
    })
}

fn route_from_kind(route: RouteKind) -> Route {
    match route {
        RouteKind::Wine => Route::Wine,
        RouteKind::Proton => Route::Proton,
        RouteKind::Vm => Route::Vm,
    }
}

fn files_cannot_reach_window() -> String {
    strings::t("windows-files-cannot-reach")
}

fn on_off(on: bool) -> String {
    strings::t(if on { "windows-on" } else { "windows-off" })
}

/// How many places an app was given, in words.
fn locations(count: usize) -> String {
    match count {
        0 => strings::t("windows-off"),
        1 => strings::t("windows-location-one"),
        n => format_all("windows-locations", &[("count", n.to_string())]),
    }
}

/// The service's network shape in words: off, open, or the sites allowed.
fn network_label(network: &str) -> String {
    match network {
        "none" => strings::t("windows-off"),
        "unrestricted" => strings::t("windows-on"),
        other => other
            .strip_suffix(" approved host(s)")
            .and_then(|count| count.parse::<usize>().ok())
            .map_or_else(
                || other.to_owned(),
                |count| match count {
                    1 => strings::t("windows-approved-site-one"),
                    n => format_all("windows-approved-sites", &[("count", n.to_string())]),
                },
            ),
    }
}

fn app_status(status: InstallationStatus) -> AppStatus {
    match status {
        InstallationStatus::Installing | InstallationStatus::Uninstalling => AppStatus::Installing,
        InstallationStatus::Installed => AppStatus::Ready,
        InstallationStatus::NeedsAttention | InstallationStatus::Failed => AppStatus::Failed,
        InstallationStatus::Uninstalled => AppStatus::Uninstalled,
    }
}

#[cfg(test)]
mod tests {
    use compat_service::{AccessSummary, ExplainStatus};

    use super::*;

    #[test]
    fn publishing_a_vm_candidate_reports_the_service_detail() {
        let event = operation(Ok(ProtocolResponse::VmCandidatePublished {
            installation: VmInstallation {
                detail: "Example Editor was added to Windows Apps.".into(),
                phase: compat_service::VmInstallationPhase::Completed,
                exit_code: Some(0),
                candidates: Vec::new(),
            },
        }));
        assert!(matches!(
            event,
            WorkerEvent::Operation(Ok(detail)) if detail == "Example Editor was added to Windows Apps."
        ));
    }

    fn retained_installation(status: InstallationStatus) -> ManagedInstallation {
        ManagedInstallation {
            capsule_id: "retained-setup".into(),
            display_name: "Retained setup".into(),
            route: RouteKind::Wine,
            runner: "Wine".into(),
            status,
            program_count: 0,
            can_retry: true,
            can_vendor_uninstall: false,
            can_delete_data: true,
            detail: "The local setup needs attention.".into(),
        }
    }

    #[test]
    fn live_model_explains_vm_availability_and_setup_recovery() {
        {
            assert!(can_escalate_install_to_vm(&retained_installation(
                InstallationStatus::Failed
            )));
            assert!(can_escalate_install_to_vm(&retained_installation(
                InstallationStatus::NeedsAttention
            )));
            assert!(!can_escalate_install_to_vm(&retained_installation(
                InstallationStatus::Installing
            )));
            assert!(!can_escalate_install_to_vm(&retained_installation(
                InstallationStatus::Uninstalling
            )));
            assert!(!can_escalate_install_to_vm(&retained_installation(
                InstallationStatus::Uninstalled
            )));
        }
        {
            let app = app_from_managed(managed_vm(), None, None);
            assert!(!app.launch_available);
            assert_eq!(
                app.launch_unavailable_reason.as_deref(),
                Some("Guest bridge unavailable")
            );
            assert!(
                app.grants
                    .iter()
                    .any(|grant| grant.label == "Sound" && !grant.enabled && grant.detail == "Off")
            );
            assert_eq!(app.capsule, "Shared Windows appliance");
            assert_eq!(app.catalogue_source, "Installed in Windows");
            assert!(!app.portal_file_open_available);
            assert_eq!(
                app.portal_file_open_unavailable_reason.as_deref(),
                Some(files_cannot_reach_window().as_str()),
                "the person reads why files can't arrive, not the presentation's name"
            );
            assert!(
                app.fallback_detail.is_empty(),
                "each unavailable action explains itself where it is"
            );
        }
        {
            let mut application = managed_vm();
            application.vm_launch_available = true;
            application.presentation = Some(compat_service::VmPresentation::NativeRail);
            application.portal_file_open_available = true;
            application.portal_file_open_unavailable_reason = None;

            let app = app_from_managed(application, None, None);

            assert!(app.portal_file_open_available);
            assert_eq!(app.portal_file_open_unavailable_reason, None);
        }
        {
            let catalogue = catalogue_from_managed(
                Vec::new(),
                &[],
                VmAvailability::ready("The managed Windows appliance is ready."),
                None,
                None,
            );
            assert!(catalogue.apps.is_empty());
            assert!(catalogue.vm.ready);
            assert_eq!(
                catalogue.vm.detail,
                "The managed Windows appliance is ready."
            );
        }
    }

    fn managed_vm() -> ManagedApplication {
        ManagedApplication {
            id: "windows-editor".into(),
            app_id: "so.tilo.windows.editor".into(),
            display_name: "Editor".into(),
            publisher: "Vendor".into(),
            capsule_id: None,
            program_id: "editor".into(),
            explanation: ExplainStatus {
                route: RouteKind::Vm,
                catalogue_record: Some("editor-vm".into()),
                match_reason: MatchReason::ProductPublisher,
                capsule_id: None,
                runner: "windows-vm managed".into(),
                security_profile: "windows-app".into(),
                vm_fallback_was_explicit: false,
                access: AccessSummary {
                    files_read_only: 0,
                    files_read_write: 0,
                    network: "none".into(),
                    microphone: false,
                    audio_output: false,
                },
                summary: "Managed fallback".into(),
            },
            vm_launch_available: false,
            vm_unavailable_reason: Some("Guest bridge unavailable".into()),
            presentation: Some(compat_service::VmPresentation::AppFocusedSpice),
            portal_file_open_available: false,
            portal_file_open_unavailable_reason: Some(
                "Open file is unavailable in app-focused SPICE because this presentation has no portal file bridge."
                    .into(),
            ),
            audio_output_available: false,
            audio_unavailable_reason: Some("Playback-only proxy unavailable".into()),
            installation_status: None,
            remove_launcher_available: false,
            vendor_uninstall_available: false,
            running: false,
        }
    }

    #[test]
    fn vm_publications_and_session_access_are_explicit() {
        {
            let mut availability = VmAvailability::ready("Ready");
            availability.shared_session = compat_service::VmSharedSessionLeaseSummary {
                active: true,
                effective_grants: vec![
                    compat_service::VmSharedSessionGrantKind::Clipboard,
                    compat_service::VmSharedSessionGrantKind::PortalFilesReadOnly,
                ],
                contributors: vec![compat_service::VmSharedSessionContributor {
                    published_application_id: "editor".into(),
                    display_name: "Editor".into(),
                }],
            };
            let catalogue = catalogue_from_managed(Vec::new(), &[], availability, None, None);
            assert!(catalogue.vm.shared_session_active);
            assert_eq!(
                catalogue.vm.shared_session_grants,
                ["Clipboard", "Read-only files"]
            );
            assert_eq!(catalogue.vm.shared_session_contributors, ["Editor"]);
            assert_eq!(catalogue.vm.running_apps, 1);
        }
        {
            let catalogue = catalogue_from_managed(
                Vec::new(),
                &[],
                VmAvailability::ready("Ready"),
                Some(VmInstallation {
                    detail: "Discovered application".into(),
                    phase: compat_service::VmInstallationPhase::Completed,
                    exit_code: Some(0),
                    candidates: vec![compat_service::VmInstallCandidate {
                        entry_id: "windows-vm-candidate-server-owned".into(),
                        display_name: "Example Editor".into(),
                        publisher: Some("Example Corp".into()),
                        version: Some("1.0".into()),
                    }],
                }),
                None,
            );
            let app = catalogue.app("windows-vm-candidate-server-owned").unwrap();
            assert!(app.can_publish_vm_candidate);
            assert!(!app.launch_available);
            assert_eq!(app.capsule, "Shared Windows appliance");
        }
        {
            let mut application = managed_vm();
            application.capsule_id = Some("private-capsule".into());
            application.explanation.route = RouteKind::Wine;
            application.vm_launch_available = false;
            application.vm_unavailable_reason = Some(
                "Open in Windows VM is available only for published Windows guest applications."
                    .into(),
            );

            let app = app_from_managed(application, None, None);

            assert!(!app.vm_fallback_available);
            assert!(
                app.vm_fallback_unavailable_reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("published Windows guest"))
            );
        }
    }

    #[test]
    fn vm_uninstall_refresh_and_disconnect_keep_the_cache_honest() {
        {
            let mut application = managed_vm();
            application.vm_launch_available = true;
            application.vendor_uninstall_available = true;
            let uninstallation = VmUninstallation {
                entry_id: application.id.clone(),
                detail: "Windows vendor uninstall is running.".into(),
                phase: compat_service::VmInstallationPhase::Running,
                exit_code: None,
            };

            let app = app_from_managed(application, None, Some(&uninstallation));

            assert_eq!(app.status, AppStatus::Uninstalling);
            assert!(!app.launch_available);
            assert!(!app.portal_file_open_available);
            assert!(
                app.portal_file_open_unavailable_reason
                    .as_deref()
                    .is_some_and(|reason| reason.contains("uninstall is running"))
            );
            assert!(!app.can_vendor_uninstall);
            assert!(app.fallback_detail.contains("uninstall is running"));
        }
        {
            let mut application = managed_vm();
            application.vendor_uninstall_available = true;
            let uninstallation = VmUninstallation {
                entry_id: application.id.clone(),
                detail: "Windows vendor uninstall failed; retry is available.".into(),
                phase: compat_service::VmInstallationPhase::Failed,
                exit_code: Some(1),
            };

            let app = app_from_managed(application, None, Some(&uninstallation));

            assert_eq!(app.status, AppStatus::Failed);
            assert!(app.can_vendor_uninstall);
            assert!(app.fallback_detail.contains("retry is available"));
        }
        {
            let (command_tx, _command_rx) = mpsc::channel();
            let (event_tx, event_rx) = mpsc::channel();
            let mut sample = SampleProvider::new();
            let mut provider = LiveProvider {
                cached: sample.catalogue(),
                commands: command_tx,
                events: event_rx,
                notice: None,
                notice_error: false,
                stopping: None,
                operation_result: None,
                pending_vm_install: None,
                pending_vm_install_confirmation: None,
            };
            event_tx
                .send(WorkerEvent::Unavailable("invalid response".into()))
                .unwrap();
            let catalogue = provider.catalogue();
            assert!(catalogue.apps.is_empty());
            assert_eq!(catalogue.vm.detail, "invalid response");
        }
    }

    /// A pending installer choice survives a list; a launch is answered once the program
    /// started, the list then shows it open and offers to end it, and once it ended the manager
    /// says how.
    #[test]
    fn a_list_keeps_the_choice_and_tells_what_programs_did() {
        let (command_tx, _command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let mut provider = LiveProvider {
            cached: unavailable_catalogue("Connecting"),
            commands: command_tx,
            events: event_rx,
            notice: None,
            notice_error: false,
            stopping: None,
            operation_result: None,
            pending_vm_install: None,
            pending_vm_install_confirmation: Some(VmInstallConfirmation {
                display_name: "Example setup".into(),
                detail: "Choose a destination.".into(),
                recommended_route: Route::Vm,
            }),
        };
        event_tx
            .send(WorkerEvent::Listed(Box::new(Listed {
                applications: Vec::new(),
                installations: Vec::new(),
                vm_availability: VmAvailability::ready("Ready"),
                vm_installation: None,
                vm_uninstallation: None,
                ended: Vec::new(),
            })))
            .unwrap();

        let catalogue = provider.catalogue();

        assert_eq!(
            catalogue
                .vm_install_confirmation
                .as_ref()
                .map(|confirmation| confirmation.display_name.as_str()),
            Some("Example setup")
        );

        // A launch is answered once the program started; the list then shows it going and
        // offers to end it, and once it ended the manager says how.
        provider.pending_vm_install_confirmation = None;
        let listed = |going: bool, ended: Vec<ProgramEnd>| {
            let mut application = managed_vm();
            application.id = "probe".into();
            application.capsule_id = Some("capsule".into());
            application.presentation = None;
            application.running = going;
            WorkerEvent::Listed(Box::new(Listed {
                applications: vec![application],
                installations: Vec::new(),
                vm_availability: VmAvailability::ready("Ready"),
                vm_installation: None,
                vm_uninstallation: None,
                ended,
            }))
        };
        event_tx.send(listed(true, Vec::new())).unwrap();
        let app = provider.catalogue().app("probe").cloned().expect("listed");
        assert_eq!(app.status, AppStatus::Running);
        provider
            .stop("probe")
            .expect("a program that is open can be ended");
        event_tx
            .send(listed(
                false,
                vec![ProgramEnd {
                    entry_id: "probe".into(),
                    display_name: "Probe".into(),
                    success: false,
                    exit_code: Some(3),
                    error: None,
                    said: None,
                }],
            ))
            .unwrap();
        let app = provider.catalogue().app("probe").cloned().expect("listed");
        assert_ne!(app.status, AppStatus::Running);
        assert_eq!(
            provider.take_notice().as_deref(),
            Some("Probe stopped with an error (status 3).")
        );
        assert!(provider.notice_is_error(), "an error is said as one");
        assert!(provider.stop("probe").is_err(), "nothing is open to end");
        let said = |exit_code| ProgramEnd {
            entry_id: "probe".into(),
            display_name: "Probe".into(),
            success: false,
            exit_code,
            error: None,
            said: Some("wine: could not load ntdll.so".into()),
        };
        assert_eq!(
            ended_notice(&said(Some(3))).as_deref(),
            Some("Probe stopped with an error (status 3): wine: could not load ntdll.so")
        );
        assert_eq!(
            ended_notice(&said(None)).as_deref(),
            Some("Probe stopped: wine: could not load ntdll.so")
        );
    }
}
