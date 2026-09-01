//! Provider boundary for the session compatibility service. No runner command
//! or host path is constructed in UI code.
use std::fs::File;
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use compat_service::{
    InstallationStatus, ManagedApplication, ManagedInstallation, MatchReason, PROTOCOL_VERSION,
    ProtocolRequest, ProtocolResponse, RouteKind, SessionClient, VmAvailability, VmInstallation,
    VmUninstallation,
};

use crate::model::{
    AppStatus, ApplianceState, Catalogue, Confidence, Grant, Route, VmInstallConfirmation,
    WindowsApp,
};

const REFRESH_INTERVAL: Duration = Duration::from_secs(2);

pub trait CompatibilityProvider: Send {
    fn catalogue(&mut self) -> Catalogue;
    /// Queue a local launch of a server-owned managed entry.
    ///
    /// # Errors
    ///
    /// Returns an error if the entry is not in the authenticated snapshot or
    /// the service worker is unavailable.
    fn launch(&mut self, app_id: &str) -> Result<(), String>;
    /// Queue the separately authorized VM fallback action.
    ///
    /// # Errors
    ///
    /// Returns an error when the VM is unavailable or the entry is unknown.
    fn launch_vm_fallback(&mut self, app_id: &str) -> Result<(), String>;
    /// Transfer a portal-open installer descriptor to the service.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker cannot accept the descriptor.
    fn install(&mut self, source: File, source_name: &str) -> Result<(), String>;
    /// # Errors
    ///
    /// Returns an error if the confirmed portal descriptor is unavailable.
    fn confirm_capsule_install(&mut self) -> Result<(), String>;
    /// # Errors
    ///
    /// Returns an error if the confirmed portal descriptor or appliance is unavailable.
    fn confirm_vm_install(&mut self) -> Result<(), String>;
    /// # Errors
    ///
    /// Returns an error if the server-owned candidate is unavailable.
    fn publish_vm_candidate(&mut self, entry_id: &str) -> Result<(), String>;
    /// # Errors
    ///
    /// Returns an error if the server-owned guest uninstaller is unavailable.
    fn vendor_uninstall_vm(&mut self, entry_id: &str) -> Result<(), String>;
    /// Retry setup from the retained digest-pinned installer.
    ///
    /// # Errors
    ///
    /// Returns an error when the capsule is unavailable or unauthorized.
    fn retry_install(&mut self, capsule_id: &str) -> Result<(), String>;
    /// Remove one launcher while retaining its installation.
    ///
    /// # Errors
    ///
    /// Returns an error when the launcher is unavailable or unauthorized.
    fn remove_launcher(&mut self, app_id: &str) -> Result<(), String>;
    /// Run the discovered vendor uninstaller inside its capsule.
    ///
    /// # Errors
    ///
    /// Returns an error when no authorized vendor uninstaller exists.
    fn vendor_uninstall(&mut self, capsule_id: &str) -> Result<(), String>;
    /// Delete a managed installation and its private capsule data.
    ///
    /// # Errors
    ///
    /// Returns an error when the capsule is unavailable or unauthorized.
    fn delete_data(&mut self, capsule_id: &str) -> Result<(), String>;
    fn take_notice(&mut self) -> Option<String> {
        None
    }
    /// Sets a user route override.
    ///
    /// # Errors
    ///
    /// Returns an error when the service rejects the app or requested route.
    fn set_route_override(&mut self, app_id: &str, route: Route) -> Result<(), String>;
    /// Removes a user route override.
    ///
    /// # Errors
    ///
    /// Returns an error when the service does not know the app.
    fn reset_route_override(&mut self, app_id: &str) -> Result<(), String>;
}

/// Safe live fallback until the compatibility service endpoint is available.
/// A control-plane UI must never make fixture software look installed.
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
        Self::with_detail("The qOS compatibility service is unavailable in this session.")
    }
}

impl CompatibilityProvider for EmptyProvider {
    fn catalogue(&mut self) -> Catalogue {
        Catalogue {
            apps: Vec::new(),
            vm: ApplianceState {
                ready: false,
                detail: self.detail.clone(),
                running_apps: 0,
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
        Err("The qOS compatibility service is not connected.".into())
    }

    fn reset_route_override(&mut self, _app_id: &str) -> Result<(), String> {
        Err("The qOS compatibility service is not connected.".into())
    }
}

/// Predictable fixture. It deliberately looks like the service response, not a
/// mock of Wine: code depending on it cannot accidentally gain host execution.
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
                WindowsApp { id: "adobe-reader".into(), name: "Adobe Acrobat Reader".into(), publisher: "Adobe".into(), route: Route::Wine, recommended_route: Route::Wine, runner: "Wine 11.0 (qOS pinned)".into(), capsule: "Adobe Reader".into(), prefix: "capsules/adobe-reader".into(), catalogue_source: "qOS compatibility catalogue".into(), confidence: Confidence::Verified, reason: "Verified desktop-app recipe; isolated Wine capsule is the smallest compatible layer.".into(), status: AppStatus::Ready, grants: grants(&[("Downloads", "Read and write", true), ("Clipboard", "On demand", true), ("Camera", "Not granted", false)]), fallback_detail: "Repair this private capsule or use its own supported route.".into(), launch_available: true, launch_unavailable_reason: None, route_override_available: true, route_override_unavailable_reason: None, can_retry_install: false, can_remove_launcher: true, can_vendor_uninstall: true, can_delete_data: true, can_publish_vm_candidate: false, installation_detail: Some("Installed in a private capsule.".into()), vm_fallback_available: false, vm_fallback_unavailable_reason: Some("Open in Windows VM is available only for published Windows guest applications.".into()) },
                WindowsApp { id: "cyberpunk-2077".into(), name: "Cyberpunk 2077".into(), publisher: "CD PROJEKT RED".into(), route: Route::Proton, recommended_route: Route::Proton, runner: "UMU-Proton 10.0-3".into(), capsule: "Steam · Cyberpunk 2077".into(), prefix: "games/cyberpunk-2077".into(), catalogue_source: "Steam + ProtonDB recipe".into(), confidence: Confidence::Community, reason: "Game route uses Steam Runtime and UMU-Proton, including the game's known compatibility fixups.".into(), status: AppStatus::Running, grants: grants(&[("Games library", "Read and write", true), ("Controller", "Connected devices", true), ("Microphone", "Not granted", false)]), fallback_detail: "Use the catalogue-selected Proton repair route.".into(), launch_available: true, launch_unavailable_reason: None, route_override_available: true, route_override_unavailable_reason: None, can_retry_install: false, can_remove_launcher: true, can_vendor_uninstall: true, can_delete_data: true, can_publish_vm_candidate: false, installation_detail: Some("Installed in a private capsule.".into()), vm_fallback_available: false, vm_fallback_unavailable_reason: Some("Open in Windows VM is available only for published Windows guest applications.".into()) },
                WindowsApp { id: "visual-studio".into(), name: "Visual Studio".into(), publisher: "Microsoft".into(), route: Route::Vm, recommended_route: Route::Vm, runner: "Windows 11 managed VM".into(), capsule: "Windows workspace".into(), prefix: "vm/windows-workspace".into(), catalogue_source: "qOS fallback policy".into(), confidence: Confidence::Fallback, reason: "Requires Windows services not safely reproduced in a Wine capsule; it opens as an integrated guest app.".into(), status: AppStatus::Ready, grants: grants(&[("Workspace exchange", "Selected files only", true), ("Clipboard", "On demand", true), ("USB devices", "Not granted", false)]), fallback_detail: "One shared VM is suspended when idle. Its desktop is used only for setup or recovery.".into(), launch_available: true, launch_unavailable_reason: None, route_override_available: true, route_override_unavailable_reason: None, can_retry_install: false, can_remove_launcher: true, can_vendor_uninstall: true, can_delete_data: true, can_publish_vm_candidate: false, installation_detail: Some("Managed by the fallback appliance.".into()), vm_fallback_available: true, vm_fallback_unavailable_reason: None },
            ], vm: ApplianceState { ready: true, detail: "Managed Windows workspace · suspended when idle · 2 guest apps available".into(), running_apps: 0 }, vm_install_confirmation: None } }
    }
}
impl Default for SampleProvider {
    fn default() -> Self {
        Self::new()
    }
}
impl CompatibilityProvider for SampleProvider {
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
            runner: "Wine 11.0 (qOS pinned)".into(),
            capsule: "imported-sample".into(),
            prefix: "Private capsule · host path hidden".into(),
            catalogue_source: "qOS safe default".into(),
            confidence: Confidence::Heuristic,
            reason: "The installer is being validated and run in a private capsule.".into(),
            status: AppStatus::Installing,
            grants: Vec::new(),
            fallback_detail: "Setup is isolated from the host.".into(),
            launch_available: false,
            launch_unavailable_reason: Some("Setup is still running.".into()),
            route_override_available: false,
            route_override_unavailable_reason: None,
            can_retry_install: false,
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
            Route::Wine => "Wine 11.0 (qOS pinned)",
            Route::Proton => "UMU-Proton 10.0-3",
            Route::Vm => "Windows 11 managed VM",
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
    Install { source: File, source_name: String },
    InstallInCapsule { source: File, source_name: String },
    InstallInVm { source: File, source_name: String },
    PublishVmCandidate(String),
    RetryInstall(String),
    RemoveLauncher(String),
    VendorUninstall(String),
    VendorUninstallVm(String),
    DeleteData(String),
}

enum WorkerEvent {
    Listed {
        applications: Vec<ManagedApplication>,
        installations: Vec<ManagedInstallation>,
        vm_availability: VmAvailability,
        vm_installation: Option<VmInstallation>,
        vm_uninstallation: Option<VmUninstallation>,
    },
    VmInstallConfirmation {
        source: File,
        source_name: String,
        confirmation: compat_service::VmInstallConfirmation,
    },
    Unavailable(String),
    Notice(String),
}

pub struct LiveProvider {
    cached: Catalogue,
    commands: Sender<WorkerCommand>,
    events: Receiver<WorkerEvent>,
    refresh_pending: bool,
    next_refresh: Instant,
    notice: Option<String>,
    pending_vm_install: Option<(File, String)>,
}

impl LiveProvider {
    /// Connect to the launcher-inherited descriptor and move all blocking I/O
    /// onto a dedicated worker thread.
    ///
    /// # Errors
    ///
    /// Returns an error when no verified descriptor was inherited or the
    /// bounded worker thread cannot be started.
    pub fn connect() -> Result<Self, String> {
        let raw = runtime_portal::service_fd("compat")
            .ok_or_else(|| "The qOS compatibility service is unavailable.".to_string())?;
        // SAFETY: `runtime_portal::service_fd` verified that the inherited slot
        // is open. The app creates exactly one live provider, which becomes its
        // sole owner and transfers it immediately to `SessionClient`.
        let owned = unsafe { OwnedFd::from_raw_fd(raw) };
        let client = SessionClient::from_owned_fd(owned);
        let (command_tx, command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        std::thread::Builder::new()
            .name("qos-windows-service".to_string())
            .spawn(move || worker(client, &command_rx, &event_tx))
            .map_err(|error| format!("Could not start compatibility worker: {error}"))?;
        command_tx
            .send(WorkerCommand::Refresh)
            .map_err(|_| "The compatibility worker stopped before startup.".to_string())?;
        Ok(Self {
            cached: unavailable_catalogue("Connecting to the qOS compatibility service."),
            commands: command_tx,
            events: event_rx,
            refresh_pending: true,
            next_refresh: Instant::now() + REFRESH_INTERVAL,
            notice: None,
            pending_vm_install: None,
        })
    }

    fn drain(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                WorkerEvent::Listed {
                    applications,
                    installations,
                    vm_availability,
                    vm_installation,
                    vm_uninstallation,
                } => {
                    self.cached = catalogue_from_managed(
                        applications,
                        &installations,
                        vm_availability,
                        vm_installation,
                        vm_uninstallation,
                    );
                    self.refresh_pending = false;
                    self.next_refresh = Instant::now() + REFRESH_INTERVAL;
                }
                WorkerEvent::Unavailable(detail) => {
                    // A malformed, mismatched, or disconnected response never
                    // leaves stale installed applications painted as live.
                    self.cached = unavailable_catalogue(&detail);
                    self.refresh_pending = false;
                    self.next_refresh = Instant::now() + REFRESH_INTERVAL;
                    self.notice = Some(detail);
                }
                WorkerEvent::Notice(detail) => self.notice = Some(detail),
                WorkerEvent::VmInstallConfirmation {
                    source,
                    source_name,
                    confirmation,
                } => {
                    self.pending_vm_install = Some((source, source_name));
                    self.notice = Some(confirmation.detail.clone());
                    self.cached.vm_install_confirmation = Some(VmInstallConfirmation {
                        display_name: confirmation.display_name,
                        detail: confirmation.detail,
                        recommended_route: route_from_kind(confirmation.recommended_route),
                    });
                }
            }
        }
    }
}

impl CompatibilityProvider for LiveProvider {
    fn catalogue(&mut self) -> Catalogue {
        self.drain();
        if !self.refresh_pending && Instant::now() >= self.next_refresh {
            if self.commands.send(WorkerCommand::Refresh).is_ok() {
                self.refresh_pending = true;
            } else {
                self.cached = unavailable_catalogue("The compatibility service disconnected.");
            }
        }
        self.cached.clone()
    }

    fn launch(&mut self, app_id: &str) -> Result<(), String> {
        self.drain();
        let application = self.cached.app(app_id).ok_or_else(|| {
            "That managed Windows application is not in this session.".to_string()
        })?;
        if !application.launch_available {
            return Err(application
                .launch_unavailable_reason
                .clone()
                .unwrap_or_else(|| "This launch route is unavailable.".to_string()));
        }
        self.commands
            .send(WorkerCommand::Launch(app_id.to_string()))
            .map_err(|_| "The compatibility service worker has stopped.".to_string())
    }

    fn launch_vm_fallback(&mut self, app_id: &str) -> Result<(), String> {
        self.drain();
        let application = self.cached.app(app_id).ok_or_else(|| {
            "That managed Windows application is not in this session.".to_string()
        })?;
        if !application.vm_fallback_available {
            return Err(application
                .vm_fallback_unavailable_reason
                .clone()
                .unwrap_or_else(|| "The Windows fallback is unavailable.".to_string()));
        }
        self.commands
            .send(WorkerCommand::LaunchVmFallback(app_id.to_string()))
            .map_err(|_| "The compatibility service worker has stopped.".to_string())
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
            runner: "Selecting from catalogue…".into(),
            capsule: "Pending private capsule".into(),
            prefix: "Private capsule · host path hidden".into(),
            catalogue_source: "Inspecting exact installer digest".into(),
            confidence: Confidence::Heuristic,
            reason: "qOS is validating the selected file and choosing its compatibility recipe."
                .into(),
            status: AppStatus::Installing,
            grants: Vec::new(),
            fallback_detail: "Setup runs outside the manager in the compatibility service.".into(),
            launch_available: false,
            launch_unavailable_reason: Some("Setup is still running.".into()),
            route_override_available: false,
            route_override_unavailable_reason: None,
            can_retry_install: false,
            can_remove_launcher: false,
            can_vendor_uninstall: false,
            can_delete_data: false,
            can_publish_vm_candidate: false,
            installation_detail: Some("Validating and installing…".into()),
            vm_fallback_available: false,
            vm_fallback_unavailable_reason: Some("Setup is still running.".into()),
        });
        self.commands
            .send(WorkerCommand::Install {
                source,
                source_name: source_name.to_string(),
            })
            .map_err(|_| "The compatibility service worker has stopped.".to_string())
    }

    fn confirm_capsule_install(&mut self) -> Result<(), String> {
        let (source, source_name) = self
            .pending_vm_install
            .take()
            .ok_or_else(|| "Choose an installer before confirming a destination.".to_string())?;
        self.cached.vm_install_confirmation = None;
        self.commands
            .send(WorkerCommand::InstallInCapsule {
                source,
                source_name,
            })
            .map_err(|_| "The compatibility service worker has stopped.".to_string())
    }

    fn confirm_vm_install(&mut self) -> Result<(), String> {
        let (source, source_name) = self
            .pending_vm_install
            .take()
            .ok_or_else(|| "Choose an installer before confirming a destination.".to_string())?;
        self.cached.vm_install_confirmation = None;
        self.commands
            .send(WorkerCommand::InstallInVm {
                source,
                source_name,
            })
            .map_err(|_| "The compatibility service worker has stopped.".to_string())
    }

    fn publish_vm_candidate(&mut self, entry_id: &str) -> Result<(), String> {
        self.commands
            .send(WorkerCommand::PublishVmCandidate(entry_id.to_string()))
            .map_err(|_| "The compatibility service worker has stopped.".to_string())
    }

    fn vendor_uninstall_vm(&mut self, entry_id: &str) -> Result<(), String> {
        self.commands
            .send(WorkerCommand::VendorUninstallVm(entry_id.to_string()))
            .map_err(|_| "The compatibility service worker has stopped.".to_string())
    }

    fn retry_install(&mut self, capsule_id: &str) -> Result<(), String> {
        self.commands
            .send(WorkerCommand::RetryInstall(capsule_id.to_string()))
            .map_err(|_| "The compatibility service worker has stopped.".to_string())
    }

    fn remove_launcher(&mut self, app_id: &str) -> Result<(), String> {
        self.commands
            .send(WorkerCommand::RemoveLauncher(app_id.to_string()))
            .map_err(|_| "The compatibility service worker has stopped.".to_string())
    }

    fn vendor_uninstall(&mut self, capsule_id: &str) -> Result<(), String> {
        self.commands
            .send(WorkerCommand::VendorUninstall(capsule_id.to_string()))
            .map_err(|_| "The compatibility service worker has stopped.".to_string())
    }

    fn delete_data(&mut self, capsule_id: &str) -> Result<(), String> {
        self.commands
            .send(WorkerCommand::DeleteData(capsule_id.to_string()))
            .map_err(|_| "The compatibility service worker has stopped.".to_string())
    }

    fn take_notice(&mut self) -> Option<String> {
        self.drain();
        self.notice.take()
    }

    fn set_route_override(&mut self, _app_id: &str, _route: Route) -> Result<(), String> {
        Err(
            "Route changes need a trusted qOS policy action; this session API is launch-only."
                .into(),
        )
    }

    fn reset_route_override(&mut self, _app_id: &str) -> Result<(), String> {
        Err(
            "Route changes need a trusted qOS policy action; this session API is launch-only."
                .into(),
        )
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "the worker serializes one bounded protocol stream and its exhaustive commands"
)]
fn worker(
    mut client: SessionClient,
    commands: &Receiver<WorkerCommand>,
    events: &Sender<WorkerEvent>,
) {
    while let Ok(command) = commands.recv() {
        match command {
            WorkerCommand::Refresh => {
                // A setup surface can be closed while Windows keeps working.
                // Refreshing the server-owned singleton status before listing
                // prevents discovery rows from being permanently stale; the
                // client never supplies an operation ID.
                match client.request(&ProtocolRequest::RefreshVmInstallation {
                    version: PROTOCOL_VERSION,
                }) {
                    Ok(ProtocolResponse::VmInstallationRefreshed { .. }) => {}
                    Ok(ProtocolResponse::Error { error }) => {
                        let _ = events.send(WorkerEvent::Notice(error.message));
                    }
                    Ok(_) => {
                        let _ = events.send(WorkerEvent::Notice(
                            "The compatibility service returned an invalid Windows setup refresh."
                                .into(),
                        ));
                    }
                    Err(error) => {
                        let _ = events.send(WorkerEvent::Notice(error.to_string()));
                    }
                }
                match client.request(&ProtocolRequest::RefreshVmUninstallation {
                    version: PROTOCOL_VERSION,
                }) {
                    Ok(ProtocolResponse::VmUninstallationRefreshed { .. }) => {}
                    Ok(ProtocolResponse::Error { error }) => {
                        let _ = events.send(WorkerEvent::Notice(error.message));
                    }
                    Ok(_) => {
                        let _ = events.send(WorkerEvent::Notice(
                            "The compatibility service returned an invalid Windows uninstall refresh."
                                .into(),
                        ));
                    }
                    Err(error) => {
                        let _ = events.send(WorkerEvent::Notice(error.to_string()));
                    }
                }
                let event = match client.request(&ProtocolRequest::List {
                    version: PROTOCOL_VERSION,
                }) {
                    Ok(ProtocolResponse::Listed {
                        applications,
                        installations,
                        vm_availability,
                        vm_installation,
                        vm_uninstallation,
                    }) => WorkerEvent::Listed {
                        applications,
                        installations,
                        vm_availability,
                        vm_installation,
                        vm_uninstallation,
                    },
                    Ok(ProtocolResponse::Error { error }) => {
                        WorkerEvent::Unavailable(error.message)
                    }
                    Ok(_) => WorkerEvent::Unavailable(
                        "The compatibility service returned an invalid list response.".into(),
                    ),
                    Err(error) => WorkerEvent::Unavailable(error.to_string()),
                };
                if events.send(event).is_err() {
                    return;
                }
            }
            WorkerCommand::Launch(entry_id) => {
                let event = match client.request(&ProtocolRequest::Launch {
                    version: PROTOCOL_VERSION,
                    entry_id,
                }) {
                    Ok(ProtocolResponse::Launched { receipt }) if receipt.success => {
                        WorkerEvent::Notice("The Windows application exited normally.".into())
                    }
                    Ok(ProtocolResponse::Launched { receipt }) => WorkerEvent::Notice(format!(
                        "The Windows application exited with status {}.",
                        receipt
                            .exit_code
                            .map_or_else(|| "unknown".to_string(), |code| code.to_string())
                    )),
                    Ok(ProtocolResponse::Error { error }) => WorkerEvent::Notice(error.message),
                    Ok(_) => WorkerEvent::Unavailable(
                        "The compatibility service returned an invalid launch response.".into(),
                    ),
                    Err(error) => WorkerEvent::Unavailable(error.to_string()),
                };
                if events.send(event).is_err() {
                    return;
                }
            }
            WorkerCommand::LaunchVmFallback(entry_id) => {
                let event = match client.request(&ProtocolRequest::LaunchVmFallback {
                    version: PROTOCOL_VERSION,
                    entry_id,
                }) {
                    Ok(ProtocolResponse::Launched { receipt }) if receipt.success => {
                        WorkerEvent::Notice(
                            "The application exited from the managed Windows fallback.".into(),
                        )
                    }
                    Ok(ProtocolResponse::Launched { receipt }) => WorkerEvent::Notice(format!(
                        "The Windows fallback application exited with status {}.",
                        receipt
                            .exit_code
                            .map_or_else(|| "unknown".to_string(), |code| code.to_string())
                    )),
                    Ok(ProtocolResponse::Error { error }) => WorkerEvent::Notice(error.message),
                    Ok(_) => WorkerEvent::Unavailable(
                        "The compatibility service returned an invalid VM launch response.".into(),
                    ),
                    Err(error) => WorkerEvent::Unavailable(error.to_string()),
                };
                if events.send(event).is_err() {
                    return;
                }
            }
            WorkerCommand::Install {
                source,
                source_name,
            } => {
                let event = match client.request_with_file(
                    &ProtocolRequest::Install {
                        version: PROTOCOL_VERSION,
                        source_name: source_name.clone(),
                    },
                    &source,
                ) {
                    Ok(ProtocolResponse::VmInstallConfirmation { confirmation }) => {
                        WorkerEvent::VmInstallConfirmation {
                            source,
                            source_name,
                            confirmation,
                        }
                    }
                    Ok(ProtocolResponse::Installed { receipt }) => {
                        WorkerEvent::Notice(receipt.detail)
                    }
                    Ok(ProtocolResponse::Error { error }) => WorkerEvent::Notice(error.message),
                    Ok(_) => WorkerEvent::Unavailable(
                        "The compatibility service returned an invalid install response.".into(),
                    ),
                    Err(error) => WorkerEvent::Unavailable(error.to_string()),
                };
                let needs_confirmation = matches!(event, WorkerEvent::VmInstallConfirmation { .. });
                if events.send(event).is_err() {
                    return;
                }
                if needs_confirmation {
                    continue;
                }
                let refresh = match client.request(&ProtocolRequest::List {
                    version: PROTOCOL_VERSION,
                }) {
                    Ok(ProtocolResponse::Listed {
                        applications,
                        installations,
                        vm_availability,
                        vm_installation,
                        vm_uninstallation,
                    }) => WorkerEvent::Listed {
                        applications,
                        installations,
                        vm_availability,
                        vm_installation,
                        vm_uninstallation,
                    },
                    Ok(ProtocolResponse::Error { error }) => {
                        WorkerEvent::Unavailable(error.message)
                    }
                    Ok(_) => WorkerEvent::Unavailable(
                        "The compatibility service returned an invalid list response.".into(),
                    ),
                    Err(error) => WorkerEvent::Unavailable(error.to_string()),
                };
                if events.send(refresh).is_err() {
                    return;
                }
            }
            WorkerCommand::InstallInCapsule {
                source,
                source_name,
            } => {
                install_destination_request(
                    &mut client,
                    events,
                    &ProtocolRequest::InstallInCapsule {
                        version: PROTOCOL_VERSION,
                        source_name,
                    },
                    &source,
                    "Private capsule setup started.",
                );
            }
            WorkerCommand::InstallInVm {
                source,
                source_name,
            } => {
                install_destination_request(
                    &mut client,
                    events,
                    &ProtocolRequest::InstallInVm {
                        version: PROTOCOL_VERSION,
                        source_name,
                    },
                    &source,
                    "Windows setup finished or is still running in its integrated app surface.",
                );
            }
            WorkerCommand::PublishVmCandidate(entry_id) => {
                lifecycle_request(
                    &mut client,
                    events,
                    &ProtocolRequest::PublishVmCandidate {
                        version: PROTOCOL_VERSION,
                        entry_id,
                    },
                    "Application added to Windows Apps.",
                );
            }
            WorkerCommand::RetryInstall(capsule_id) => {
                lifecycle_request(
                    &mut client,
                    events,
                    &ProtocolRequest::RetryInstall {
                        version: PROTOCOL_VERSION,
                        capsule_id,
                    },
                    "Retry finished",
                );
            }
            WorkerCommand::RemoveLauncher(entry_id) => {
                lifecycle_request(
                    &mut client,
                    events,
                    &ProtocolRequest::RemoveLauncher {
                        version: PROTOCOL_VERSION,
                        entry_id,
                    },
                    "Launcher removed; the installed application and its data were kept.",
                );
            }
            WorkerCommand::VendorUninstall(capsule_id) => {
                lifecycle_request(
                    &mut client,
                    events,
                    &ProtocolRequest::VendorUninstall {
                        version: PROTOCOL_VERSION,
                        capsule_id,
                    },
                    "Vendor uninstall finished; private leftover data was kept.",
                );
            }
            WorkerCommand::VendorUninstallVm(entry_id) => {
                lifecycle_request(
                    &mut client,
                    events,
                    &ProtocolRequest::VendorUninstallVm {
                        version: PROTOCOL_VERSION,
                        entry_id,
                    },
                    "Windows uninstall finished; the published application was removed.",
                );
            }
            WorkerCommand::DeleteData(capsule_id) => {
                lifecycle_request(
                    &mut client,
                    events,
                    &ProtocolRequest::DeleteData {
                        version: PROTOCOL_VERSION,
                        capsule_id,
                    },
                    "Application and all of its private capsule data were deleted.",
                );
            }
        }
    }
}

fn install_destination_request(
    client: &mut SessionClient,
    events: &Sender<WorkerEvent>,
    request: &ProtocolRequest,
    source: &File,
    success: &str,
) {
    let event = match client.request_with_file(request, source) {
        Ok(ProtocolResponse::Installed { receipt }) => WorkerEvent::Notice(receipt.detail),
        Ok(ProtocolResponse::VmInstallStarted { installation }) => {
            WorkerEvent::Notice(installation.detail)
        }
        Ok(ProtocolResponse::Error { error }) => WorkerEvent::Notice(error.message),
        Ok(_) => WorkerEvent::Unavailable(
            "The compatibility service returned an invalid installer response.".into(),
        ),
        Err(error) => WorkerEvent::Unavailable(error.to_string()),
    };
    if events.send(event).is_err() {
        return;
    }
    lifecycle_request(
        client,
        events,
        &ProtocolRequest::List {
            version: PROTOCOL_VERSION,
        },
        success,
    );
}

fn lifecycle_request(
    client: &mut SessionClient,
    events: &Sender<WorkerEvent>,
    request: &ProtocolRequest,
    success: &str,
) {
    let event = match client.request(request) {
        Ok(ProtocolResponse::Listed {
            applications,
            installations,
            vm_availability,
            vm_installation,
            vm_uninstallation,
        }) => WorkerEvent::Listed {
            applications,
            installations,
            vm_availability,
            vm_installation,
            vm_uninstallation,
        },
        Ok(
            ProtocolResponse::Retried { receipt } | ProtocolResponse::VendorUninstalled { receipt },
        ) => WorkerEvent::Notice(receipt.detail),
        Ok(ProtocolResponse::LauncherRemoved { .. } | ProtocolResponse::DataDeleted { .. }) => {
            WorkerEvent::Notice(success.to_string())
        }
        Ok(ProtocolResponse::VmVendorUninstallation { uninstallation }) => {
            WorkerEvent::Notice(uninstallation.detail)
        }
        Ok(ProtocolResponse::Error { error }) => WorkerEvent::Notice(error.message),
        Ok(_) => WorkerEvent::Unavailable(
            "The compatibility service returned an invalid management response.".into(),
        ),
        Err(error) => WorkerEvent::Unavailable(error.to_string()),
    };
    if events.send(event).is_err() {
        return;
    }
    let refresh = match client.request(&ProtocolRequest::List {
        version: PROTOCOL_VERSION,
    }) {
        Ok(ProtocolResponse::Listed {
            applications,
            installations,
            vm_availability,
            vm_installation,
            vm_uninstallation,
        }) => WorkerEvent::Listed {
            applications,
            installations,
            vm_availability,
            vm_installation,
            vm_uninstallation,
        },
        Ok(ProtocolResponse::Error { error }) => WorkerEvent::Unavailable(error.message),
        Ok(_) => WorkerEvent::Unavailable(
            "The compatibility service returned an invalid list response.".into(),
        ),
        Err(error) => WorkerEvent::Unavailable(error.to_string()),
    };
    let _ = events.send(refresh);
}

fn unavailable_catalogue(detail: &str) -> Catalogue {
    Catalogue {
        apps: Vec::new(),
        vm: ApplianceState {
            ready: false,
            detail: detail.to_string(),
            running_apps: 0,
        },
        vm_install_confirmation: None,
    }
}

fn catalogue_from_managed(
    applications: Vec<ManagedApplication>,
    installations: &[ManagedInstallation],
    vm_availability: VmAvailability,
    vm_installation: Option<VmInstallation>,
    vm_uninstallation: Option<VmUninstallation>,
) -> Catalogue {
    let mut apps = applications
        .into_iter()
        .map(|application| {
            let installation = installations
                .iter()
                .find(|item| application.capsule_id.as_deref() == Some(item.capsule_id.as_str()));
            app_from_managed(application, installation, vm_uninstallation.as_ref())
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
    Catalogue {
        apps,
        vm: ApplianceState {
            ready: vm_availability.available,
            detail: vm_availability.detail,
            running_apps: 0,
        },
        vm_install_confirmation: None,
    }
}

/// A discovery row is deliberately not a launcher. It exists only while the
/// privileged delegate retains the matching installer operation, and the UI
/// can submit its opaque ID to publish the guest-selected application.
fn app_from_vm_candidate(candidate: &compat_service::VmInstallCandidate) -> WindowsApp {
    WindowsApp {
        id: candidate.entry_id.clone(),
        name: candidate.display_name.clone(),
        publisher: candidate
            .publisher
            .clone()
            .unwrap_or_else(|| "Windows installer discovery".into()),
        route: Route::Vm,
        recommended_route: Route::Vm,
        runner: "Windows managed installer".into(),
        capsule: "Shared Windows appliance".into(),
        prefix: "Not published yet · no host capsule".into(),
        catalogue_source: "Authenticated Windows setup discovery".into(),
        confidence: Confidence::Verified,
        reason: candidate.version.as_ref().map_or_else(
            || "Windows setup discovered this application. Add it to Windows Apps to create a managed guest launcher.".into(),
            |version| format!("Windows setup discovered version {version}. Add it to Windows Apps to create a managed guest launcher."),
        ),
        status: AppStatus::Ready,
        grants: Vec::new(),
        fallback_detail: "This is an installer discovery, not an executable host entry.".into(),
        launch_available: false,
        launch_unavailable_reason: Some("Add this discovered application to Windows Apps first.".into()),
        route_override_available: false,
        route_override_unavailable_reason: None,
        can_retry_install: false,
        can_remove_launcher: false,
        can_vendor_uninstall: false,
        can_delete_data: false,
        can_publish_vm_candidate: true,
        installation_detail: Some("Discovered by the active Windows setup transaction.".into()),
        vm_fallback_available: false,
        vm_fallback_unavailable_reason: Some("This discovered application has not been published yet.".into()),
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
    let mut grants = vec![
        Grant {
            label: "Read-only files".into(),
            detail: format!("{} granted location(s)", explanation.access.files_read_only),
            enabled: explanation.access.files_read_only > 0,
        },
        Grant {
            label: "Writable files".into(),
            detail: format!(
                "{} granted location(s)",
                explanation.access.files_read_write
            ),
            enabled: explanation.access.files_read_write > 0,
        },
        Grant {
            label: "Network".into(),
            detail: explanation.access.network.clone(),
            enabled: explanation.access.network != "none",
        },
        Grant {
            label: "Microphone".into(),
            detail: if explanation.access.microphone {
                "Granted"
            } else {
                "Not granted"
            }
            .into(),
            enabled: explanation.access.microphone,
        },
    ];
    grants.push(Grant {
        label: "Audio output".into(),
        detail: application
            .audio_unavailable_reason
            .clone()
            .unwrap_or_else(|| {
                if application.audio_output_available {
                    "Available"
                } else {
                    "Unavailable"
                }
                .into()
            }),
        enabled: application.audio_output_available,
    });
    let mut disabled = Vec::new();
    if let Some(reason) = &application.vm_unavailable_reason {
        disabled.push(format!("Windows VM: {reason}"));
    }
    if let Some(reason) = &application.audio_unavailable_reason {
        disabled.push(format!("Audio output: {reason}"));
    }
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
        prefix: if route == Route::Vm {
            "Guest-managed application data · no host capsule".into()
        } else {
            "Private capsule · host path hidden".into()
        },
        catalogue_source: if route == Route::Vm {
            "Authenticated Windows guest catalogue".into()
        } else {
            explanation.catalogue_record.as_deref().map_or_else(
                || "qOS safe default".to_string(),
                |id| format!("qOS recipe · {id}"),
            )
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
                        application
                            .installation_status
                            .map_or(AppStatus::Ready, app_status)
                    },
                    |_| AppStatus::Failed,
                )
            },
            |_| AppStatus::Installing,
        ),
        grants,
        fallback_detail: active_vm_uninstall
            .or(failed_vm_uninstall)
            .map_or_else(|| disabled.join(" "), |status| status.detail.clone()),
        launch_available,
        launch_unavailable_reason,
        route_override_available: false,
        route_override_unavailable_reason: Some(
            "Route changes need a trusted qOS policy action and are disabled in this session API."
                .into(),
        ),
        can_retry_install: installation.is_some_and(|item| item.can_retry),
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
        prefix: "Private capsule · host path hidden".into(),
        catalogue_source: "Portal import".into(),
        confidence: Confidence::Heuristic,
        reason: "qOS retained this setup transaction so it can be retried or removed safely."
            .into(),
        status: app_status(installation.status),
        grants: Vec::new(),
        fallback_detail: installation.detail.clone(),
        launch_available: false,
        launch_unavailable_reason: Some(
            "No application launcher is currently available for this installation.".into(),
        ),
        route_override_available: false,
        route_override_unavailable_reason: None,
        can_retry_install: installation.can_retry,
        can_remove_launcher: false,
        can_vendor_uninstall: installation.can_vendor_uninstall,
        can_delete_data: installation.can_delete_data,
        can_publish_vm_candidate: false,
        installation_detail: Some(installation.detail.clone()),
        vm_fallback_available: false,
        vm_fallback_unavailable_reason: Some(
            "No published guest application matches this retained setup.".into(),
        ),
    }
}

fn route_from_kind(route: RouteKind) -> Route {
    match route {
        RouteKind::Wine => Route::Wine,
        RouteKind::Proton => Route::Proton,
        RouteKind::Vm => Route::Vm,
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

    fn managed_vm() -> ManagedApplication {
        ManagedApplication {
            id: "windows-editor".into(),
            app_id: "dev.qos.windows.editor".into(),
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
            audio_output_available: false,
            audio_unavailable_reason: Some("Playback-only proxy unavailable".into()),
            installation_status: None,
            remove_launcher_available: false,
            vendor_uninstall_available: false,
        }
    }

    #[test]
    fn vm_and_audio_disabled_reasons_are_explicit_in_the_live_model() {
        let app = app_from_managed(managed_vm(), None, None);
        assert!(!app.launch_available);
        assert_eq!(
            app.launch_unavailable_reason.as_deref(),
            Some("Guest bridge unavailable")
        );
        assert!(app.fallback_detail.contains("Guest bridge unavailable"));
        assert!(
            app.fallback_detail
                .contains("Playback-only proxy unavailable")
        );
        assert!(
            app.grants
                .iter()
                .any(|grant| grant.label == "Audio output" && !grant.enabled)
        );
        assert_eq!(app.capsule, "Shared Windows appliance");
        assert!(app.prefix.contains("no host capsule"));
        assert_eq!(
            app.catalogue_source,
            "Authenticated Windows guest catalogue"
        );
    }

    #[test]
    fn ready_vm_is_reported_even_without_published_application_rows() {
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

    #[test]
    fn active_vm_discoveries_are_publish_only_rows() {
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

    #[test]
    fn private_capsules_do_not_offer_an_unmapped_vm_fallback() {
        let mut application = managed_vm();
        application.capsule_id = Some("private-capsule".into());
        application.explanation.route = RouteKind::Wine;
        application.vm_launch_available = false;
        application.vm_unavailable_reason = Some(
            "Open in Windows VM is available only for published Windows guest applications.".into(),
        );

        let app = app_from_managed(application, None, None);

        assert!(!app.vm_fallback_available);
        assert!(
            app.vm_fallback_unavailable_reason
                .as_deref()
                .is_some_and(|reason| reason.contains("published Windows guest"))
        );
    }

    #[test]
    fn running_vm_uninstall_keeps_the_launcher_visible_and_disables_actions() {
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

        assert_eq!(app.status, AppStatus::Installing);
        assert!(!app.launch_available);
        assert!(!app.can_vendor_uninstall);
        assert!(app.fallback_detail.contains("uninstall is running"));
    }

    #[test]
    fn failed_vm_uninstall_is_visible_and_can_be_retried() {
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

    #[test]
    fn invalid_or_disconnected_response_clears_the_live_cache() {
        let (command_tx, _command_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();
        let mut sample = SampleProvider::new();
        let mut provider = LiveProvider {
            cached: sample.catalogue(),
            commands: command_tx,
            events: event_rx,
            refresh_pending: true,
            next_refresh: Instant::now() + REFRESH_INTERVAL,
            notice: None,
            pending_vm_install: None,
        };
        event_tx
            .send(WorkerEvent::Unavailable("invalid response".into()))
            .unwrap();
        let catalogue = provider.catalogue();
        assert!(catalogue.apps.is_empty());
        assert_eq!(catalogue.vm.detail, "invalid response");
    }
}
