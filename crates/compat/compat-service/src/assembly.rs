//! The compatibility service as a desktop session serves it: the parts this
//! crate assembles, and the little the session supplies itself.
//!
//! The session supplies only what it alone knows ([`SessionSide`]): which
//! launcher processes it started ([`TrustedLauncherRegistry`]), what each
//! workspace's sandbox grants ([`CapabilityAuthority`]), and the display each
//! launch is given ([`LaunchDisplays`]). Everything else is built here: the
//! shipped catalogue, the capsule store, the Bubblewrap executor and the
//! Windows appliance, which is looked for again on every request until it
//! exists.
//!
//! A computer without the Windows stack still serves the manager: the
//! catalogue is empty, a capsule launch is refused with
//! [`crate::PlanError::WindowsSupportNotInstalled`], and the appliance reports
//! itself unavailable with the reason.

use std::fs::File;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};

use compat_catalogue::Catalogue;
use windows_vm::{
    ApplianceConfig, ApplicationPresentation, GuestBridgeConfig, HttpsGuestBridge, LibvirtBackend,
    PortalDocumentStore, PortalRailCapabilityBroker, ProvisioningStore, WindowsAppliance,
};

use crate::{
    AudioIsolatedRailLauncher, AudioIsolatedSpiceLauncher, BwrapExecutor, CapabilityAuthority,
    CapsuleStore, CompatSessionHandle, EntryDirectory, LaunchDisplays, LaunchResourceBroker,
    LauncherAuthenticator, LiveVmDelegate, ManagedCompatibility, PortalRailPortalAuthority,
    RAIL_PRESENTER, Reopenings, RuntimeRegistry, SandboxExecutor, SharedSessionCapabilityBroker,
    ShippedCataloguePaths, SystemResourceBroker, SystemRuntimeRegistry, TrustedLauncherRegistry,
    VmAvailability, VmCandidatePublication, VmDelegate, VmInstallation, VmInstallerInput,
    VmInventory, VmRouteRequest, VmSharedSessionTracker, VmUninstallation, load_shipped_catalogue,
    spawn_session_service,
};

/// Where the one Windows appliance keeps its disk, firmware and bridge state.
const APPLIANCE_ROOT: &str = "/var/lib/tilo/windows";
/// Why a computer without an appliance offers none.
const NOT_SET_UP: &str = "The Windows appliance is not set up on this computer.";

/// What the session that serves the service supplies.
pub struct SessionSide {
    /// The socket the launcher hands the Windows Apps manager.
    pub socket: PathBuf,
    /// The `tilo-launcher` the session starts its apps through: the only
    /// executable whose processes may connect.
    pub launcher: PathBuf,
    /// The session's own process, the parent of every launcher it starts.
    pub parent_pid: u32,
    /// The launches the session started.
    pub launches: Arc<dyn TrustedLauncherRegistry>,
    /// What each workspace's sandbox grants, read at every request.
    pub sandboxes: Arc<dyn CapabilityAuthority>,
    /// The display each launch is given as it starts.
    pub displays: Arc<dyn LaunchDisplays>,
    /// Where the capsules (each Windows program's private prefix) are kept.
    pub capsules: PathBuf,
    /// The entries the service can open now, which the session reads as it reopens a
    /// saved tab: `None` until the service read them.
    pub entries: EntryDirectory,
    /// What the session asks as itself: a saved tab's program, opened again.
    pub reopenings: Reopenings,
    /// Where the service publishes the device's manager as it starts: the session's system
    /// service reads this computer's Windows programs through it.
    pub manager: crate::ManagerOut,
    /// Whether the desktop's person administers the computer
    /// ([`crate::account_administers_computer`]): only they choose how a program opens.
    pub administrator: Arc<dyn Fn() -> bool + Send + Sync>,
}

/// How a session's service is put together: [`Assembly::system`] on a Tilo
/// computer, or parts a test stands in.
pub struct Assembly(Box<dyn FnOnce(SessionSide) -> Result<CompatSessionHandle, String> + Send>);

impl Assembly {
    /// The computer's own Windows stack: the shipped catalogue, Bubblewrap,
    /// and the appliance once it is set up.
    #[must_use]
    pub fn system() -> Self {
        Self(Box::new(|side| {
            let catalogue = shipped_catalogue();
            let vm = LazyVmDelegate::new({
                let displays = Arc::clone(&side.displays);
                move || appliance_delegate(Arc::clone(&displays))
            });
            serve(
                side,
                catalogue,
                SystemRuntimeRegistry::new(),
                SystemResourceBroker::new(),
                BwrapExecutor,
                vm,
            )
        }))
    }

    /// A service assembled by `assemble`, from the session's side.
    #[must_use]
    pub fn with(
        assemble: impl FnOnce(SessionSide) -> Result<CompatSessionHandle, String> + Send + 'static,
    ) -> Self {
        Self(Box::new(assemble))
    }

    /// Start serving the session's side.
    ///
    /// # Errors
    ///
    /// Why the service could not start: its capsule store or socket.
    pub fn start(self, side: SessionSide) -> Result<CompatSessionHandle, String> {
        (self.0)(side)
    }
}

impl Default for Assembly {
    fn default() -> Self {
        Self::system()
    }
}

/// Serve `side` with the given parts. Each client is served on a thread of
/// its own; nothing here blocks the caller's thread once it returns.
///
/// # Errors
///
/// Why the service could not start: its capsule store or socket.
pub fn serve<R, B, E, V>(
    side: SessionSide,
    catalogue: Catalogue,
    runtimes: R,
    resources: B,
    executor: E,
    vm: V,
) -> Result<CompatSessionHandle, String>
where
    R: RuntimeRegistry + 'static,
    B: LaunchResourceBroker + 'static,
    E: SandboxExecutor + 'static,
    V: VmDelegate + 'static,
{
    let store = CapsuleStore::new(&side.capsules).map_err(|error| error.to_string())?;
    let managed = Arc::new(
        ManagedCompatibility::new(
            catalogue,
            store,
            runtimes,
            resources,
            executor,
            vm,
            side.displays,
        )
        .map_err(|error| error.to_string())?
        .with_administrator_check(side.administrator),
    );
    // What this device's apps read of the programs and the appliance, before any manager
    // connects: the session's system service publishes it as the device's `windows-apps`.
    side.manager.set(Arc::new(crate::DeviceManager::new(
        Arc::clone(&managed),
        Arc::clone(&side.sandboxes),
    )));
    let authenticator = LauncherAuthenticator::new(side.launcher, side.parent_pid, side.launches);
    spawn_session_service(
        side.socket,
        managed,
        side.sandboxes,
        authenticator,
        side.entries,
        side.reopenings,
    )
    .map_err(|error| error.to_string())
}

/// The catalogue the image ships, or an empty one on a computer without it:
/// the manager then lists what is set up and every launch says what is
/// missing.
fn shipped_catalogue() -> Catalogue {
    match load_shipped_catalogue(&ShippedCataloguePaths::default()) {
        Ok(report) => {
            if !report.skipped_bottles_files.is_empty() {
                tracing::warn!(
                    skipped = report.skipped_bottles_files.len(),
                    "windows compatibility catalogue skipped unsupported Bottles metadata"
                );
            }
            report.catalogue
        }
        Err(error) => {
            tracing::info!(%error, "no Windows program catalogue: every program takes the default route");
            Catalogue {
                schema_version: compat_catalogue::CATALOGUE_SCHEMA_VERSION,
                records: Vec::new(),
            }
        }
    }
}

/// The rail presentation is chosen when its wrapper says this computer
/// carries everything it needs.
fn native_rail_surface_available() -> bool {
    std::process::Command::new(RAIL_PRESENTER)
        .arg("--probe")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The appliance this computer set up, or why there is none.
fn appliance_delegate(displays: Arc<dyn LaunchDisplays>) -> Result<Arc<dyn VmDelegate>, String> {
    let domain_uuid = match std::fs::read_to_string(format!("{APPLIANCE_ROOT}/domain.uuid")) {
        Ok(uuid) => uuid,
        // The system keeps the appliance's directory to itself (tmpfiles makes it root's,
        // mode 0700): a session that may not look into it has no appliance it could reach,
        // which to the person is the same as none being set up.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            return Err(NOT_SET_UP.to_string());
        }
        Err(error) => return Err(format!("could not read the appliance's identity: {error}")),
    };
    let domain_uuid = domain_uuid.trim();
    let presentation = ApplicationPresentation::select(native_rail_surface_available());
    let configuration = ApplianceConfig::recommended(
        domain_uuid,
        format!("{APPLIANCE_ROOT}/system.qcow2"),
        format!("{APPLIANCE_ROOT}/nvram.fd"),
    );
    configuration
        .validate()
        .map_err(|error| format!("invalid appliance configuration: {error}"))?;
    let bridge = HttpsGuestBridge::new(GuestBridgeConfig {
        endpoint: "192.168.251.2:49888"
            .parse()
            .map_err(|error| format!("invalid fixed guest endpoint: {error}"))?,
        bootstrap_secret: format!("{APPLIANCE_ROOT}/bridge/bootstrap.secret").into(),
        pin_directory: format!("{APPLIANCE_ROOT}/session").into(),
        presentation,
    })
    .map_err(|error| format!("could not open authenticated guest bridge: {error}"))?;
    let shared_session = VmSharedSessionTracker::default();
    let portal_documents = Arc::new(PortalDocumentStore::default());
    let appliance = WindowsAppliance::new(
        configuration,
        ProvisioningStore::new(format!("{APPLIANCE_ROOT}/session/state.json")),
        LibvirtBackend::new(),
        bridge,
        SharedSessionCapabilityBroker::new(
            PortalRailCapabilityBroker::new(Arc::clone(&portal_documents)),
            shared_session.clone(),
        ),
        Presentation {
            rail: AudioIsolatedRailLauncher::new(Arc::clone(&displays)),
            spice: AudioIsolatedSpiceLauncher::new(displays),
        },
    );
    Ok(Arc::new(LiveVmDelegate::with_shared_session_tracker(
        appliance,
        PortalRailPortalAuthority::new(presentation, portal_documents),
        presentation,
        shared_session,
    )))
}

/// The presentation the guest chose for each application.
struct Presentation {
    rail: AudioIsolatedRailLauncher,
    spice: AudioIsolatedSpiceLauncher,
}

impl windows_vm::RemoteAppLauncher for Presentation {
    fn launch(
        &self,
        shown: &windows_vm::Shown<'_>,
        guest: &windows_vm::GuestLaunch,
        brokered: &windows_vm::BrokeredLaunch,
    ) -> Result<windows_vm::RemoteAppProcess, windows_vm::RemoteAppError> {
        match guest.presentation {
            ApplicationPresentation::NativeRail => self.rail.launch(shown, guest, brokered),
            ApplicationPresentation::AppFocusedSpice => self.spice.launch(shown, guest, brokered),
        }
    }
}

type Build = dyn Fn() -> Result<Arc<dyn VmDelegate>, String> + Send + Sync;

/// The appliance's delegate, built the first time it can be: until the
/// appliance is set up, every request builds it again and answers with why
/// it is unavailable.
pub struct LazyVmDelegate {
    build: Box<Build>,
    built: Mutex<Option<Arc<dyn VmDelegate>>>,
}

impl LazyVmDelegate {
    #[must_use]
    pub fn new(
        build: impl Fn() -> Result<Arc<dyn VmDelegate>, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            build: Box::new(build),
            built: Mutex::new(None),
        }
    }

    fn get(&self) -> Result<Arc<dyn VmDelegate>, String> {
        let unavailable = || "Windows VM delegate state is unavailable".to_string();
        let built = self.built.lock().map_err(|_| unavailable())?.clone();
        if let Some(delegate) = built {
            return Ok(delegate);
        }
        let candidate = (self.build)()?;
        let mut built = self.built.lock().map_err(|_| unavailable())?;
        Ok(built.get_or_insert_with(|| Arc::clone(&candidate)).clone())
    }
}

impl VmDelegate for LazyVmDelegate {
    fn availability(&self) -> VmAvailability {
        match self.get() {
            Ok(delegate) => delegate.availability(),
            Err(error) => VmAvailability::unavailable(error),
        }
    }

    fn inventory(&self) -> VmInventory {
        match self.get() {
            Ok(delegate) => delegate.inventory(),
            Err(error) => VmInventory {
                availability: VmAvailability::unavailable(error),
                applications: Vec::new(),
            },
        }
    }

    fn install_from_portal(
        &self,
        workspace_id: &str,
        input: VmInstallerInput,
        source: File,
    ) -> Result<VmInstallation, windows_vm::LaunchError> {
        self.get()
            .map_err(windows_vm::AgentError::Transport)?
            .install_from_portal(workspace_id, input, source)
    }

    fn installation_status(&self, workspace_id: &str) -> Option<VmInstallation> {
        self.get()
            .ok()
            .and_then(|delegate| delegate.installation_status(workspace_id))
    }

    fn refresh_installation_status(
        &self,
        workspace_id: &str,
    ) -> Result<Option<VmInstallation>, windows_vm::LaunchError> {
        // Without an appliance nothing is being set up in it: the manager asks this every
        // time it lists, and must not be told the appliance's absence as a failure.
        self.get().ok().map_or(Ok(None), |delegate| {
            delegate.refresh_installation_status(workspace_id)
        })
    }

    fn publish_install_candidate(
        &self,
        workspace_id: &str,
        entry_id: &str,
    ) -> Result<VmCandidatePublication, windows_vm::LaunchError> {
        self.get()
            .map_err(windows_vm::AgentError::Transport)?
            .publish_install_candidate(workspace_id, entry_id)
    }

    fn uninstallation_status(&self, workspace_id: &str) -> Option<VmUninstallation> {
        self.get()
            .ok()
            .and_then(|delegate| delegate.uninstallation_status(workspace_id))
    }

    fn refresh_uninstallation_status(
        &self,
        workspace_id: &str,
    ) -> Result<Option<VmUninstallation>, windows_vm::LaunchError> {
        // Without an appliance nothing is being set up in it: the manager asks this every
        // time it lists, and must not be told the appliance's absence as a failure.
        self.get().ok().map_or(Ok(None), |delegate| {
            delegate.refresh_uninstallation_status(workspace_id)
        })
    }

    fn remove_published_application(&self, entry_id: &str) -> Result<(), windows_vm::LaunchError> {
        self.get()
            .map_err(windows_vm::AgentError::Transport)?
            .remove_published_application(entry_id)
    }

    fn uninstall_published_application(
        &self,
        workspace_id: &str,
        entry_id: &str,
    ) -> Result<VmUninstallation, windows_vm::LaunchError> {
        self.get()
            .map_err(windows_vm::AgentError::Transport)?
            .uninstall_published_application(workspace_id, entry_id)
    }

    fn launch(
        &self,
        request: VmRouteRequest<'_>,
    ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError> {
        self.get()
            .map_err(windows_vm::AgentError::Transport)?
            .launch(request)
    }

    fn launch_with_portal_file(
        &self,
        request: VmRouteRequest<'_>,
        source: File,
        access: windows_vm::PortalDocumentAccess,
    ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError> {
        self.get()
            .map_err(windows_vm::AgentError::Transport)?
            .launch_with_portal_file(request, source, access)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    /// Answers every request, counting them.
    #[derive(Default)]
    struct Counting {
        calls: AtomicUsize,
    }

    impl VmDelegate for Counting {
        fn availability(&self) -> VmAvailability {
            self.calls.fetch_add(1, Ordering::AcqRel);
            VmAvailability::ready("set up")
        }

        fn installation_status(&self, _: &str) -> Option<VmInstallation> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            None
        }

        fn launch(
            &self,
            _: VmRouteRequest<'_>,
        ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError> {
            self.calls.fetch_add(1, Ordering::AcqRel);
            Err(windows_vm::LaunchError::CapabilityBroker(
                "counted".to_string(),
            ))
        }
    }

    /// A computer without an appliance has nothing being set up in it: the status the manager
    /// asks for each time it lists is none, not a failure it shows the person.
    #[test]
    fn without_an_appliance_nothing_is_being_set_up() {
        let lazy = LazyVmDelegate::new(|| Err(NOT_SET_UP.to_string()));
        assert!(matches!(
            lazy.refresh_installation_status("workspace"),
            Ok(None)
        ));
        assert!(matches!(
            lazy.refresh_uninstallation_status("workspace"),
            Ok(None)
        ));
        assert!(
            !lazy.availability().available,
            "it still says the appliance is unavailable"
        );
    }

    /// Until the appliance is set up every request says why it is unavailable and looks again;
    /// once it is, every request reaches the one delegate built for it.
    #[test]
    fn the_appliance_is_looked_for_until_it_is_set_up() {
        let inner = Arc::new(Counting::default());
        let attempts = Arc::new(AtomicUsize::new(0));
        let lazy = LazyVmDelegate::new({
            let inner = Arc::clone(&inner);
            let attempts = Arc::clone(&attempts);
            move || {
                if attempts.fetch_add(1, Ordering::AcqRel) < 2 {
                    Err("The Windows appliance is not set up on this computer.".to_string())
                } else {
                    Ok(Arc::clone(&inner) as Arc<dyn VmDelegate>)
                }
            }
        });

        for _ in 0..2 {
            let availability = lazy.availability();
            assert!(!availability.available);
            assert!(
                availability.detail.contains("not set up"),
                "{availability:?}"
            );
        }
        assert!(lazy.availability().available);
        assert!(lazy.installation_status("workspace").is_none());
        let request = VmRouteRequest {
            route: crate::RouteKind::Vm,
            app_id: "app",
            workspace_id: "workspace",
            published_application_id: "application",
            capabilities: &[],
            resolved_capabilities: &launcher::capabilities::CapabilityPlan::empty(),
        };
        let Err(error) = lazy.launch(request) else {
            panic!("the counting delegate refuses every launch");
        };
        assert!(error.to_string().contains("counted"), "{error}");
        assert_eq!(
            attempts.load(Ordering::Acquire),
            3,
            "built once it could be"
        );
        assert_eq!(inner.calls.load(Ordering::Acquire), 3);
    }
}
