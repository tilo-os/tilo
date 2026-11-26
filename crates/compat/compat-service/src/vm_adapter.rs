//! Live adapter from Tilo's compatibility service to the one Windows appliance.
//!
//! The adapter owns no Windows policy.  In particular it cannot turn a host
//! path, a catalogue recipe, or a manager request into an RDP drive.  The
//! A future, canonical Tilo portal/capability broker may supply an
//! already-resolved [`windows_vm::VmLaunchGrants`] projection, but only after
//! Tilo has probed a local RAIL surface that can carry its redirects.
//! `windows-vm` then materialises and revokes the corresponding lease around
//! the `RemoteApp` process. The live app-focused SPICE path remains deny-all.

use launcher::capabilities::CapabilityPlan;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::fs::File;
use std::os::fd::{AsFd as _, OwnedFd};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use launcher::audio_graph::AudioGraph;

use crate::{
    LaunchDisplay, LaunchDisplays, VmAvailability, VmDelegate, VmInstallCandidate, VmInstallation,
    VmInstallationPhase, VmInstallerInput, VmInstallerKind, VmInventory, VmPresentation,
    VmPublishedApplication, VmRouteRequest, VmSharedSessionContributor, VmSharedSessionGrantKind,
    VmSharedSessionLeaseSummary,
};

/// Authoritative host-side view of the one shared Windows user/session.
///
/// The tracker is fed only by the capability broker's materialized result and
/// by the authenticated guest catalogue. A management client cannot name a
/// contributor or claim a grant. This matters because RDP redirects are
/// session-wide: every Windows process in the session can potentially use the
/// union while it is active.
#[derive(Clone, Debug, Default)]
pub struct VmSharedSessionTracker {
    inner: Arc<Mutex<SharedSessionState>>,
}

#[derive(Debug, Default)]
struct SharedSessionState {
    display_names: BTreeMap<String, String>,
    leases: BTreeMap<String, SharedSessionLease>,
}

#[derive(Clone, Debug)]
struct SharedSessionLease {
    published_application_id: String,
    grants: BTreeSet<VmSharedSessionGrantKind>,
}

impl VmSharedSessionTracker {
    fn register_catalog(&self, catalog: &windows_vm::Catalog) {
        if let Ok(mut state) = self.inner.lock() {
            state.display_names = catalog
                .applications
                .iter()
                .map(|application| (application.id.clone(), application.display_name.clone()))
                .collect();
        }
    }

    fn activate(
        &self,
        request: &windows_vm::LaunchRequest,
        brokered: &windows_vm::BrokeredLaunch,
    ) -> Result<(), windows_vm::LaunchError> {
        let mut grants = BTreeSet::from([VmSharedSessionGrantKind::AudioPlayback]);
        if brokered.clipboard {
            grants.insert(VmSharedSessionGrantKind::Clipboard);
        }
        if brokered.microphone {
            grants.insert(VmSharedSessionGrantKind::Microphone);
        }
        if brokered.network {
            grants.insert(VmSharedSessionGrantKind::NetworkEgress);
        }
        if brokered.shares.iter().any(|share| share.read_only) {
            grants.insert(VmSharedSessionGrantKind::PortalFilesReadOnly);
        }
        if brokered.shares.iter().any(|share| !share.read_only) {
            grants.insert(VmSharedSessionGrantKind::PortalFilesReadWrite);
        }
        let mut state = self.inner.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "shared Windows session lease state is unavailable".to_string(),
            )
        })?;
        if state.leases.contains_key(&brokered.lease_id) {
            return Err(windows_vm::LaunchError::CapabilityBroker(
                "capability broker reused an active Windows session lease".to_string(),
            ));
        }
        state.leases.insert(
            brokered.lease_id.clone(),
            SharedSessionLease {
                published_application_id: request.application_id.clone(),
                grants,
            },
        );
        drop(state);
        Ok(())
    }

    fn revoke(&self, lease_id: &str) {
        if let Ok(mut state) = self.inner.lock() {
            state.leases.remove(lease_id);
        }
    }

    /// Bounded DTO for the management UI. Opaque lease IDs, workspace IDs,
    /// portal document IDs, paths, and credentials never leave the broker.
    #[must_use]
    pub fn summary(&self) -> VmSharedSessionLeaseSummary {
        let Ok(state) = self.inner.lock() else {
            return VmSharedSessionLeaseSummary::default();
        };
        let effective_grants = state
            .leases
            .values()
            .flat_map(|lease| lease.grants.iter().copied())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let contributors = state
            .leases
            .values()
            .map(|lease| lease.published_application_id.as_str())
            .collect::<BTreeSet<_>>()
            .into_iter()
            .map(|published_application_id| VmSharedSessionContributor {
                published_application_id: published_application_id.to_string(),
                display_name: state
                    .display_names
                    .get(published_application_id)
                    .cloned()
                    .unwrap_or_else(|| published_application_id.to_string()),
            })
            .collect::<Vec<_>>();
        VmSharedSessionLeaseSummary {
            active: !contributors.is_empty(),
            effective_grants,
            contributors,
        }
    }
}

/// Capability broker decorator which notes only resources the underlying
/// canonical broker actually materialized. Revocation removes the summary
/// before tearing down the resources, so UI state cannot linger after access.
#[derive(Debug)]
pub struct SharedSessionCapabilityBroker<C> {
    inner: C,
    tracker: VmSharedSessionTracker,
}

impl<C> SharedSessionCapabilityBroker<C> {
    #[must_use]
    pub fn new(inner: C, tracker: VmSharedSessionTracker) -> Self {
        Self { inner, tracker }
    }
}

impl<C: windows_vm::CapabilityBroker> windows_vm::CapabilityBroker
    for SharedSessionCapabilityBroker<C>
{
    fn materialize(
        &self,
        request: &windows_vm::LaunchRequest,
    ) -> Result<windows_vm::BrokeredLaunch, windows_vm::LaunchError> {
        let brokered = self.inner.materialize(request)?;
        if let Err(error) = self.tracker.activate(request, &brokered) {
            self.inner.revoke(&brokered.lease_id);
            return Err(error);
        }
        Ok(brokered)
    }

    fn revoke(&self, lease_id: &str) {
        self.tracker.revoke(lease_id);
        self.inner.revoke(lease_id);
    }
}

/// Privileged Tilo authority for a future RAIL capability projection used by
/// the Windows appliance. It is intentionally an injected adapter rather than
/// a second permission database: an implementation must use the ordinary
/// portal document handles and workspace decision records already owned by
/// Tilo, after a local RAIL surface has been probed.
///
/// A portal implementation may return opaque `portal_documents`, clipboard,
/// or a network lease only when it has materialised and can later revoke that
/// exact resource.  Raw host paths are never part of this API.
pub trait VmPortalAuthority: Send + Sync {
    fn grants_for(
        &self,
        app_id: &str,
        workspace_id: &str,
        capabilities: &CapabilityPlan,
    ) -> Result<windows_vm::VmLaunchGrants, windows_vm::LaunchError>;

    fn register_portal_file(
        &self,
        _app_id: &str,
        _workspace_id: &str,
        _source: std::fs::File,
        _access: windows_vm::PortalDocumentAccess,
    ) -> Result<windows_vm::PortalDocumentGrant, windows_vm::LaunchError> {
        Err(windows_vm::LaunchError::CapabilityBroker(
            "this Windows presentation has no portal file bridge".to_string(),
        ))
    }
}

/// Native-RAIL authority backed by the same opaque descriptor registry as the
/// drive broker. The only input is the already-open portal descriptor.
pub struct PortalRailPortalAuthority {
    native_rail: bool,
    documents: std::sync::Arc<windows_vm::PortalDocumentStore>,
}

impl PortalRailPortalAuthority {
    #[must_use]
    pub fn new(
        presentation: windows_vm::ApplicationPresentation,
        documents: std::sync::Arc<windows_vm::PortalDocumentStore>,
    ) -> Self {
        Self {
            native_rail: presentation == windows_vm::ApplicationPresentation::NativeRail,
            documents,
        }
    }
}

impl VmPortalAuthority for PortalRailPortalAuthority {
    fn grants_for(
        &self,
        app_id: &str,
        workspace_id: &str,
        capabilities: &CapabilityPlan,
    ) -> Result<windows_vm::VmLaunchGrants, windows_vm::LaunchError> {
        CanonicalRailPortalAuthority {
            native_rail: self.native_rail,
        }
        .grants_for(app_id, workspace_id, capabilities)
    }

    fn register_portal_file(
        &self,
        app_id: &str,
        workspace_id: &str,
        source: std::fs::File,
        access: windows_vm::PortalDocumentAccess,
    ) -> Result<windows_vm::PortalDocumentGrant, windows_vm::LaunchError> {
        if !self.native_rail {
            return Err(windows_vm::LaunchError::CapabilityBroker(
                "portal file drives require native RAIL".to_string(),
            ));
        }
        self.documents
            .register(workspace_id, app_id, source, access)
    }
}

/// Live projection of canonical Tilo grants which have an exact RAIL
/// transport and revocation boundary today.
///
/// Clipboard is represented by the ordinary `Device(DeviceCap::Clipboard)` grant
/// and microphone by `Device(DeviceCap::Microphone)`. Files are deliberately absent: a
/// filesystem path is not an opaque portal document handle. `NetHost` values
/// are preserved verbatim for the lease relay; this does not attach a NAT NIC
/// or convert a filtered grant into unrestricted guest networking.
#[derive(Clone, Copy, Debug)]
pub struct CanonicalRailPortalAuthority {
    native_rail: bool,
}

impl CanonicalRailPortalAuthority {
    #[must_use]
    pub fn new(presentation: windows_vm::ApplicationPresentation) -> Self {
        Self {
            native_rail: presentation == windows_vm::ApplicationPresentation::NativeRail,
        }
    }
}

impl VmPortalAuthority for CanonicalRailPortalAuthority {
    fn grants_for(
        &self,
        _app_id: &str,
        _workspace_id: &str,
        capabilities: &CapabilityPlan,
    ) -> Result<windows_vm::VmLaunchGrants, windows_vm::LaunchError> {
        let device_granted = |device| self.native_rail && capabilities.devices.contains(&device);
        Ok(windows_vm::VmLaunchGrants {
            network_hosts: if self.native_rail {
                capabilities.net_hosts.clone()
            } else {
                Vec::new()
            },
            network: self.native_rail && !capabilities.net_hosts.is_empty(),
            clipboard: device_granted(collab_types::caps::DeviceCap::Clipboard),
            microphone: device_granted(collab_types::caps::DeviceCap::Microphone),
            portal_documents: Vec::new(),
        })
    }
}

/// App-focused SPICE presentation constrained to a per-launch private audio
/// graph. This live fallback exposes playback only; microphone capture,
/// clipboard, file shares, and egress remain unavailable until a probed local
/// RAIL surface can carry grants from a canonical Tilo broker.
pub struct AudioIsolatedSpiceLauncher {
    presenter: Presenter,
}

/// Native RAIL presentation on the X11 display the launch's keeper serves and
/// a private audio graph. Playback is always present; the proxy publishes a
/// source only when the brokered shared-session lease includes microphone.
pub struct AudioIsolatedRailLauncher {
    presenter: Presenter,
    binary: PathBuf,
}

/// The wrapper that presents a guest application through `FreeRDP` RAIL.
pub const RAIL_PRESENTER: &str = "/usr/libexec/tilo-windows-rail";

/// Where the presentation finds the RDP password: the descriptor its private
/// `--tilo-password-fd` argument names, which becomes `FreeRDP`'s standard input.
const PASSWORD_ARGUMENT: &str = "--tilo-password-fd";

/// What both presentations share: the displays each launch is given, the
/// launcher that starts each launch's private audio graph, and how the graphs
/// are named apart.
///
/// A presentation starts as a Windows program does: as one of the session's
/// own launches behind an app keeper where the session keeps them, so it stays
/// open (and `FreeRDP` connected) while the desktop restarts in place. Its
/// command is the launcher's `windows-audio`, which starts the graph as the
/// presentation's own children, then the presentation itself.
struct Presenter {
    displays: Arc<dyn LaunchDisplays>,
    launcher: PathBuf,
    prepare_audio_graph: crate::resources::PrepareAudioGraph,
    /// Names each launch's audio directory apart from the other presentation's.
    kind: &'static str,
    next: AtomicU64,
}

impl Presenter {
    fn new(displays: Arc<dyn LaunchDisplays>, kind: &'static str) -> Self {
        Self {
            displays,
            launcher: PathBuf::from(crate::plan::TILO_LAUNCHER),
            prepare_audio_graph: AudioGraph::prepare,
            kind,
            next: AtomicU64::new(0),
        }
    }

    /// Present `command` as `shown` says until it ends, with a private audio
    /// graph that carries a source only for `microphone`, on an X11 display of
    /// the keeper's when `x11`. `network`, when the guest was given one, is
    /// kept in the launch's keeper directory for the desktop that adopts the
    /// program after a restart to serve egress again.
    fn present(
        &self,
        shown: &windows_vm::Shown<'_>,
        microphone: bool,
        x11: bool,
        network: Option<&windows_vm::GuestNetworkBinding>,
        command: windows_vm::CommandRequest,
        failed: fn(Option<i32>, String) -> windows_vm::RemoteAppError,
    ) -> Result<windows_vm::RemoteAppProcess, windows_vm::RemoteAppError> {
        let (entry_id, app_id) = (
            vm_entry_id(shown.application_id),
            vm_app_id(shown.application_id),
        );
        let kept = self
            .displays
            .kept_display(&crate::KeptProgram {
                app_id: &app_id,
                display_name: shown.display_name,
                entry_id: &entry_id,
                workspace_id: shown.workspace_id,
                x11,
                setup: false,
            })
            .transpose()
            .map_err(|error| failed(None, error))?;
        let keeper_dir = kept.as_ref().map(|kept| kept.display().runtime_dir.clone());
        let display = match &kept {
            Some(kept) => kept.display().clone(),
            None => self
                .displays
                .display_for(shown.workspace_id)
                .map_err(|error| failed(None, error))?,
        };
        // Written before the program starts, so a desktop that restarts in place at any moment
        // after it is running finds the guest's endpoint and token to serve its egress again.
        if let (Some(dir), Some(network)) = (&keeper_dir, network)
            && let Err(error) = network.persist(dir)
        {
            tracing::warn!(dir = %dir.display(), %error, "a Windows appliance program's filtered network is not kept for the next desktop");
        }
        let forget_network = || {
            if let (Some(dir), Some(_)) = (&keeper_dir, network) {
                windows_vm::GuestNetworkBinding::forget(dir);
            }
        };
        let sequence = self.next.fetch_add(1, Ordering::Relaxed);
        let graph = match (self.prepare_audio_graph)(
            display.runtime_dir.join("tilo-windows-vm").join(format!(
                "{}-{}-{sequence}",
                self.kind,
                std::process::id()
            )),
            display.uid,
            microphone,
        ) {
            Ok(graph) => graph,
            Err(error) => {
                forget_network();
                return Err(failed(None, format!("private audio setup failed: {error}")));
            }
        };
        let ended = self
            .command(&graph, &display, command)
            .and_then(|command| match kept {
                Some(kept) => kept.start(command),
                None => command.spawn(),
            })
            .and_then(|program| program.wait())
            .map_err(|error| failed(None, error.to_string()));
        graph.remove();
        let ended = match ended {
            Ok(ended) => ended,
            Err(error) => {
                forget_network();
                return Err(error);
            }
        };
        if !ended.success {
            return Err(failed(ended.exit_code, ended.said.unwrap_or_default()));
        }
        Ok(windows_vm::RemoteAppProcess {
            exit_code: ended.exit_code,
        })
    }

    /// `request` started by the launcher after the private `graph`, shown on
    /// `display` (its own Wayland socket, whatever the session's process
    /// inherited) and heard through the graph. A secret `request` would write
    /// to its standard input reaches it through a pipe at a descriptor of its
    /// own instead, whose number its arguments name: a kept program has no
    /// standard input of the service's.
    fn command(
        &self,
        graph: &AudioGraph,
        display: &LaunchDisplay,
        request: windows_vm::CommandRequest,
    ) -> Result<crate::SandboxCommand, crate::ExecutionError> {
        let native = graph.pipewire_socket();
        let audio_runtime = native.parent().ok_or_else(|| {
            crate::ExecutionError::Io(std::io::Error::other(
                "private audio endpoint has no runtime directory",
            ))
        })?;
        let mut environment: Vec<(OsString, OsString)> = vec![
            (
                "WAYLAND_DISPLAY".into(),
                display.wayland_socket.clone().into(),
            ),
            ("PIPEWIRE_RUNTIME_DIR".into(), audio_runtime.into()),
            ("PIPEWIRE_REMOTE".into(), "pipewire-0".into()),
            (
                "PULSE_SERVER".into(),
                format!("unix:{}", graph.pulse_socket().display()).into(),
            ),
        ];
        environment.extend(request.environment);
        let mut args: Vec<OsString> = vec![
            "windows-audio".into(),
            "--graph".into(),
            graph.root().into(),
            "--".into(),
            "/usr/bin/env".into(),
        ];
        args.extend(environment.into_iter().map(|(name, value)| {
            let mut setting = name;
            setting.push("=");
            setting.push(value);
            setting
        }));
        args.push(request.program);
        args.extend(request.args);
        let mut command = crate::SandboxCommand::new(&self.launcher, args);
        if !request.stdin.is_empty() {
            let (reader, mut writer) = std::io::pipe()?;
            // Far below a pipe's capacity: written whole before anyone reads, and gone from
            // the pipe once read.
            std::io::Write::write_all(&mut writer, &request.stdin)?;
            drop(writer);
            command.inherited.push((
                crate::execute::duplicate_above_handed_numbers(OwnedFd::from(reader).as_fd())?,
                crate::execute::PASSWORD_CHILD_FD,
            ));
            command
                .args
                .push(format!("{PASSWORD_ARGUMENT}={}", crate::execute::PASSWORD_CHILD_FD).into());
        }
        Ok(command)
    }
}

impl AudioIsolatedRailLauncher {
    #[must_use]
    pub fn new(displays: Arc<dyn LaunchDisplays>) -> Self {
        Self {
            presenter: Presenter::new(displays, "rail"),
            binary: PathBuf::from(RAIL_PRESENTER),
        }
    }

    /// Present through the wrapper at `binary`, started by the launcher at
    /// `launcher`, after audio graphs `prepare_audio_graph` prepares, instead
    /// of the installed ones: a test's stand-ins.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn with_stand_ins(
        mut self,
        binary: PathBuf,
        launcher: PathBuf,
        prepare_audio_graph: fn(
            PathBuf,
            u32,
            bool,
        )
            -> Result<AudioGraph, launcher::audio_graph::AudioGraphError>,
    ) -> Self {
        self.binary = binary;
        self.presenter.launcher = launcher;
        self.presenter.prepare_audio_graph = prepare_audio_graph;
        self
    }
}

impl windows_vm::RemoteAppLauncher for AudioIsolatedRailLauncher {
    fn launch(
        &self,
        shown: &windows_vm::Shown<'_>,
        guest: &windows_vm::GuestLaunch,
        brokered: &windows_vm::BrokeredLaunch,
    ) -> Result<windows_vm::RemoteAppProcess, windows_vm::RemoteAppError> {
        let command = windows_vm::FreeRdpLauncher::new()
            .with_binary(&self.binary)
            .command(guest, brokered)?;
        // FreeRDP's RAIL client is an X11 program: the keeper serves it a display of its own.
        self.presenter.present(
            shown,
            brokered.microphone,
            true,
            brokered.network_binding.as_ref(),
            command,
            |code, stderr| windows_vm::RemoteAppError::FreeRdp { code, stderr },
        )
    }
}

impl AudioIsolatedSpiceLauncher {
    #[must_use]
    pub fn new(displays: Arc<dyn LaunchDisplays>) -> Self {
        Self {
            presenter: Presenter::new(displays, "spice"),
        }
    }
}

impl windows_vm::RemoteAppLauncher for AudioIsolatedSpiceLauncher {
    fn launch(
        &self,
        shown: &windows_vm::Shown<'_>,
        guest: &windows_vm::GuestLaunch,
        brokered: &windows_vm::BrokeredLaunch,
    ) -> Result<windows_vm::RemoteAppProcess, windows_vm::RemoteAppError> {
        let command = windows_vm::AppFocusedSpiceLauncher::new().command(guest, brokered)?;
        // The app-focused SPICE fallback carries no egress: the guest is given no network.
        self.presenter
            .present(shown, false, false, None, command, |code, stderr| {
                windows_vm::RemoteAppError::Spice { code, stderr }
            })
    }
}

/// Whether `entry_id` names a program of the Windows appliance: it closes
/// inside Windows, so the manager never offers to quit it.
#[must_use]
pub(crate) fn is_appliance_entry(entry_id: &str) -> bool {
    entry_id.starts_with(APPLIANCE_ENTRY_PREFIX)
}

const APPLIANCE_ENTRY_PREFIX: &str = "windows-vm-";

/// Concrete delegate installed in Tilo's privileged compatibility host.
///
/// `WindowsAppliance` provides the singleton libvirt backend, authenticated
/// guest bridge, catalogue validation, selected RAIL/SPICE presentation, and
/// short-lived resource revocation.  This type supplies the missing service
/// glue: only the catalogue-selected published application id crosses the
/// boundary and only the normal Tilo portal may project grants.
pub struct LiveVmDelegate<B, A, C, L, P> {
    appliance: windows_vm::WindowsAppliance<B, A, C, L>,
    portal: P,
    /// The host surface chosen by the authenticated guest bridge adapter.
    /// It is descriptive for availability; `WindowsAppliance::launch` still
    /// verifies the guest's actual response before a viewer is started.
    presentation: windows_vm::ApplicationPresentation,
    shared_session: VmSharedSessionTracker,
    launch_lock: Mutex<()>,
    installer: Mutex<Option<ActiveVmInstallation>>,
    uninstaller: Mutex<Option<ActiveVmUninstallation>>,
}

struct ActiveVmUninstallation {
    workspace_id: String,
    entry_id: String,
    operation: windows_vm::UninstallationOperationId,
    status: crate::VmUninstallation,
}

/// The guest keeps the transaction itself; its workspace, operation and
/// catalogue generation are also kept with the appliance's state
/// ([`windows_vm::WindowsAppliance::retain_setup`]) until it needs nothing more
/// of the person, so a service that restarts while the setup's window stays
/// open finds it again and offers the same next step.
struct ActiveVmInstallation {
    /// The authenticated Tilo workspace that started this transaction. This
    /// deliberately outlives a manager connection so reconnects in the same
    /// workspace can resume it, while other workspaces cannot observe or
    /// publish its candidates.
    workspace_id: String,
    operation: windows_vm::InstallationOperationId,
    generation: u64,
    candidates: BTreeMap<String, windows_vm::DiscoveredApplication>,
    status: VmInstallation,
}

impl ActiveVmInstallation {
    fn belongs_to(&self, workspace_id: &str) -> bool {
        self.workspace_id == workspace_id
    }

    fn status_for(&self, workspace_id: &str) -> Option<VmInstallation> {
        self.belongs_to(workspace_id).then(|| self.status.clone())
    }

    fn candidate_for(
        &self,
        workspace_id: &str,
        entry_id: &str,
    ) -> Option<windows_vm::DiscoveredApplication> {
        self.belongs_to(workspace_id)
            .then(|| self.candidates.get(entry_id).cloned())
            .flatten()
    }

    /// `operation` of `workspace_id`, whose setup is still going on in the guest as far as the service
    /// knows: what it holds until the guest is next asked.
    fn running(
        workspace_id: String,
        operation: windows_vm::InstallationOperationId,
        generation: u64,
    ) -> Self {
        let status = vm_installation_from_guest(
            &operation,
            &windows_vm::InstallationStatus {
                operation_id: operation.clone(),
                state: windows_vm::InstallationState::Running,
                exit_code: None,
                candidates: Vec::new(),
            },
        );
        Self {
            workspace_id,
            operation,
            generation,
            candidates: BTreeMap::new(),
            status,
        }
    }

    /// The setup ended and every application it found was added or none was: nothing in it is
    /// left for the person to do.
    fn settled(&self) -> bool {
        matches!(
            self.status.phase,
            VmInstallationPhase::Completed | VmInstallationPhase::Failed
        ) && self.candidates.is_empty()
    }
}

impl ActiveVmUninstallation {
    fn belongs_to(&self, workspace_id: &str) -> bool {
        self.workspace_id == workspace_id
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self.status.phase,
            VmInstallationPhase::Completed | VmInstallationPhase::Failed
        )
    }
}

impl<B, A, C, L, P> LiveVmDelegate<B, A, C, L, P> {
    fn installer(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, Option<ActiveVmInstallation>>, windows_vm::LaunchError>
    {
        self.installer.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "Windows installer state is unavailable".to_string(),
            )
        })
    }

    /// `operation` needs nothing more of the person: the appliance stops keeping it, so a later
    /// service does not offer it again. What this service shows of it stays until the next setup.
    fn forget_setup(&self, operation: &windows_vm::InstallationOperationId) {
        if let Err(error) = self.appliance.forget_setup(operation) {
            tracing::warn!(%error, "a finished Windows setup stays kept");
        }
    }

    fn require_no_active_uninstallation(&self) -> Result<(), windows_vm::LaunchError> {
        let uninstaller = self.uninstaller.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "Windows uninstaller state is unavailable".to_string(),
            )
        })?;
        if uninstaller
            .as_ref()
            .is_some_and(|active| !active.is_terminal())
        {
            return Err(windows_vm::LaunchError::InvalidRequest(
                "Windows uninstallation is in progress",
            ));
        }
        drop(uninstaller);
        Ok(())
    }

    #[must_use]
    pub fn new(
        appliance: windows_vm::WindowsAppliance<B, A, C, L>,
        portal: P,
        presentation: windows_vm::ApplicationPresentation,
    ) -> Self {
        Self::with_shared_session_tracker(
            appliance,
            portal,
            presentation,
            VmSharedSessionTracker::default(),
        )
    }

    #[must_use]
    pub fn with_shared_session_tracker(
        appliance: windows_vm::WindowsAppliance<B, A, C, L>,
        portal: P,
        presentation: windows_vm::ApplicationPresentation,
        shared_session: VmSharedSessionTracker,
    ) -> Self {
        // A setup that was in progress when the service before this one ended: its window may
        // still be open, and the next step is the same one.
        let installer = appliance
            .retained_setup()
            .map(|(workspace_id, operation, generation)| {
                ActiveVmInstallation::running(workspace_id, operation, generation)
            });
        Self {
            appliance,
            portal,
            presentation,
            shared_session,
            launch_lock: Mutex::new(()),
            installer: Mutex::new(installer),
            uninstaller: Mutex::new(None),
        }
    }

    #[must_use]
    pub fn into_parts(self) -> (windows_vm::WindowsAppliance<B, A, C, L>, P) {
        (self.appliance, self.portal)
    }
}

impl<B, A, C, L, P> VmDelegate for LiveVmDelegate<B, A, C, L, P>
where
    B: windows_vm::VmBackend,
    A: windows_vm::GuestAgent,
    C: windows_vm::CapabilityBroker,
    L: windows_vm::RemoteAppLauncher,
    P: VmPortalAuthority,
{
    fn availability(&self) -> VmAvailability {
        let mut availability = match self.appliance.launch_readiness() {
            Ok(_) => VmAvailability::ready(format!(
                "Ready for an explicit fallback launch using {}.",
                self.presentation
            )),
            Err(windows_vm::LaunchError::NotReady(state))
                if matches!(
                    state.phase,
                    windows_vm::AppliancePhase::Installing { .. }
                        | windows_vm::AppliancePhase::AwaitingGuestBridge
                ) =>
            {
                VmAvailability::ready(
                    "Prepared appliance; the next explicit fallback verifies the guest bridge.",
                )
            }
            Err(error) => VmAvailability::unavailable(error.to_string()),
        };
        availability.shared_session = self.shared_session.summary();
        availability
    }

    fn inventory(&self) -> VmInventory {
        let availability = self.availability();
        let applications = match self.appliance.cached_catalog() {
            Ok(Some(catalog)) => {
                self.shared_session.register_catalog(&catalog);
                catalog
                    .applications
                    .iter()
                    .map(|application| VmPublishedApplication {
                        entry_id: vm_entry_id(&application.id),
                        app_id: vm_app_id(&application.id),
                        published_application_id: application.id.clone(),
                        display_name: application.display_name.clone(),
                        presentation: match self.presentation {
                            windows_vm::ApplicationPresentation::NativeRail => {
                                VmPresentation::NativeRail
                            }
                            windows_vm::ApplicationPresentation::AppFocusedSpice => {
                                VmPresentation::AppFocusedSpice
                            }
                        },
                        uninstall_available: application.uninstall_available,
                    })
                    .collect()
            }
            // A missing or malformed persisted catalogue fails closed: it is
            // never replaced with a host executable scan and never wakes the
            // guest merely to populate the manager.
            Ok(None) | Err(_) => Vec::new(),
        };
        VmInventory {
            availability,
            applications,
        }
    }

    fn install_from_portal(
        &self,
        workspace_id: &str,
        input: VmInstallerInput,
        source: File,
    ) -> Result<VmInstallation, windows_vm::LaunchError> {
        let _launch_guard = self.launch_lock.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "singleton Windows launch check is unavailable".to_string(),
            )
        })?;
        self.require_no_active_uninstallation()?;
        let package = windows_vm::InstallerPackage::new(
            match input.kind {
                VmInstallerKind::Exe => windows_vm::InstallerKind::Exe,
                VmInstallerKind::Msi => windows_vm::InstallerKind::Msi,
            },
            input.byte_len,
            &input.sha256,
        )?;
        let staged = self.appliance.stage_installer(&package, source)?;
        let prepared = self.appliance.begin_installation(&staged)?;
        let operation = prepared.operation_id.clone();
        let catalog = self.appliance.catalog()?;
        // Kept before the setup shows: its window outlives a desktop restart, and the service
        // after it resumes this transaction rather than forgetting its next step.
        self.appliance
            .retain_setup(workspace_id, &operation, catalog.generation)?;
        *self.installer()? = Some(ActiveVmInstallation::running(
            workspace_id.to_string(),
            operation.clone(),
            catalog.generation,
        ));
        // This is intentionally blocking: setup is visible only through the
        // typed app-focused SPICE surface, never an invisible process or a
        // general Windows desktop.
        if let Err(error) = self.appliance.present_installation(workspace_id, &prepared) {
            self.forget_setup(&operation);
            self.installer()?
                .take_if(|active| active.operation == operation);
            return Err(error);
        }
        let guest_status = self.appliance.installation_status(&operation)?;
        let mut installer = self.installer()?;
        let active = installer
            .as_mut()
            .filter(|active| active.operation == operation)
            .ok_or(windows_vm::AgentError::InvalidInstaller)?;
        active.candidates = candidate_map(&guest_status);
        active.status = vm_installation_from_guest(&operation, &guest_status);
        let (status, settled) = (active.status.clone(), active.settled());
        drop(installer);
        if settled {
            self.forget_setup(&operation);
        }
        Ok(status)
    }

    fn installation_status(&self, workspace_id: &str) -> Option<VmInstallation> {
        self.installer.lock().ok().and_then(|active| {
            active
                .as_ref()
                .and_then(|active| active.status_for(workspace_id))
        })
    }

    fn refresh_installation_status(
        &self,
        workspace_id: &str,
    ) -> Result<Option<VmInstallation>, windows_vm::LaunchError> {
        let operation = self
            .installer()?
            .as_ref()
            .filter(|active| active.belongs_to(workspace_id))
            .map(|active| active.operation.clone());
        let Some(operation) = operation else {
            return Ok(None);
        };
        let guest_status = match self.appliance.installation_status(&operation) {
            Ok(status) => status,
            // The guest no longer knows the transaction (Windows was reset meanwhile): there is
            // nothing left to resume.
            Err(windows_vm::LaunchError::Agent(windows_vm::AgentError::InvalidInstaller)) => {
                self.forget_setup(&operation);
                return Ok(None);
            }
            Err(error) => return Err(error),
        };
        let mut installer = self.installer()?;
        let active = installer
            .as_mut()
            .filter(|active| active.operation == operation && active.belongs_to(workspace_id))
            .ok_or(windows_vm::AgentError::InvalidInstaller)?;
        active.candidates = candidate_map(&guest_status);
        active.status = vm_installation_from_guest(&operation, &guest_status);
        let (status, settled) = (active.status.clone(), active.settled());
        drop(installer);
        if settled {
            self.forget_setup(&operation);
        }
        Ok(Some(status))
    }

    fn publish_install_candidate(
        &self,
        workspace_id: &str,
        entry_id: &str,
    ) -> Result<crate::VmCandidatePublication, windows_vm::LaunchError> {
        let (operation, generation, candidate) = self
            .installer()?
            .as_ref()
            .and_then(|active| {
                active
                    .candidate_for(workspace_id, entry_id)
                    .map(|candidate| (active.operation.clone(), active.generation, candidate))
            })
            .ok_or(windows_vm::AgentError::InvalidInstaller)?;
        let result = self.appliance.publish_candidate(
            &operation,
            &candidate.id,
            &vm_published_application_id(&candidate.id),
            &candidate.display_name,
            generation,
        )?;
        let mut installer = self.installer()?;
        let active = installer
            .as_mut()
            .filter(|active| {
                active.operation == operation
                    && active.generation == generation
                    && active.belongs_to(workspace_id)
            })
            .ok_or(windows_vm::AgentError::InvalidInstaller)?;
        active.generation = result.catalog_generation;
        active.candidates.remove(entry_id);
        active
            .status
            .candidates
            .retain(|candidate| candidate.entry_id != entry_id);
        let (status, settled) = (active.status.clone(), active.settled());
        drop(installer);
        if settled {
            self.forget_setup(&operation);
        } else {
            // The candidates left are published against the generation this one moved to.
            self.appliance
                .retain_setup(workspace_id, &operation, result.catalog_generation)?;
        }
        Ok(crate::VmCandidatePublication {
            installation: status,
            published_entry_id: vm_entry_id(&vm_published_application_id(&candidate.id)),
        })
    }

    fn remove_published_application(&self, entry_id: &str) -> Result<(), windows_vm::LaunchError> {
        let catalog = self
            .appliance
            .cached_catalog()?
            .ok_or(windows_vm::LaunchError::PublicationChanged)?;
        let application = catalog
            .applications
            .iter()
            .find(|application| vm_entry_id(&application.id) == entry_id)
            .ok_or(windows_vm::LaunchError::PublicationChanged)?;
        self.appliance
            .remove_publication(&application.id, catalog.generation)?;
        Ok(())
    }

    fn uninstallation_status(&self, workspace_id: &str) -> Option<crate::VmUninstallation> {
        self.uninstaller.lock().ok().and_then(|active| {
            active
                .as_ref()
                .filter(|active| active.belongs_to(workspace_id))
                .map(|active| active.status.clone())
        })
    }

    fn refresh_uninstallation_status(
        &self,
        workspace_id: &str,
    ) -> Result<Option<crate::VmUninstallation>, windows_vm::LaunchError> {
        let active = self
            .uninstaller
            .lock()
            .map_err(|_| {
                windows_vm::LaunchError::CapabilityBroker(
                    "Windows uninstaller state is unavailable".to_string(),
                )
            })?
            .as_ref()
            .filter(|active| active.belongs_to(workspace_id))
            .map(|active| (active.operation.clone(), active.entry_id.clone()));
        let Some((operation, entry_id)) = active else {
            return Ok(None);
        };
        let guest_status = self.appliance.uninstallation_status(&operation)?;
        let mut uninstaller = self.uninstaller.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "Windows uninstaller state is unavailable".to_string(),
            )
        })?;
        let active = uninstaller
            .as_mut()
            .filter(|active| {
                active.operation == operation
                    && active.entry_id == entry_id
                    && active.belongs_to(workspace_id)
            })
            .ok_or(windows_vm::AgentError::InvalidInstaller)?;
        active.status = vm_uninstallation_from_guest(&entry_id, &guest_status);
        let status = active.status.clone();
        drop(uninstaller);
        Ok(Some(status))
    }

    fn uninstall_published_application(
        &self,
        workspace_id: &str,
        entry_id: &str,
    ) -> Result<crate::VmUninstallation, windows_vm::LaunchError> {
        // Serialise the start of uninstall with every launch/install check.
        // Once this transaction is recorded, those paths continue to reject
        // until guest status reaches a terminal state.
        let _launch_guard = self.launch_lock.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "singleton Windows launch check is unavailable".to_string(),
            )
        })?;
        let uninstaller = self.uninstaller.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "Windows uninstaller state is unavailable".to_string(),
            )
        })?;
        if let Some(active) = uninstaller.as_ref()
            && !active.is_terminal()
        {
            return if active.belongs_to(workspace_id) && active.entry_id == entry_id {
                Ok(active.status.clone())
            } else {
                Err(windows_vm::AgentError::InvalidInstaller.into())
            };
        }
        // Do not hold this state lock while presenting the guest surface.
        drop(uninstaller);
        let catalog = self
            .appliance
            .cached_catalog()?
            .ok_or(windows_vm::LaunchError::PublicationChanged)?;
        let application = catalog
            .applications
            .iter()
            .find(|application| {
                vm_entry_id(&application.id) == entry_id && application.uninstall_available
            })
            .ok_or(windows_vm::LaunchError::PublicationChanged)?;
        let prepared = self.appliance.begin_uninstallation(&application.id)?;
        // The uninstaller, like setup, is exposed only as a guest-authenticated
        // app-focused SPICE surface with deny-all brokered capabilities.
        self.appliance
            .present_uninstallation(workspace_id, &prepared)?;
        let guest_status = self
            .appliance
            .uninstallation_status(&prepared.operation_id)?;
        let status = vm_uninstallation_from_guest(entry_id, &guest_status);
        let mut uninstaller = self.uninstaller.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "Windows uninstaller state is unavailable".to_string(),
            )
        })?;
        *uninstaller = Some(ActiveVmUninstallation {
            workspace_id: workspace_id.to_string(),
            entry_id: entry_id.to_string(),
            operation: prepared.operation_id,
            status: status.clone(),
        });
        drop(uninstaller);
        Ok(status)
    }

    fn launch(
        &self,
        request: VmRouteRequest<'_>,
    ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError> {
        let _launch_guard = self.launch_lock.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "singleton Windows launch check is unavailable".to_string(),
            )
        })?;
        self.require_no_active_uninstallation()?;
        // The catalogue itself is the only source allowed to select the VM.
        // This prevents a manager-side route override from using the VM as an
        // arbitrary executable runner.
        if request.route != crate::RouteKind::Vm {
            return Err(windows_vm::LaunchError::InvalidRequest("VM route"));
        }
        let grants = self.portal.grants_for(
            request.app_id,
            request.workspace_id,
            request.resolved_capabilities,
        )?;
        validate_portal_projection(&grants, request.resolved_capabilities)?;
        self.appliance.launch(&windows_vm::LaunchRequest {
            workspace_id: request.workspace_id.to_string(),
            application_id: request.published_application_id.to_string(),
            grants,
        })
    }

    fn launch_with_portal_file(
        &self,
        request: VmRouteRequest<'_>,
        source: File,
        access: windows_vm::PortalDocumentAccess,
    ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError> {
        let _launch_guard = self.launch_lock.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "singleton Windows launch check is unavailable".to_string(),
            )
        })?;
        self.require_no_active_uninstallation()?;
        if request.route != crate::RouteKind::Vm {
            return Err(windows_vm::LaunchError::InvalidRequest("VM route"));
        }
        let mut grants = self.portal.grants_for(
            request.app_id,
            request.workspace_id,
            request.resolved_capabilities,
        )?;
        let document = self.portal.register_portal_file(
            request.app_id,
            request.workspace_id,
            source,
            access,
        )?;
        grants.portal_documents.push(document);
        validate_portal_projection(&grants, request.resolved_capabilities)?;
        self.appliance.launch(&windows_vm::LaunchRequest {
            workspace_id: request.workspace_id.to_string(),
            application_id: request.published_application_id.to_string(),
            grants,
        })
    }
}

/// The manager's entry for the guest's published application `published_id`.
#[must_use]
pub fn vm_entry_id(published_id: &str) -> String {
    format!(
        "{APPLIANCE_ENTRY_PREFIX}{}",
        vm_identity_digest(published_id)
    )
}

/// The app id the windows of the guest's published application `published_id` are shown under.
#[must_use]
pub fn vm_app_id(published_id: &str) -> String {
    format!("so.tilo.windows.vm.{}", vm_identity_digest(published_id))
}

fn vm_identity_digest(published_id: &str) -> String {
    let digest = Sha256::digest(published_id.as_bytes());
    hex::encode(digest.get(..16).unwrap_or(&digest))
}

fn vm_candidate_entry_id(
    operation: &windows_vm::InstallationOperationId,
    candidate_id: &str,
) -> String {
    format!(
        "windows-vm-candidate-{}",
        vm_identity_digest(&format!("{}\0{candidate_id}", operation.as_str()))
    )
}

fn vm_published_application_id(candidate_id: &str) -> String {
    format!("tilo-app-{}", vm_identity_digest(candidate_id))
}

fn candidate_map(
    status: &windows_vm::InstallationStatus,
) -> BTreeMap<String, windows_vm::DiscoveredApplication> {
    status
        .candidates
        .iter()
        .cloned()
        .map(|candidate| {
            (
                vm_candidate_entry_id(&status.operation_id, &candidate.id),
                candidate,
            )
        })
        .collect()
}

fn vm_installation_from_guest(
    operation: &windows_vm::InstallationOperationId,
    status: &windows_vm::InstallationStatus,
) -> VmInstallation {
    let phase = match status.state {
        windows_vm::InstallationState::Starting => VmInstallationPhase::Starting,
        windows_vm::InstallationState::Running => VmInstallationPhase::Running,
        windows_vm::InstallationState::Completed => VmInstallationPhase::Completed,
        windows_vm::InstallationState::Failed => VmInstallationPhase::Failed,
    };
    let candidates: Vec<_> = status
        .candidates
        .iter()
        .map(|candidate| VmInstallCandidate {
            entry_id: vm_candidate_entry_id(operation, &candidate.id),
            display_name: candidate.display_name.clone(),
            publisher: candidate.publisher.clone(),
            version: candidate.version.clone(),
        })
        .collect();
    VmInstallation {
        detail: match phase {
            VmInstallationPhase::Starting => {
                "Windows setup is starting in the integrated app surface.".to_string()
            }
            VmInstallationPhase::Running => {
                "Windows setup is still running in the integrated app surface.".to_string()
            }
            VmInstallationPhase::Completed if candidates.is_empty() => {
                "Setup completed; no publishable application was discovered.".to_string()
            }
            VmInstallationPhase::Completed => {
                "Setup completed. Choose applications to add to Windows Apps.".to_string()
            }
            VmInstallationPhase::Failed => "Windows setup did not complete.".to_string(),
        },
        phase,
        exit_code: status.exit_code,
        candidates,
    }
}

fn vm_uninstallation_from_guest(
    entry_id: &str,
    status: &windows_vm::UninstallationStatus,
) -> crate::VmUninstallation {
    let phase = match status.state {
        windows_vm::InstallationState::Starting => VmInstallationPhase::Starting,
        windows_vm::InstallationState::Running => VmInstallationPhase::Running,
        windows_vm::InstallationState::Completed => VmInstallationPhase::Completed,
        windows_vm::InstallationState::Failed => VmInstallationPhase::Failed,
    };
    let detail = match phase {
        VmInstallationPhase::Starting => "Windows vendor uninstall is starting.".to_string(),
        VmInstallationPhase::Running => "Windows vendor uninstall is running.".to_string(),
        VmInstallationPhase::Completed => "Windows vendor uninstall completed.".to_string(),
        VmInstallationPhase::Failed => {
            "Windows vendor uninstall failed; retry is available.".to_string()
        }
    };
    crate::VmUninstallation {
        entry_id: entry_id.to_string(),
        detail,
        phase,
        exit_code: status.exit_code,
    }
}

fn validate_portal_projection(
    grants: &windows_vm::VmLaunchGrants,
    capabilities: &CapabilityPlan,
) -> Result<(), windows_vm::LaunchError> {
    if grants.network == grants.network_hosts.is_empty()
        || (grants.network && !capabilities.allow_network)
        || grants
            .network_hosts
            .iter()
            .any(|host| !capabilities.net_hosts.contains(host))
    {
        return Err(windows_vm::LaunchError::BrokerEscalated("network"));
    }
    if grants.microphone
        && !capabilities
            .devices
            .contains(&collab_types::caps::DeviceCap::Microphone)
    {
        return Err(windows_vm::LaunchError::BrokerEscalated("microphone"));
    }
    if grants.clipboard
        && !capabilities
            .devices
            .contains(&collab_types::caps::DeviceCap::Clipboard)
    {
        return Err(windows_vm::LaunchError::BrokerEscalated("clipboard"));
    }
    Ok(())
}
