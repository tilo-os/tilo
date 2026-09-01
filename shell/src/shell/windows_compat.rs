//! Session-owned Windows compatibility service integration.
//!
//! The application receives only a connected descriptor. Authority remains
//! in this compositor: a one-shot launcher record authenticates its identity,
//! and workspace capabilities are re-read for every list and launch request.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use compat_service::{
    AudioIsolatedSpiceLauncher, BwrapExecutor, CapabilityAuthority, CapsuleStore,
    CompatSessionHandle, DenyAllVmCapabilityBroker, DenyAllVmPortalAuthority, HostSession,
    LauncherAuthenticator, LiveVmDelegate, ManagedCompatibility, ShippedCataloguePaths,
    SystemResourceBroker, SystemRuntimeRegistry, TrustedLauncherRegistry, VmAvailability,
    VmDelegate, VmRouteRequest, load_shipped_catalogue, spawn_session_service,
};
use windows_vm::{
    ApplianceConfig, ApplicationPresentation, GuestBridgeConfig, HttpsGuestBridge, LibvirtBackend,
    ProvisioningStore, StdCommandRunner, WindowsAppliance,
};

use crate::shell::state::WorkspaceTreeHandle;

struct WorkspaceCapabilityAuthority {
    tree: WorkspaceTreeHandle,
}

impl CapabilityAuthority for WorkspaceCapabilityAuthority {
    fn capabilities_for(
        &self,
        workspace_id: &str,
        _app_id: &str,
    ) -> Result<Vec<collab_types::caps::CapOverride>, String> {
        self.tree
            .read()
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == workspace_id)
            // Return the canonical list including restrictions. The broker's
            // CapabilityPlan filters it to the server-owned application ID.
            .map(|workspace| workspace.capabilities.clone())
            .ok_or_else(|| "workspace is no longer present in this session".to_string())
    }
}

struct CompositorLaunchRegistry;

impl TrustedLauncherRegistry for CompositorLaunchRegistry {
    fn consume(
        &self,
        pid: u32,
        process_start_time: u64,
        app_id: &str,
        workspace_id: &str,
    ) -> Option<BTreeSet<String>> {
        super::launcher::consume_trusted_service_launch(
            pid,
            process_start_time,
            app_id,
            workspace_id,
        )
        .map(|entries| entries.into_iter().collect())
    }
}

struct EntryRegistryRefresh {
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl Drop for EntryRegistryRefresh {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(worker) = self.worker.take() {
            worker.thread().unpark();
            let _ = worker.join();
        }
    }
}

pub struct WindowsCompatHandle {
    #[allow(
        dead_code,
        reason = "dropping it closes the authenticated session service"
    )]
    service: CompatSessionHandle,
    #[allow(dead_code, reason = "dropping it cancels and joins registry refresh")]
    registry_refresh: EntryRegistryRefresh,
}

fn live_vm_delegate() -> Result<Arc<dyn VmDelegate>, String> {
    const ROOT: &str = "/var/lib/qos/windows";
    let domain_uuid = std::fs::read_to_string(format!("{ROOT}/domain.uuid"))
        .map_err(|error| format!("could not read singleton VM identity: {error}"))?;
    let domain_uuid = domain_uuid.trim();
    let presentation = ApplicationPresentation::AppFocusedSpice;
    let configuration = ApplianceConfig::recommended(
        domain_uuid,
        format!("{ROOT}/system.qcow2"),
        format!("{ROOT}/nvram.fd"),
    );
    configuration
        .validate()
        .map_err(|error| format!("invalid singleton VM configuration: {error}"))?;
    let bridge = HttpsGuestBridge::new(GuestBridgeConfig {
        endpoint: "192.168.251.2:49888"
            .parse()
            .map_err(|error| format!("invalid fixed guest endpoint: {error}"))?,
        bootstrap_secret: format!("{ROOT}/bridge/bootstrap.secret").into(),
        pin_directory: format!("{ROOT}/session").into(),
        presentation,
    })
    .map_err(|error| format!("could not open authenticated guest bridge: {error}"))?;
    let runner = StdCommandRunner;
    let appliance = WindowsAppliance::new(
        configuration,
        ProvisioningStore::new(format!("{ROOT}/session/state.json")),
        LibvirtBackend::new(runner),
        bridge,
        DenyAllVmCapabilityBroker::default(),
        AudioIsolatedSpiceLauncher::new(runner),
    );
    Ok(Arc::new(LiveVmDelegate::new(
        appliance,
        DenyAllVmPortalAuthority,
        presentation,
    )))
}

#[derive(Default)]
struct SessionVmDelegate {
    live: Mutex<Option<Arc<dyn VmDelegate>>>,
}

impl SessionVmDelegate {
    fn get(&self) -> Result<Arc<dyn VmDelegate>, String> {
        if let Some(delegate) = self
            .live
            .lock()
            .map_err(|_| "Windows VM delegate state is unavailable".to_string())?
            .clone()
        {
            return Ok(delegate);
        }
        let candidate = live_vm_delegate()?;
        let mut slot = self
            .live
            .lock()
            .map_err(|_| "Windows VM delegate state is unavailable".to_string())?;
        Ok(slot.get_or_insert_with(|| Arc::clone(&candidate)).clone())
    }
}

impl VmDelegate for SessionVmDelegate {
    fn availability(&self) -> VmAvailability {
        match self.get() {
            Ok(delegate) => delegate.availability(),
            Err(error) => VmAvailability::unavailable(error),
        }
    }

    fn launch(
        &self,
        request: VmRouteRequest<'_>,
    ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError> {
        self.get()
            .map_err(windows_vm::AgentError::Transport)?
            .launch(request)
    }
}

fn vm_delegate() -> Box<dyn VmDelegate> {
    Box::new(SessionVmDelegate::default())
}

fn spawn_entry_registry_refresh(store: CapsuleStore) -> Result<EntryRegistryRefresh, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let worker = std::thread::Builder::new()
        .name("qos-windows-registry".to_string())
        .spawn(move || {
            while !thread_stop.load(Ordering::Acquire) {
                std::thread::park_timeout(Duration::from_secs(2));
                if thread_stop.load(Ordering::Acquire) {
                    break;
                }
                let entries = match store.registered_programs() {
                    Ok(entries) => entries,
                    Err(error) => {
                        tracing::warn!(%error, "could not refresh managed Windows entries");
                        continue;
                    }
                };
                let before = super::launcher::windows_compat_entry_generation();
                let generation = super::launcher::set_windows_compat_entry_ids(
                    entries.into_iter().map(|entry| entry.entry_id).collect(),
                );
                if generation != before {
                    // Existing connected managers retain the one-shot entry
                    // snapshot authenticated at their launch. Relaunch is the
                    // explicit refresh boundary; no client can widen itself.
                    tracing::info!(
                        generation,
                        "managed Windows entries changed; relaunch Windows Apps to refresh"
                    );
                }
            }
        })
        .map_err(|error| format!("could not start Windows entry registry refresh: {error}"))?;
    Ok(EntryRegistryRefresh {
        stop,
        worker: Some(worker),
    })
}

/// Start the real session broker. Failure is fail-closed: the optional
/// application service remains absent and Windows Manager shows repair state.
///
/// # Errors
///
/// Returns an error when the catalogue, capsule store, compositor resources,
/// launcher authentication, or bounded service socket cannot be initialized.
pub fn spawn(tree: WorkspaceTreeHandle) -> Result<WindowsCompatHandle, String> {
    let report = load_shipped_catalogue(&ShippedCataloguePaths::default())
        .map_err(|error| error.to_string())?;
    if !report.skipped_bottles_files.is_empty() {
        tracing::warn!(
            skipped = report.skipped_bottles_files.len(),
            "windows compatibility catalogue skipped unsupported Bottles metadata"
        );
    }

    let store = CapsuleStore::new(cli::data_home().join("qos/windows-compat"))
        .map_err(|error| error.to_string())?;
    let allowed_entries = store
        .registered_programs()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|entry| entry.entry_id)
        .collect();
    let _generation = super::launcher::set_windows_compat_entry_ids(allowed_entries);
    let registry_refresh = spawn_entry_registry_refresh(store.clone())?;

    let wayland_socket = super::launcher::wayland_socket()
        .ok_or_else(|| "the compositor Wayland socket is not ready".to_string())?;
    let render_node = crate::compositor::render_node::resolved()
        .map(std::path::Path::to_path_buf)
        .ok_or_else(|| "the compositor has no safe render node to delegate".to_string())?;
    // SAFETY: `geteuid` has no preconditions and does not mutate memory.
    let uid = unsafe { libc::geteuid() };
    let session = HostSession {
        uid,
        wayland_socket,
        render_node,
    };
    let managed = ManagedCompatibility::new(
        report.catalogue,
        store,
        SystemRuntimeRegistry::new(),
        SystemResourceBroker::new(),
        BwrapExecutor,
        vm_delegate(),
        session,
    )
    .map_err(|error| error.to_string())?;
    let launcher = super::launcher::resolve_launcher_bin().map_err(|error| error.to_string())?;
    let authenticator = LauncherAuthenticator::new(
        launcher,
        std::process::id(),
        Arc::new(CompositorLaunchRegistry),
    );
    let service = spawn_session_service(
        launcher::services::socket_for("compat"),
        Arc::new(managed),
        Arc::new(WorkspaceCapabilityAuthority { tree }),
        authenticator,
    )
    .map_err(|error| error.to_string())?;
    Ok(WindowsCompatHandle {
        service,
        registry_refresh,
    })
}
