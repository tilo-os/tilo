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
use std::fs::File;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::{
    AudioProxyLease, VmAvailability, VmDelegate, VmInstallCandidate, VmInstallation,
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

/// Capability broker decorator which records only resources the underlying
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
/// Clipboard is represented by the ordinary `Device("clipboard")` grant and
/// microphone by `Device("microphone")`. Files are deliberately absent: a
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
        let device_granted = |name: &str| {
            self.native_rail && capabilities.devices.iter().any(|device| device == name)
        };
        Ok(windows_vm::VmLaunchGrants {
            network_hosts: if self.native_rail {
                capabilities.net_hosts.clone()
            } else {
                Vec::new()
            },
            network: self.native_rail && !capabilities.net_hosts.is_empty(),
            clipboard: device_granted("clipboard"),
            microphone: device_granted("microphone"),
            portal_documents: Vec::new(),
        })
    }
}

/// Materialises channel-only RAIL resources from the canonical projection.
/// The microphone's actual private `PipeWire` source is created by
/// [`AudioIsolatedRailLauncher`]; this lease controls whether that source and
/// the `FreeRDP` redirectors exist and ensures the shared-session tracker sees
/// only the materialised result.
pub struct CanonicalRailCapabilityBroker {
    next: AtomicU64,
    /// Every launch receives a distinct host listener/token/policy. This
    /// prevents the host relay from silently broadening its requested policy,
    /// but does not make programs in the persistent shared Windows user
    /// session mutually distrustful: active redirects are an effective union
    /// within that guest session.
    network_leases: Mutex<BTreeMap<String, windows_vm::GuestNetworkProxy>>,
}

impl Default for CanonicalRailCapabilityBroker {
    fn default() -> Self {
        Self {
            next: AtomicU64::new(0),
            network_leases: Mutex::new(BTreeMap::new()),
        }
    }
}

impl windows_vm::CapabilityBroker for CanonicalRailCapabilityBroker {
    fn materialize(
        &self,
        request: &windows_vm::LaunchRequest,
    ) -> Result<windows_vm::BrokeredLaunch, windows_vm::LaunchError> {
        if request.grants.network == request.grants.network_hosts.is_empty()
            || !request.grants.portal_documents.is_empty()
        {
            return Err(windows_vm::LaunchError::CapabilityBroker(
                "the Windows appliance received a grant without an exact live broker".to_string(),
            ));
        }
        let sequence = self.next.fetch_add(1, Ordering::Relaxed);
        let lease_id = format!("rail-{}-{sequence}", std::process::id());
        let (network_proxy, network_binding) = if request.grants.network {
            let mut leases = self.network_leases.lock().map_err(|_| {
                windows_vm::LaunchError::CapabilityBroker(
                    "Windows network lease state is unavailable".to_string(),
                )
            })?;
            let proxy = windows_vm::GuestNetworkProxy::start(&request.grants.network_hosts)
                .map_err(|error| windows_vm::LaunchError::CapabilityBroker(error.to_string()))?;
            let binding = proxy.binding().clone();
            if leases.insert(lease_id.clone(), proxy).is_some() {
                return Err(windows_vm::LaunchError::CapabilityBroker(
                    "Windows network lease identifier was reused".to_string(),
                ));
            }
            drop(leases);
            (true, Some(binding))
        } else {
            (false, None)
        };
        Ok(windows_vm::BrokeredLaunch {
            lease_id,
            network: network_proxy,
            network_binding,
            clipboard: request.grants.clipboard,
            microphone: request.grants.microphone,
            shares: Vec::new(),
        })
    }

    fn revoke(&self, lease_id: &str) {
        if let Ok(mut leases) = self.network_leases.lock() {
            // Dropping a lease proxy wakes the listener and terminates all
            // established TCP tunnels belonging to this lease only.
            leases.remove(lease_id);
        }
    }
}

/// Safe authority for the live app-focused SPICE presentation. `RemoteApp`
/// still works with playback, but receives no files, clipboard, microphone,
/// or egress. It remains the required authority until a probed local RAIL
/// surface and canonical broker can carry and revoke those redirects.
#[derive(Clone, Copy, Debug, Default)]
pub struct DenyAllVmPortalAuthority;

impl VmPortalAuthority for DenyAllVmPortalAuthority {
    fn grants_for(
        &self,
        _app_id: &str,
        _workspace_id: &str,
        _capabilities: &CapabilityPlan,
    ) -> Result<windows_vm::VmLaunchGrants, windows_vm::LaunchError> {
        Ok(windows_vm::VmLaunchGrants::default())
    }
}

/// Lease broker for the live app-focused SPICE fallback. That presentation
/// cannot carry RDP redirectors, so it materializes an explicit empty lease
/// instead of pretending requested host resources reached Windows. A future
/// RAIL-only broker must materialise and revoke canonical Tilo grants.
#[derive(Debug, Default)]
pub struct DenyAllVmCapabilityBroker {
    next: AtomicU64,
}

impl windows_vm::CapabilityBroker for DenyAllVmCapabilityBroker {
    fn materialize(
        &self,
        _request: &windows_vm::LaunchRequest,
    ) -> Result<windows_vm::BrokeredLaunch, windows_vm::LaunchError> {
        let sequence = self.next.fetch_add(1, Ordering::Relaxed);
        Ok(windows_vm::BrokeredLaunch::deny_all(format!(
            "spice-{}-{sequence}",
            std::process::id()
        )))
    }

    fn revoke(&self, _lease_id: &str) {}
}

/// App-focused SPICE presentation constrained to a per-launch private audio
/// graph. This live fallback exposes playback only; microphone capture,
/// clipboard, file shares, and egress remain unavailable until a probed local
/// RAIL surface can carry grants from a canonical Tilo broker.
pub struct AudioIsolatedSpiceLauncher<R> {
    runner: R,
    root: PathBuf,
    next: AtomicU64,
    uid: u32,
}

/// Native RAIL presentation through Tilo's rootless Xwayland bridge and a
/// private audio graph. Playback is always present; the proxy publishes a
/// source only when the brokered shared-session lease includes microphone.
pub struct AudioIsolatedRailLauncher<R> {
    runner: R,
    root: PathBuf,
    next: AtomicU64,
    uid: u32,
}

impl<R> AudioIsolatedRailLauncher<R> {
    #[must_use]
    pub fn new(runner: R) -> Self {
        let uid = rustix::process::geteuid().as_raw();
        Self {
            runner,
            root: PathBuf::from(format!("/run/user/{uid}/tilo-windows-vm")),
            next: AtomicU64::new(0),
            uid,
        }
    }
}

impl<R: windows_vm::CommandRunner> windows_vm::RemoteAppLauncher for AudioIsolatedRailLauncher<R> {
    fn launch(
        &self,
        guest: &windows_vm::GuestLaunch,
        brokered: &windows_vm::BrokeredLaunch,
    ) -> Result<windows_vm::RemoteAppProcess, windows_vm::RemoteAppError> {
        let sequence = self.next.fetch_add(1, Ordering::Relaxed);
        let lease = AudioProxyLease::start(
            self.root
                .join(format!("rail-{}-{sequence}", std::process::id())),
            self.uid,
            brokered.microphone,
        )
        .map_err(|error| windows_vm::RemoteAppError::FreeRdp {
            code: None,
            stderr: format!("private audio setup failed: {error}"),
        })?;
        let audio_runtime = lease
            .pipewire_socket()
            .parent()
            .ok_or_else(|| windows_vm::RemoteAppError::FreeRdp {
                code: None,
                stderr: "private audio endpoint has no runtime directory".to_string(),
            })?
            .to_path_buf();
        let request = windows_vm::FreeRdpLauncher::new(())
            .with_binary("/usr/libexec/tilo-windows-rail")
            .command(guest, brokered)?
            .env("PIPEWIRE_RUNTIME_DIR", audio_runtime)
            .env("PIPEWIRE_REMOTE", "pipewire-0")
            .env(
                "PULSE_SERVER",
                format!("unix:{}", lease.pulse_socket().display()),
            );
        let output = self.runner.run(&request)?;
        if !output.success {
            return Err(windows_vm::RemoteAppError::FreeRdp {
                code: output.code,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(windows_vm::RemoteAppProcess {
            exit_code: output.code,
        })
    }
}

impl<R> AudioIsolatedSpiceLauncher<R> {
    #[must_use]
    pub fn new(runner: R) -> Self {
        let uid = rustix::process::geteuid().as_raw();
        Self {
            runner,
            root: PathBuf::from(format!("/run/user/{uid}/tilo-windows-vm")),
            next: AtomicU64::new(0),
            uid,
        }
    }
}

impl<R: windows_vm::CommandRunner> windows_vm::RemoteAppLauncher for AudioIsolatedSpiceLauncher<R> {
    fn launch(
        &self,
        guest: &windows_vm::GuestLaunch,
        brokered: &windows_vm::BrokeredLaunch,
    ) -> Result<windows_vm::RemoteAppProcess, windows_vm::RemoteAppError> {
        let sequence = self.next.fetch_add(1, Ordering::Relaxed);
        let lease = AudioProxyLease::start(
            self.root.join(format!("{}-{sequence}", std::process::id())),
            self.uid,
            false,
        )
        .map_err(|error| windows_vm::RemoteAppError::Spice {
            code: None,
            stderr: format!("private audio setup failed: {error}"),
        })?;
        let audio_runtime = lease
            .pipewire_socket()
            .parent()
            .ok_or_else(|| windows_vm::RemoteAppError::Spice {
                code: None,
                stderr: "private audio endpoint has no runtime directory".to_string(),
            })?
            .to_path_buf();
        let request = windows_vm::AppFocusedSpiceLauncher::new(())
            .command(guest, brokered)?
            .env("PIPEWIRE_RUNTIME_DIR", audio_runtime)
            .env("PIPEWIRE_REMOTE", "pipewire-0")
            .env(
                "PULSE_SERVER",
                format!("unix:{}", lease.pulse_socket().display()),
            );
        let output = self.runner.run(&request)?;
        if !output.success {
            return Err(windows_vm::RemoteAppError::Spice {
                code: output.code,
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
            });
        }
        Ok(windows_vm::RemoteAppProcess {
            exit_code: output.code,
        })
    }
}

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
    launch_gate: Mutex<()>,
    installer: Mutex<Option<ActiveVmInstallation>>,
    uninstaller: Mutex<Option<ActiveVmUninstallation>>,
}

struct ActiveVmUninstallation {
    workspace_id: String,
    entry_id: String,
    operation: windows_vm::UninstallationOperationId,
    status: crate::VmUninstallation,
}

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
        Self {
            appliance,
            portal,
            presentation,
            shared_session,
            launch_gate: Mutex::new(()),
            installer: Mutex::new(None),
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
        let _launch_guard = self.launch_gate.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "singleton Windows launch gate is unavailable".to_string(),
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
        // This is intentionally blocking: setup is visible only through the
        // typed app-focused SPICE surface, never an invisible process or a
        // general Windows desktop.
        self.appliance.present_installation(&prepared)?;
        let guest_status = self.appliance.installation_status(&operation)?;
        let catalog = self.appliance.catalog()?;
        let status = vm_installation_from_guest(&operation, &guest_status);
        let active = ActiveVmInstallation {
            workspace_id: workspace_id.to_string(),
            operation,
            generation: catalog.generation,
            candidates: candidate_map(&guest_status),
            status: status.clone(),
        };
        *self.installer.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "Windows installer state is unavailable".to_string(),
            )
        })? = Some(active);
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
            .installer
            .lock()
            .map_err(|_| {
                windows_vm::LaunchError::CapabilityBroker(
                    "Windows installer state is unavailable".to_string(),
                )
            })?
            .as_ref()
            .filter(|active| active.belongs_to(workspace_id))
            .map(|active| active.operation.clone());
        let Some(operation) = operation else {
            return Ok(None);
        };
        let guest_status = self.appliance.installation_status(&operation)?;
        let mut installer = self.installer.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "Windows installer state is unavailable".to_string(),
            )
        })?;
        let active = installer
            .as_mut()
            .filter(|active| active.operation == operation && active.belongs_to(workspace_id))
            .ok_or(windows_vm::AgentError::InvalidInstaller)?;
        active.candidates = candidate_map(&guest_status);
        active.status = vm_installation_from_guest(&operation, &guest_status);
        let status = active.status.clone();
        drop(installer);
        Ok(Some(status))
    }

    fn publish_install_candidate(
        &self,
        workspace_id: &str,
        entry_id: &str,
    ) -> Result<crate::VmCandidatePublication, windows_vm::LaunchError> {
        let (operation, generation, candidate) = self
            .installer
            .lock()
            .map_err(|_| {
                windows_vm::LaunchError::CapabilityBroker(
                    "Windows installer state is unavailable".to_string(),
                )
            })?
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
        let mut installer = self.installer.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "Windows installer state is unavailable".to_string(),
            )
        })?;
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
        let status = active.status.clone();
        drop(installer);
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
        let _launch_guard = self.launch_gate.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "singleton Windows launch gate is unavailable".to_string(),
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
        self.appliance.present_uninstallation(&prepared)?;
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
        let _launch_guard = self.launch_gate.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "singleton Windows launch gate is unavailable".to_string(),
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
            // `published_application_id` is a server-owned registered
            // program id. `WindowsAppliance` additionally requires it to be
            // present in the authenticated guest catalogue before launch.
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
        let _launch_guard = self.launch_gate.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "singleton Windows launch gate is unavailable".to_string(),
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

fn vm_entry_id(published_id: &str) -> String {
    format!("windows-vm-{}", vm_identity_digest(published_id))
}

fn vm_app_id(published_id: &str) -> String {
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
            .iter()
            .any(|device| device == "microphone")
    {
        return Err(windows_vm::LaunchError::BrokerEscalated("microphone"));
    }
    if grants.clipboard
        && !capabilities
            .devices
            .iter()
            .any(|device| device == "clipboard")
    {
        return Err(windows_vm::LaunchError::BrokerEscalated("clipboard"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portal_projections_are_denied_by_default_and_exact_for_leases() {
        {
            let grants = DenyAllVmPortalAuthority
                .grants_for("app", "workspace", &CapabilityPlan::empty())
                .unwrap();
            assert!(!grants.network);
            assert!(!grants.clipboard);
            assert!(!grants.microphone);
            assert!(grants.portal_documents.is_empty());
        }
        {
            use windows_vm::CapabilityBroker as _;

            let authority =
                CanonicalRailPortalAuthority::new(windows_vm::ApplicationPresentation::NativeRail);
            let capabilities = CapabilityPlan {
                devices: vec!["clipboard".to_string(), "microphone".to_string()],
                allow_network: true,
                net_hosts: vec!["example.test".to_string()],
                ro_paths: vec!["/tmp/document".into()],
                ..CapabilityPlan::empty()
            };
            let grants = authority
                .grants_for("app", "workspace", &capabilities)
                .unwrap();
            assert!(grants.clipboard && grants.microphone);
            assert!(grants.network);
            assert_eq!(grants.network_hosts, vec!["example.test"]);
            assert!(grants.portal_documents.is_empty());

            // The actual host-only address belongs to libvirt, not a unit test
            // process. Exercise the non-network portions here; GuestNetworkProxy
            // has its own loopback listener tests and production fails closed if
            // the dedicated address is not available.
            let grants = windows_vm::VmLaunchGrants {
                network: false,
                network_hosts: Vec::new(),
                ..grants
            };
            let lease = CanonicalRailCapabilityBroker::default()
                .materialize(&windows_vm::LaunchRequest {
                    workspace_id: "workspace".to_string(),
                    application_id: "published.app".to_string(),
                    grants,
                })
                .unwrap();
            assert!(lease.clipboard && lease.microphone);
            assert!(!lease.network && lease.shares.is_empty());
        }
        {
            use windows_vm::CapabilityBroker as _;
            let broker = DenyAllVmCapabilityBroker::default();
            let lease = broker
                .materialize(&windows_vm::LaunchRequest {
                    workspace_id: "workspace".to_string(),
                    application_id: "published.app".to_string(),
                    grants: windows_vm::VmLaunchGrants::default(),
                })
                .unwrap();
            lease.validate().unwrap();
            assert!(!lease.network && !lease.clipboard && !lease.microphone);
            assert!(lease.shares.is_empty());
        }
        {
            let result = validate_portal_projection(
                &windows_vm::VmLaunchGrants {
                    network_hosts: vec!["example.test".to_string()],
                    network: true,
                    clipboard: false,
                    microphone: true,
                    portal_documents: Vec::new(),
                },
                &CapabilityPlan::empty(),
            );
            assert!(matches!(
                result,
                Err(windows_vm::LaunchError::BrokerEscalated("network"))
            ));

            let result = validate_portal_projection(
                &windows_vm::VmLaunchGrants {
                    network_hosts: Vec::new(),
                    network: false,
                    clipboard: false,
                    microphone: true,
                    portal_documents: Vec::new(),
                },
                &CapabilityPlan::empty(),
            );
            assert!(matches!(
                result,
                Err(windows_vm::LaunchError::BrokerEscalated("microphone"))
            ));
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn guest_identities_and_statuses_stay_host_owned_and_workspace_scoped() {
        {
            use windows_vm::CapabilityBroker as _;

            struct Broker;
            impl windows_vm::CapabilityBroker for Broker {
                fn materialize(
                    &self,
                    _: &windows_vm::LaunchRequest,
                ) -> Result<windows_vm::BrokeredLaunch, windows_vm::LaunchError> {
                    Ok(windows_vm::BrokeredLaunch {
                        lease_id: "server-lease".to_string(),
                        network: false,
                        network_binding: None,
                        clipboard: true,
                        microphone: false,
                        shares: vec![windows_vm::ShareBinding {
                            drive_name: "TILO1".to_string(),
                            portal_document_id: "portal-document".to_string(),
                            host_path: windows_vm::portal_lease_root()
                                .join("server-lease/portal-document"),
                            read_only: true,
                        }],
                    })
                }

                fn revoke(&self, _: &str) {}
            }

            let tracker = VmSharedSessionTracker::default();
            tracker.register_catalog(&windows_vm::Catalog {
                generation: 1,
                applications: vec![windows_vm::PublishedApplication {
                    id: "published.app".to_string(),
                    display_name: "Published app".to_string(),
                    remote_app_alias: "published.app".to_string(),
                    icon_sha256: None,
                    associations: Vec::new(),
                    uninstall_available: false,
                }],
            });
            let broker = SharedSessionCapabilityBroker::new(Broker, tracker.clone());
            let request = windows_vm::LaunchRequest {
                workspace_id: "workspace".to_string(),
                application_id: "published.app".to_string(),
                grants: windows_vm::VmLaunchGrants {
                    network_hosts: Vec::new(),
                    network: false,
                    clipboard: true,
                    microphone: false,
                    portal_documents: vec![windows_vm::PortalDocumentGrant {
                        handle: "portal-document".to_string(),
                        access: windows_vm::PortalDocumentAccess::ReadOnly,
                    }],
                },
            };
            let lease = broker.materialize(&request).unwrap();
            let summary = tracker.summary();
            assert!(summary.active);
            assert_eq!(
                summary.effective_grants,
                vec![
                    VmSharedSessionGrantKind::AudioPlayback,
                    VmSharedSessionGrantKind::Clipboard,
                    VmSharedSessionGrantKind::PortalFilesReadOnly,
                ]
            );
            assert_eq!(
                summary.contributors,
                vec![VmSharedSessionContributor {
                    published_application_id: "published.app".to_string(),
                    display_name: "Published app".to_string(),
                }]
            );
            broker.revoke(&lease.lease_id);
            assert_eq!(tracker.summary(), VmSharedSessionLeaseSummary::default());
        }
        {
            let entry = vm_entry_id("guest-word");
            let app = vm_app_id("guest-word");
            assert!(entry.starts_with("windows-vm-"));
            assert!(app.starts_with("so.tilo.windows.vm."));
            assert_ne!(entry, "guest-word");
            assert_ne!(app, "guest-word");
            assert_eq!(entry, vm_entry_id("guest-word"));
            assert_ne!(entry, vm_entry_id("guest-excel"));
        }
        {
            let operation =
                windows_vm::InstallationOperationId::parse("installer-operation-a").unwrap();
            let candidate = windows_vm::DiscoveredApplication {
                id: "guest-app".to_string(),
                display_name: "Guest app".to_string(),
                product_name: None,
                publisher: None,
                version: None,
                uninstall: None,
            };
            let candidate_entry = vm_candidate_entry_id(&operation, &candidate.id);
            let active = ActiveVmInstallation {
                workspace_id: "workspace-a".to_string(),
                operation,
                generation: 1,
                candidates: BTreeMap::from([(candidate_entry.clone(), candidate)]),
                status: VmInstallation {
                    detail: "Running".to_string(),
                    phase: VmInstallationPhase::Running,
                    exit_code: None,
                    candidates: Vec::new(),
                },
            };

            assert!(active.status_for("workspace-a").is_some());
            assert!(
                active
                    .candidate_for("workspace-a", &candidate_entry)
                    .is_some()
            );
            assert!(active.status_for("workspace-b").is_none());
            assert!(
                active
                    .candidate_for("workspace-b", &candidate_entry)
                    .is_none()
            );
        }
        {
            let operation =
                windows_vm::UninstallationOperationId::parse("uninstall-operation-a").unwrap();
            let running = vm_uninstallation_from_guest(
                "windows-vm-entry",
                &windows_vm::UninstallationStatus {
                    operation_id: operation.clone(),
                    state: windows_vm::InstallationState::Running,
                    exit_code: None,
                    catalog_generation: None,
                },
            );
            assert_eq!(running.phase, VmInstallationPhase::Running);
            assert!(running.detail.contains("running"));

            let failed = vm_uninstallation_from_guest(
                "windows-vm-entry",
                &windows_vm::UninstallationStatus {
                    operation_id: operation,
                    state: windows_vm::InstallationState::Failed,
                    exit_code: Some(1),
                    catalog_generation: None,
                },
            );
            assert_eq!(failed.phase, VmInstallationPhase::Failed);
            assert_eq!(failed.exit_code, Some(1));
            assert!(failed.detail.contains("retry"));
        }
    }
}
