//! Provider boundary for the session compatibility service. No runner command
//! or host path is constructed in UI code.
use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::{Duration, Instant};

use compat_service::{
    ManagedApplication, MatchReason, PROTOCOL_VERSION, ProtocolRequest, ProtocolResponse,
    RouteKind, SessionClient,
};

use crate::model::{AppStatus, ApplianceState, Catalogue, Confidence, Grant, Route, WindowsApp};

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
        }
    }

    fn launch(&mut self, _app_id: &str) -> Result<(), String> {
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
                WindowsApp { id: "adobe-reader".into(), name: "Adobe Acrobat Reader".into(), publisher: "Adobe".into(), route: Route::Wine, recommended_route: Route::Wine, runner: "Wine 11.0 (qOS pinned)".into(), capsule: "Adobe Reader".into(), prefix: "capsules/adobe-reader".into(), catalogue_source: "qOS compatibility catalogue".into(), confidence: Confidence::Verified, reason: "Verified desktop-app recipe; isolated Wine capsule is the smallest compatible layer.".into(), status: AppStatus::Ready, grants: grants(&[("Downloads", "Read and write", true), ("Clipboard", "On demand", true), ("Camera", "Not granted", false)]), fallback_detail: "If it stops working, repair this capsule or open it in the managed Windows VM.".into(), launch_available: true, launch_unavailable_reason: None, route_override_available: true, route_override_unavailable_reason: None },
                WindowsApp { id: "cyberpunk-2077".into(), name: "Cyberpunk 2077".into(), publisher: "CD PROJEKT RED".into(), route: Route::Proton, recommended_route: Route::Proton, runner: "UMU-Proton 10.0-3".into(), capsule: "Steam · Cyberpunk 2077".into(), prefix: "games/cyberpunk-2077".into(), catalogue_source: "Steam + ProtonDB recipe".into(), confidence: Confidence::Community, reason: "Game route uses Steam Runtime and UMU-Proton, including the game's known compatibility fixups.".into(), status: AppStatus::Running, grants: grants(&[("Games library", "Read and write", true), ("Controller", "Connected devices", true), ("Microphone", "Not granted", false)]), fallback_detail: "The Windows VM is offered only after Proton repair options are exhausted.".into(), launch_available: true, launch_unavailable_reason: None, route_override_available: true, route_override_unavailable_reason: None },
                WindowsApp { id: "visual-studio".into(), name: "Visual Studio".into(), publisher: "Microsoft".into(), route: Route::Vm, recommended_route: Route::Vm, runner: "Windows 11 managed VM".into(), capsule: "Windows workspace".into(), prefix: "vm/windows-workspace".into(), catalogue_source: "qOS fallback policy".into(), confidence: Confidence::Fallback, reason: "Requires Windows services not safely reproduced in a Wine capsule; it opens as an integrated guest app.".into(), status: AppStatus::Ready, grants: grants(&[("Workspace exchange", "Selected files only", true), ("Clipboard", "On demand", true), ("USB devices", "Not granted", false)]), fallback_detail: "One shared VM is suspended when idle. Its desktop is used only for setup or recovery.".into(), launch_available: true, launch_unavailable_reason: None, route_override_available: true, route_override_unavailable_reason: None },
            ], vm: ApplianceState { ready: true, detail: "Managed Windows workspace · suspended when idle · 2 guest apps available".into(), running_apps: 0 } } }
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
}

enum WorkerEvent {
    Listed(Vec<ManagedApplication>),
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
        })
    }

    fn drain(&mut self) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                WorkerEvent::Listed(applications) => {
                    self.cached = catalogue_from_managed(applications);
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

fn worker(
    mut client: SessionClient,
    commands: &Receiver<WorkerCommand>,
    events: &Sender<WorkerEvent>,
) {
    while let Ok(command) = commands.recv() {
        match command {
            WorkerCommand::Refresh => {
                let event = match client.request(&ProtocolRequest::List {
                    version: PROTOCOL_VERSION,
                }) {
                    Ok(ProtocolResponse::Listed { applications }) => {
                        WorkerEvent::Listed(applications)
                    }
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
        }
    }
}

fn unavailable_catalogue(detail: &str) -> Catalogue {
    Catalogue {
        apps: Vec::new(),
        vm: ApplianceState {
            ready: false,
            detail: detail.to_string(),
            running_apps: 0,
        },
    }
}

fn catalogue_from_managed(applications: Vec<ManagedApplication>) -> Catalogue {
    let vm_reason = applications
        .iter()
        .find_map(|application| application.vm_unavailable_reason.clone())
        .unwrap_or_else(|| "The privileged Windows guest adapter is unavailable.".to_string());
    let vm_ready = applications
        .iter()
        .any(|application| application.vm_launch_available);
    Catalogue {
        apps: applications.into_iter().map(app_from_managed).collect(),
        vm: ApplianceState {
            ready: vm_ready,
            detail: if vm_ready {
                "The managed Windows appliance is ready.".to_string()
            } else {
                vm_reason
            },
            running_apps: 0,
        },
    }
}

fn app_from_managed(application: ManagedApplication) -> WindowsApp {
    let explanation = application.explanation;
    let route = match explanation.route {
        RouteKind::Wine => Route::Wine,
        RouteKind::Proton => Route::Proton,
        RouteKind::Vm => Route::Vm,
    };
    let launch_available = route != Route::Vm || application.vm_launch_available;
    let launch_unavailable_reason = (!launch_available)
        .then(|| application.vm_unavailable_reason.clone())
        .flatten();
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
        capsule: application.capsule_id,
        prefix: "Private capsule · host path hidden".into(),
        catalogue_source: explanation.catalogue_record.as_deref().map_or_else(
            || "qOS safe default".to_string(),
            |id| format!("qOS recipe · {id}"),
        ),
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
        status: AppStatus::Ready,
        grants,
        fallback_detail: disabled.join(" "),
        launch_available,
        launch_unavailable_reason,
        route_override_available: false,
        route_override_unavailable_reason: Some(
            "Route changes need a trusted qOS policy action and are disabled in this session API."
                .into(),
        ),
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
            capsule_id: "editor".into(),
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
        }
    }

    #[test]
    fn vm_and_audio_disabled_reasons_are_explicit_in_the_live_model() {
        let app = app_from_managed(managed_vm());
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
        };
        event_tx
            .send(WorkerEvent::Unavailable("invalid response".into()))
            .unwrap();
        let catalogue = provider.catalogue();
        assert!(catalogue.apps.is_empty());
        assert_eq!(catalogue.vm.detail, "invalid response");
    }
}
