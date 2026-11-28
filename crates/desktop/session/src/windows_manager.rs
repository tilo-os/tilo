//! This computer's Windows programs as the device's `windows-apps` resource reads them: the
//! compatibility service's catalogue, the managed appliance's owner status and commands, and
//! whether the layer itself is installed.
//!
//! The session's system service serves the reading and its actions through the [`SessionManager`]
//! the compatibility service publishes as it starts ([`compat_service::ManagerOut`]). The owner
//! helper is this session's own: its status is read from the files the owner leaves, and every
//! command that changes the appliance goes through the same privileged helper the Windows Apps
//! manager used to call.

use std::fs::File;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tilo_system_resources::windows_apps::{
    Access, AppStatus, Appliance, AppliancePhase, Confidence, EndedProgram, License,
    PendingInstall, PowerState, Route, SharedSession, Source, TemporaryConnection, WindowsApp,
    WindowsApps, WindowsAppsAction, WindowsProblem, WindowsProblemCode,
};

use compat_service::{RouteKind, SessionError};

/// The owner's helper: the one program that reads and changes the appliance.
const OWNER: &str = "/usr/libexec/tilo-windows-owner";
/// The console the owner opens on the recovery desktop.
const RECOVERY_CONSOLE: &str = "/usr/libexec/tilo-windows-recovery-console";
/// How long the owner's status is served before it is read again.
const STATUS_FRESH: Duration = Duration::from_secs(2);
/// How long a program that ended is said, after it ended.
const ENDED_SAID: Duration = Duration::from_secs(30);

/// The Windows programs and appliance as this session reads and drives them.
pub struct SessionWindows {
    manager: compat_service::ManagerOut,
    appliance: ApplianceOwner,
    /// The last program that ended, with when it did.
    ended: Mutex<Option<(Instant, EndedProgram)>>,
}

impl SessionWindows {
    /// The manager over the compatibility service `manager` and the appliance on this computer.
    #[must_use]
    pub fn new(manager: compat_service::ManagerOut, wake: Option<tilo_service::Wake>) -> Arc<Self> {
        Arc::new(Self {
            manager,
            appliance: ApplianceOwner::new(wake),
            ended: Mutex::default(),
        })
    }
}

impl tilo_system_service::windows_apps::WindowsManager for SessionWindows {
    fn reading(&self, workspace: &str) -> WindowsApps {
        let support = tilo_system_service::windows_compat::reading();
        let mut appliance = self.appliance.reading();
        let Some(manager) = self.manager.get() else {
            return WindowsApps {
                summary: "This computer's Windows compatibility service is not running.".to_owned(),
                support,
                appliance,
                ..WindowsApps::default()
            };
        };
        let list = match manager.list(workspace) {
            Ok(list) => list,
            Err(error) => {
                return WindowsApps {
                    summary: error.to_string(),
                    support,
                    appliance,
                    ..WindowsApps::default()
                };
            }
        };
        appliance.shared_session = shared_session(&list);
        if let Some(end) = &list.ended {
            *lock(&self.ended) = Some((
                Instant::now(),
                EndedProgram {
                    program: end.program.clone(),
                    name: end.name.clone(),
                    success: end.success,
                    exit_code: end.exit_code.map(i64::from),
                    error: end.error.clone(),
                    said: end.said.clone(),
                },
            ));
        }
        let ended = lock(&self.ended)
            .as_ref()
            .filter(|(at, _)| at.elapsed() < ENDED_SAID)
            .map(|(_, end)| end.clone());
        let installation = list.installation.as_ref();
        let mut apps: Vec<WindowsApp> = list
            .applications
            .iter()
            .map(|application| {
                let installation = list.installations.iter().find(|item| {
                    application.capsule_id.as_deref() == Some(item.capsule_id.as_str())
                });
                app_of(application, installation)
            })
            .collect();
        for installation in &list.installations {
            if !apps
                .iter()
                .any(|application| application.capsule == installation.capsule_id)
            {
                apps.push(app_of_installation(installation));
            }
        }
        if let Some(installation) = installation {
            apps.extend(installation.candidates.iter().map(app_of_candidate));
        }
        let pending_install = list
            .pending_install
            .as_ref()
            .map(|confirmation| PendingInstall {
                display_name: confirmation.display_name.clone(),
                detail: confirmation.detail.clone(),
                recommended_route: route_of(confirmation.recommended_route),
            });
        WindowsApps {
            serving: true,
            summary: list.vm_availability.detail.clone(),
            apps,
            appliance,
            pending_install,
            support,
            ended,
            pending: Vec::new(),
            problem: None,
        }
    }

    fn change(&self, workspace: &str, action: &WindowsAppsAction) -> Result<(), WindowsProblem> {
        if let Some(result) = self.appliance_change(action) {
            return result;
        }
        self.layer_change(workspace, action)
    }
}

/// Why the layer refused a change, as the device names it, with its own words kept.
fn refused(error: &SessionError) -> WindowsProblem {
    let code = match &error {
        SessionError::UnknownEntry => WindowsProblemCode::AppGone,
        SessionError::NotRunning | SessionError::VmInstallConfirmationExpired => {
            WindowsProblemCode::OperationGone
        }
        SessionError::AlreadyRunning => WindowsProblemCode::Busy,
        _ => WindowsProblemCode::Command,
    };
    WindowsProblem::new(code, error.to_string())
}

/// Why a program or a file did not open: what is no longer there, or why it did not open.
fn not_opened(error: &SessionError) -> WindowsProblem {
    match &error {
        SessionError::UnknownEntry => {
            WindowsProblem::new(WindowsProblemCode::AppGone, error.to_string())
        }
        _ => opened(&error.to_string()),
    }
}

/// Hand Windows installation media to the appliance's owner.
fn import_iso(path: Option<&str>) -> Result<(), WindowsProblem> {
    let Some(path) = path else {
        return Err(WindowsProblem::new(
            WindowsProblemCode::Open,
            "no Windows ISO was chosen",
        ));
    };
    let source =
        File::open(path).map_err(|error| opened(&format!("could not open {path}: {error}")))?;
    ApplianceOwner::import_iso(source)
        .map_err(|detail| WindowsProblem::new(WindowsProblemCode::Command, detail))
}

/// What the device said as it could not open something.
fn opened(detail: &str) -> WindowsProblem {
    WindowsProblem::new(WindowsProblemCode::Open, detail)
}

/// Whether the appliance's power state takes `action`.
fn power_applies(action: &str, state: PowerState) -> bool {
    matches!(
        (state, action),
        (PowerState::Running, "restart" | "suspend" | "shutdown")
            | (PowerState::Suspended | PowerState::Paused, "resume")
            | (PowerState::ShutDown, "start")
    )
}

impl SessionWindows {
    /// The actions that change the appliance itself rather than the programs; `None` for the
    /// ones the compatibility layer carries out.
    fn appliance_change(&self, action: &WindowsAppsAction) -> Option<Result<(), WindowsProblem>> {
        Some(match action {
            WindowsAppsAction::Recovery => self.recovery(),
            WindowsAppsAction::Start => self.lifecycle("start"),
            WindowsAppsAction::Restart => self.lifecycle("restart"),
            WindowsAppsAction::Suspend => self.lifecycle("suspend"),
            WindowsAppsAction::Resume => self.lifecycle("resume"),
            WindowsAppsAction::Shutdown => self.lifecycle("shutdown"),
            WindowsAppsAction::TemporaryNetwork => self.egress_open("activation"),
            WindowsAppsAction::UpdateNetwork => self.egress_open("windows-update"),
            WindowsAppsAction::RevokeNetwork => self.egress_revoke(),
            _ => return None,
        })
    }

    /// The actions the compatibility layer carries out: opening, stopping and installing
    /// programs, and what a setup does.
    fn layer_change(
        &self,
        workspace: &str,
        action: &WindowsAppsAction,
    ) -> Result<(), WindowsProblem> {
        match action {
            WindowsAppsAction::Install { path } => self.install(workspace, path.as_deref()),
            WindowsAppsAction::ImportIso { path } => import_iso(path.as_deref()),
            WindowsAppsAction::InstallPrivateCapsule => self
                .manager()?
                .install_in_capsule(workspace)
                .map_err(|error| refused(&error)),
            WindowsAppsAction::InstallWindowsAppliance => self
                .manager()?
                .install_in_vm(workspace)
                .map_err(|error| refused(&error)),
            WindowsAppsAction::Launch { program } => self
                .manager()?
                .launch(workspace, program)
                .map_err(|error| not_opened(&error)),
            WindowsAppsAction::Stop { program } => self
                .manager()?
                .stop(workspace, program)
                .map_err(|error| refused(&error)),
            WindowsAppsAction::LaunchVmFallback { program } => self
                .manager()?
                .launch_vm(workspace, program, None)
                .map_err(|error| not_opened(&error)),
            WindowsAppsAction::OpenFile { program, path } => {
                self.open_file(workspace, program, path.as_deref())
            }
            WindowsAppsAction::Publish { program } => self
                .manager()?
                .publish(workspace, program)
                .map_err(|error| refused(&error)),
            WindowsAppsAction::RetryInstall { capsule } => self
                .manager()?
                .retry(workspace, capsule)
                .map_err(|error| refused(&error)),
            WindowsAppsAction::EscalateInstall { capsule } => self
                .manager()?
                .escalate(workspace, capsule)
                .map_err(|error| refused(&error)),
            WindowsAppsAction::RemoveLauncher { program } => self
                .manager()?
                .remove_launcher(program)
                .map_err(|error| refused(&error)),
            WindowsAppsAction::Uninstall { program } => self
                .manager()?
                .uninstall(workspace, program)
                .map_err(|error| refused(&error)),
            WindowsAppsAction::DeleteData { capsule } => self
                .manager()?
                .delete_data(capsule)
                .map_err(|error| refused(&error)),
            WindowsAppsAction::SetRoute { program, route } => self
                .manager()?
                .set_route(
                    workspace,
                    program,
                    route.map(|route| match route {
                        Route::Wine => RouteKind::Wine,
                        Route::Proton => RouteKind::Proton,
                        Route::Vm => RouteKind::Vm,
                    }),
                )
                .map_err(|error| refused(&error)),
            WindowsAppsAction::Recovery
            | WindowsAppsAction::Start
            | WindowsAppsAction::Restart
            | WindowsAppsAction::Suspend
            | WindowsAppsAction::Resume
            | WindowsAppsAction::Shutdown
            | WindowsAppsAction::TemporaryNetwork
            | WindowsAppsAction::UpdateNetwork
            | WindowsAppsAction::RevokeNetwork => Err(WindowsProblem::new(
                WindowsProblemCode::Command,
                "the action changes the appliance, not a program",
            )),
        }
    }

    /// Inspect an installer the person chose and set it up.
    fn install(&self, workspace: &str, path: Option<&str>) -> Result<(), WindowsProblem> {
        let Some(path) = path else {
            return Err(WindowsProblem::new(
                WindowsProblemCode::Open,
                "no installer was chosen",
            ));
        };
        let source =
            File::open(path).map_err(|error| opened(&format!("could not open {path}: {error}")))?;
        let name = std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                WindowsProblem::new(
                    WindowsProblemCode::Open,
                    "the installer's path has no file name",
                )
            })?
            .to_owned();
        self.manager()?
            .install(workspace, source, &name)
            .map(drop)
            .map_err(|error| refused(&error))
    }

    /// Open a program with a document, when one was chosen.
    fn open_file(
        &self,
        workspace: &str,
        program: &str,
        path: Option<&str>,
    ) -> Result<(), WindowsProblem> {
        let source = match path {
            Some(path) => Some(
                File::open(path)
                    .map_err(|error| opened(&format!("could not open {path}: {error}")))?,
            ),
            None => None,
        };
        self.manager()?
            .launch_vm(workspace, program, source)
            .map_err(|error| not_opened(&error))
    }

    fn manager(&self) -> Result<Arc<dyn compat_service::SessionManager>, WindowsProblem> {
        self.manager.get().ok_or_else(|| {
            WindowsProblem::new(
                WindowsProblemCode::Unmanageable,
                "This computer's Windows compatibility service is not running.",
            )
        })
    }

    /// Open the appliance's recovery desktop, once it is set up.
    fn recovery(&self) -> Result<(), WindowsProblem> {
        if !self.appliance.reading().recovery_available {
            return Err(WindowsProblem::new(
                WindowsProblemCode::RecoveryUnavailable,
                "",
            ));
        }
        self.appliance
            .recovery()
            .map_err(|detail| WindowsProblem::new(WindowsProblemCode::RecoveryOpen, detail))
    }

    /// Run one fixed lifecycle action, when the appliance's power state takes it.
    fn lifecycle(&self, action: &str) -> Result<(), WindowsProblem> {
        let appliance = self.appliance.reading();
        if !appliance.configured {
            return Err(WindowsProblem::new(WindowsProblemCode::SetUpPower, ""));
        }
        if !power_applies(action, appliance.power_state) {
            return Err(WindowsProblem::new(WindowsProblemCode::PowerState, ""));
        }
        ApplianceOwner::lifecycle(action)
            .map_err(|detail| WindowsProblem::new(WindowsProblemCode::Command, detail))
    }

    /// Connect the appliance for a while, once Windows runs.
    fn egress_open(&self, purpose: &str) -> Result<(), WindowsProblem> {
        let appliance = self.appliance.reading();
        if !appliance.configured {
            return Err(WindowsProblem::new(WindowsProblemCode::SetUpConnection, ""));
        }
        if appliance.power_state != PowerState::Running {
            return Err(WindowsProblem::new(WindowsProblemCode::NotRunning, ""));
        }
        ApplianceOwner::egress_open(purpose)
            .map_err(|detail| WindowsProblem::new(WindowsProblemCode::Command, detail))
    }

    /// End the temporary connection, when there is one.
    fn egress_revoke(&self) -> Result<(), WindowsProblem> {
        if self.appliance.reading().temporary_connection.is_none() {
            return Err(WindowsProblem::new(
                WindowsProblemCode::ConnectionNoLease,
                "",
            ));
        }
        ApplianceOwner::egress_revoke()
            .map_err(|detail| WindowsProblem::new(WindowsProblemCode::Command, detail))
    }
}
/// One program as the app's page shows it.
fn app_of(
    application: &compat_service::ManagedApplication,
    installation: Option<&compat_service::ManagedInstallation>,
) -> WindowsApp {
    let explanation = &application.explanation;
    let route = route_of(explanation.route);
    let recommended_route = route_of(application.recommended_route);
    let launch_available = route != Route::Vm || application.vm_launch_available;
    let portal_file_open_available = application.portal_file_open_available && launch_available;
    WindowsApp {
        id: application.id.clone(),
        name: application.display_name.clone(),
        publisher: application.publisher.clone(),
        route,
        recommended_route,
        runner: explanation.runner.clone(),
        capsule: if route == Route::Vm {
            "Shared Windows appliance".to_owned()
        } else {
            application
                .capsule_id
                .clone()
                .unwrap_or_else(|| "Unavailable private capsule".to_owned())
        },
        setup_name: installation.map(|item| item.display_name.clone()),
        source: if route == Route::Vm {
            Source::InstalledInWindows
        } else if explanation.catalogue_record.is_some() {
            Source::Catalogue
        } else {
            Source::Default
        },
        confidence: match explanation.match_reason {
            compat_service::MatchReason::DefaultWine => Confidence::Fallback,
            compat_service::MatchReason::Executable
            | compat_service::MatchReason::ProductPublisher => Confidence::Heuristic,
            _ => Confidence::Verified,
        },
        reason: explanation.summary.clone(),
        status: if application.running {
            AppStatus::Running
        } else {
            application
                .installation_status
                .map_or(AppStatus::Ready, status_of)
        },
        access: Access {
            files_read_only: explanation.access.files_read_only,
            files_read_write: explanation.access.files_read_write,
            network: explanation.access.network.clone(),
            microphone: explanation.access.microphone,
            sound: application.audio_output_available,
        },
        fallback_detail: installation
            .filter(|_| !launch_available)
            .map(|item| item.detail.clone())
            .unwrap_or_default(),
        launch_available: launch_available.into(),
        launch_unavailable_reason: (!launch_available)
            .then(|| application.vm_unavailable_reason.clone())
            .flatten(),
        portal_file_open_available: portal_file_open_available.into(),
        portal_file_open_unavailable_reason: application
            .portal_file_open_unavailable_reason
            .clone(),
        route_override_available: application.opens_with_choice_available.into(),
        route_override_unavailable_reason: application.opens_with_unavailable_reason.clone(),
        can_retry_install: installation.is_some_and(|item| item.can_retry).into(),
        can_escalate_install_to_vm: installation.is_some_and(escalates_to_vm).into(),
        can_remove_launcher: application.remove_launcher_available.into(),
        can_vendor_uninstall: (application.vendor_uninstall_available
            || installation.is_some_and(|item| item.can_vendor_uninstall))
        .into(),
        can_delete_data: installation.is_some_and(|item| item.can_delete_data).into(),
        can_publish_vm_candidate: false.into(),
        installation: installation.map(|item| item.detail.clone()),
        vm_fallback_available: application.vm_launch_available.into(),
        vm_fallback_unavailable_reason: application.vm_unavailable_reason.clone(),
    }
}

/// A setup kept only for its data, whose programs are gone.
fn app_of_installation(installation: &compat_service::ManagedInstallation) -> WindowsApp {
    WindowsApp {
        id: format!("installation-{}", installation.capsule_id),
        name: installation.display_name.clone(),
        publisher: "Imported application".to_owned(),
        route: route_of(installation.route),
        recommended_route: route_of(installation.route),
        runner: installation.runner.clone(),
        capsule: installation.capsule_id.clone(),
        setup_name: Some(installation.display_name.clone()),
        source: Source::AddedFromFile,
        confidence: Confidence::Heuristic,
        reason: "Tilo kept this setup so you can retry it or remove it safely.".to_owned(),
        status: status_of(installation.status),
        access: Access::default(),
        fallback_detail: installation.detail.clone(),
        launch_available: false.into(),
        launch_unavailable_reason: Some("This setup hasn't added an app to open yet.".to_owned()),
        portal_file_open_available: false.into(),
        portal_file_open_unavailable_reason: Some(
            "This setup hasn't added an app to open yet.".to_owned(),
        ),
        route_override_available: false.into(),
        route_override_unavailable_reason: None,
        can_retry_install: installation.can_retry.into(),
        can_escalate_install_to_vm: escalates_to_vm(installation).into(),
        can_remove_launcher: false.into(),
        can_vendor_uninstall: installation.can_vendor_uninstall.into(),
        can_delete_data: installation.can_delete_data.into(),
        can_publish_vm_candidate: false.into(),
        installation: Some(installation.detail.clone()),
        vm_fallback_available: false.into(),
        vm_fallback_unavailable_reason: Some(
            "This setup hasn't added an app to open yet.".to_owned(),
        ),
    }
}

/// An application Windows setup found, which the person has not added to this computer yet.
fn app_of_candidate(candidate: &compat_service::VmInstallCandidate) -> WindowsApp {
    WindowsApp {
        id: candidate.entry_id.clone(),
        name: candidate.display_name.clone(),
        publisher: candidate
            .publisher
            .clone()
            .unwrap_or_else(|| "Unknown publisher".to_owned()),
        route: Route::Vm,
        recommended_route: Route::Vm,
        runner: "Windows".to_owned(),
        capsule: "Shared Windows appliance".to_owned(),
        setup_name: None,
        source: Source::FoundBySetup,
        confidence: Confidence::Verified,
        reason: match &candidate.version {
            Some(version) => format!(
                "Windows setup found version {version} of this app. Add it to Windows Apps to open it from here."
            ),
            None => "Windows setup found this app. Add it to Windows Apps to open it from here."
                .to_owned(),
        },
        status: AppStatus::Ready,
        access: Access::default(),
        fallback_detail: String::new(),
        launch_available: false.into(),
        launch_unavailable_reason: Some("Add this app to Windows Apps first.".to_owned()),
        portal_file_open_available: false.into(),
        portal_file_open_unavailable_reason: Some("Add this app to Windows Apps first.".to_owned()),
        route_override_available: false.into(),
        route_override_unavailable_reason: None,
        can_retry_install: false.into(),
        can_escalate_install_to_vm: false.into(),
        can_remove_launcher: false.into(),
        can_vendor_uninstall: false.into(),
        can_delete_data: false.into(),
        can_publish_vm_candidate: true.into(),
        installation: Some("Found during Windows setup".to_owned()),
        vm_fallback_available: false.into(),
        vm_fallback_unavailable_reason: Some("Add this app to Windows Apps first.".to_owned()),
    }
}

/// Only a setup that did not finish needs the appliance: an installed one offers nothing to
/// continue there.
fn escalates_to_vm(installation: &compat_service::ManagedInstallation) -> bool {
    matches!(installation.route, RouteKind::Wine | RouteKind::Proton)
        && matches!(
            installation.status,
            compat_service::InstallationStatus::NeedsAttention
                | compat_service::InstallationStatus::Failed
        )
}

fn status_of(status: compat_service::InstallationStatus) -> AppStatus {
    use compat_service::InstallationStatus as S;
    match status {
        S::Installing | S::Uninstalling => AppStatus::Installing,
        S::Installed => AppStatus::Ready,
        S::NeedsAttention => AppStatus::NeedsAttention,
        S::Failed => AppStatus::Failed,
        S::Uninstalled => AppStatus::Uninstalled,
    }
}

fn route_of(route: RouteKind) -> Route {
    match route {
        RouteKind::Wine => Route::Wine,
        RouteKind::Proton => Route::Proton,
        RouteKind::Vm => Route::Vm,
    }
}

/// What the appliance's shared session is, as the manager's `list` says it.
fn shared_session(list: &compat_service::ManagerList) -> SharedSession {
    SharedSession {
        active: list.vm_availability.shared_session.active,
        grants: list
            .vm_availability
            .shared_session
            .effective_grants
            .iter()
            .map(|grant| match grant {
                compat_service::VmSharedSessionGrantKind::AudioPlayback => "audio-playback",
                compat_service::VmSharedSessionGrantKind::Clipboard => "clipboard",
                compat_service::VmSharedSessionGrantKind::Microphone => "microphone",
                compat_service::VmSharedSessionGrantKind::NetworkEgress => "network-egress",
                compat_service::VmSharedSessionGrantKind::PortalFilesReadOnly => {
                    "portal-files-read-only"
                }
                compat_service::VmSharedSessionGrantKind::PortalFilesReadWrite => {
                    "portal-files-read-write"
                }
            })
            .map(str::to_owned)
            .collect(),
        contributors: list
            .vm_availability
            .shared_session
            .contributors
            .iter()
            .map(|contributor| contributor.display_name.clone())
            .collect(),
    }
}

/// The owner's status, as the appliance page reads it, and the commands that change it.
struct ApplianceOwner {
    status: Mutex<(Option<Instant>, Appliance)>,
}

impl ApplianceOwner {
    fn new(wake: Option<tilo_service::Wake>) -> Self {
        let owner = Self {
            status: Mutex::new((None, Appliance::default())),
        };
        if let Some(wake) = wake {
            // The owner leaves its status in a file; reading it again needs no command. This
            // wakes the session's loop so a reading that moved is sent to the apps watching it.
            if let Err(error) = std::thread::Builder::new()
                .name("tilo-windows-status".to_owned())
                .spawn(move || {
                    loop {
                        std::thread::sleep(STATUS_FRESH);
                        wake();
                    }
                })
            {
                tracing::warn!(%error, "the thread that wakes the session as the owner's status moves does not start; the page reads it on each request instead");
            }
        }
        owner
    }

    fn reading(&self) -> Appliance {
        let mut status = self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if status.0.is_none_or(|at| at.elapsed() >= STATUS_FRESH) {
            status.1 = read_status();
            status.0 = Some(Instant::now());
        }
        status.1.clone()
    }

    fn import_iso(source: File) -> Result<(), String> {
        let mut command = Command::new("pkexec");
        command.arg(OWNER).arg("import-iso");
        command_with_stdin(command, source)?;
        Self::start_recovery()
    }

    fn recovery(&self) -> Result<(), String> {
        if !self.reading().recovery_available {
            return Err("This appliance has no recovery desktop to open.".to_owned());
        }
        Self::start_recovery()
    }

    fn lifecycle(action: &str) -> Result<(), String> {
        run_privileged([OWNER, "lifecycle", action])
    }

    fn egress_open(purpose: &str) -> Result<(), String> {
        run_privileged([OWNER, "egress-open", purpose, "600"])?;
        Self::start_recovery()
    }

    fn egress_revoke() -> Result<(), String> {
        run_privileged([OWNER, "egress-revoke"])
    }

    fn start_recovery() -> Result<(), String> {
        run_privileged([OWNER, "recovery-start"])?;
        Command::new(RECOVERY_CONSOLE)
            .arg("--owner-authorized")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("could not open the recovery desktop: {error}"))
    }
}

/// Run one fixed owner command through polkit.
fn run_privileged<const N: usize>(arguments: [&str; N]) -> Result<(), String> {
    let mut command = Command::new("pkexec");
    command.args(arguments);
    let output = command
        .output()
        .map_err(|error| format!("could not ask for permission: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if detail.is_empty() {
            "the Windows helper did not accept the command".to_owned()
        } else {
            detail
        })
    }
}

fn command_with_stdin(mut command: Command, source: File) -> Result<(), String> {
    let output = command
        .stdin(Stdio::from(source))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("could not ask for permission: {error}"))?;
    if output.status.success() {
        Ok(())
    } else {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        Err(if detail.is_empty() {
            "the Windows helper did not accept the image".to_owned()
        } else {
            detail
        })
    }
}

/// The cell's value, whatever a panicking caller left behind.
fn lock<T>(cell: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    cell.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Seconds since the Unix epoch, as the clock reads it.
fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default()
}

/// The appliance as the owner last left its status, read without asking for anything.
fn read_status() -> Appliance {
    let text = windows_vm::owner_status_json(
        std::path::Path::new("/var/lib/tilo/windows/session/state.json"),
        std::path::Path::new("/run/tilo/windows-egress/status.json"),
        0,
    );
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Appliance::default();
    };
    let phase = match value.get("phase").and_then(serde_json::Value::as_str) {
        Some("not_provisioned") => AppliancePhase::NotSetUp,
        Some("installing") => AppliancePhase::Installing,
        Some("awaiting_guest_bridge") => AppliancePhase::FinishingSetup,
        Some("ready") => AppliancePhase::Ready,
        Some("repair_required") => AppliancePhase::NeedsRecovery,
        _ => AppliancePhase::Unavailable,
    };
    let license = match value.get("license").and_then(serde_json::Value::as_str) {
        Some("digitally_activated") => License::Digital,
        Some("product_key_activated") => License::ProductKey,
        Some("organisation_managed") => License::Managed,
        Some("evaluation") => License::Evaluation,
        Some("unlicensed") => License::Unlicensed,
        _ => License::Unknown,
    };
    let power_state = match value.get("power_state").and_then(serde_json::Value::as_str) {
        Some("running") => PowerState::Running,
        Some("suspended") => PowerState::Suspended,
        Some("paused") => PowerState::Paused,
        Some("shut_down") => PowerState::ShutDown,
        _ => PowerState::Unknown,
    };
    let temporary_connection = value
        .get("temporary_egress")
        .filter(|connection| !connection.is_null())
        .and_then(|connection| {
            Some(TemporaryConnection {
                purpose: connection.get("purpose")?.as_str()?.to_owned(),
                expires_in_seconds: connection
                    .get("expires_at_unix")?
                    .as_u64()?
                    .saturating_sub(unix_now()),
            })
        });
    Appliance {
        configured: value
            .get("configured")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(!matches!(phase, AppliancePhase::NotSetUp)),
        phase,
        license,
        power_state,
        published_applications: value
            .get("published_applications")
            .and_then(serde_json::Value::as_u64)
            .map_or(0, |count| usize::try_from(count).unwrap_or(0)),
        recovery_available: value
            .get("recovery_available")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        temporary_connection,
        shared_session: SharedSession::default(),
        detail: value
            .get("detail")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned),
    }
}
