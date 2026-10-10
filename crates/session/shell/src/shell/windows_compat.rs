//! Session-owned Windows compatibility service integration.
//!
//! The application receives only a connected descriptor. Authority remains
//! in this compositor: a one-shot launcher record authenticates its identity,
//! and workspace capabilities are re-read for every list and launch request.

use std::collections::BTreeSet;
use std::fs::File;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use compat_service::{
    AudioIsolatedRailLauncher, AudioIsolatedSpiceLauncher, BwrapExecutor, CapabilityAuthority,
    CapsuleStore, CompatSessionHandle, HostSession, LauncherAuthenticator, LiveVmDelegate,
    ManagedCompatibility, PortalRailPortalAuthority, SharedSessionCapabilityBroker,
    ShippedCataloguePaths, SystemResourceBroker, SystemRuntimeRegistry, TrustedLauncherRegistry,
    VmAvailability, VmCandidatePublication, VmDelegate, VmInstallation, VmInstallerInput,
    VmInventory, VmRouteRequest, VmSharedSessionTracker, VmUninstallation, load_shipped_catalogue,
    spawn_session_service,
};
use windows_vm::{
    ApplianceConfig, ApplicationPresentation, GuestBridgeConfig, HttpsGuestBridge, LibvirtBackend,
    PortalDocumentStore, PortalRailCapabilityBroker, ProvisioningStore, StdCommandRunner,
    WindowsAppliance,
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
            .desktop
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
    /// Dropping it closes the authenticated session service.
    _service: CompatSessionHandle,
    /// Dropping it cancels and joins registry refresh.
    _registry_refresh: EntryRegistryRefresh,
}

struct SessionPresentationLauncher {
    rail: AudioIsolatedRailLauncher<StdCommandRunner>,
    spice: AudioIsolatedSpiceLauncher<StdCommandRunner>,
}

impl windows_vm::RemoteAppLauncher for SessionPresentationLauncher {
    fn launch(
        &self,
        guest: &windows_vm::GuestLaunch,
        brokered: &windows_vm::BrokeredLaunch,
    ) -> Result<windows_vm::RemoteAppProcess, windows_vm::RemoteAppError> {
        match guest.presentation {
            ApplicationPresentation::NativeRail => {
                windows_vm::RemoteAppLauncher::launch(&self.rail, guest, brokered)
            }
            ApplicationPresentation::AppFocusedSpice => {
                windows_vm::RemoteAppLauncher::launch(&self.spice, guest, brokered)
            }
        }
    }
}

fn native_rail_surface_available() -> bool {
    std::process::Command::new("/usr/libexec/tilo-windows-rail")
        .arg("--probe")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

fn live_vm_delegate() -> Result<Arc<dyn VmDelegate>, String> {
    const ROOT: &str = "/var/lib/tilo/windows";
    let domain_uuid = std::fs::read_to_string(format!("{ROOT}/domain.uuid"))
        .map_err(|error| format!("could not read singleton VM identity: {error}"))?;
    let domain_uuid = domain_uuid.trim();
    let presentation = ApplicationPresentation::select(native_rail_surface_available());
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
    let shared_session = VmSharedSessionTracker::default();
    let portal_documents = Arc::new(PortalDocumentStore::default());
    let appliance = WindowsAppliance::new(
        configuration,
        ProvisioningStore::new(format!("{ROOT}/session/state.json")),
        LibvirtBackend::new(runner),
        bridge,
        SharedSessionCapabilityBroker::new(
            PortalRailCapabilityBroker::new(Arc::clone(&portal_documents)),
            shared_session.clone(),
        ),
        SessionPresentationLauncher {
            rail: AudioIsolatedRailLauncher::new(runner),
            spice: AudioIsolatedSpiceLauncher::new(runner),
        },
    );
    Ok(Arc::new(LiveVmDelegate::with_shared_session_tracker(
        appliance,
        PortalRailPortalAuthority::new(presentation, portal_documents),
        presentation,
        shared_session,
    )))
}

#[derive(Default)]
struct SessionVmDelegate {
    live: Mutex<Option<Arc<dyn VmDelegate>>>,
}

impl SessionVmDelegate {
    fn get(&self) -> Result<Arc<dyn VmDelegate>, String> {
        let current = self
            .live
            .lock()
            .map_err(|_| "Windows VM delegate state is unavailable".to_string())?
            .clone();
        if let Some(delegate) = current {
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
        self.get()
            .map_err(windows_vm::AgentError::Transport)?
            .refresh_installation_status(workspace_id)
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
        self.get()
            .map_err(windows_vm::AgentError::Transport)?
            .refresh_uninstallation_status(workspace_id)
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
}

fn spawn_entry_registry_refresh(
    store: CapsuleStore,
    vm: Arc<dyn VmDelegate>,
) -> Result<EntryRegistryRefresh, String> {
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let worker = std::thread::Builder::new()
        .name("tilo-windows-registry".to_string())
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
                let vm_entries = vm
                    .inventory()
                    .applications
                    .into_iter()
                    .map(|application| application.entry_id);
                let before = super::launcher::windows_compat_entry_generation();
                let generation = super::launcher::set_windows_compat_entry_ids(
                    entries
                        .into_iter()
                        .map(|entry| entry.entry_id)
                        .chain(vm_entries)
                        .collect(),
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

/// Start the session broker. Failure is fail-closed: the optional
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

    let store = CapsuleStore::new(cli::data_home().join("tilo/windows-compat"))
        .map_err(|error| error.to_string())?;
    let vm: Arc<dyn VmDelegate> = Arc::new(SessionVmDelegate::default());
    let allowed_entries = store
        .registered_programs()
        .map_err(|error| error.to_string())?
        .into_iter()
        .map(|entry| entry.entry_id)
        .chain(
            vm.inventory()
                .applications
                .into_iter()
                .map(|application| application.entry_id),
        )
        .collect();
    let _generation = super::launcher::set_windows_compat_entry_ids(allowed_entries);
    let registry_refresh = spawn_entry_registry_refresh(store.clone(), Arc::clone(&vm))?;

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
        vm,
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
        _service: service,
        _registry_refresh: registry_refresh,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU8, Ordering};

    use super::*;

    const INSTALLED: u8 = 1;
    const STATUS: u8 = 2;
    const REFRESHED: u8 = 4;
    const PUBLISHED: u8 = 8;
    const REMOVED: u8 = 16;
    const UNINSTALLED: u8 = 32;
    const UNINSTALL_STATUS: u8 = 64;
    const UNINSTALL_REFRESHED: u8 = 128;

    struct RecordingVmDelegate {
        calls: AtomicU8,
    }

    impl RecordingVmDelegate {
        fn installation() -> VmInstallation {
            VmInstallation {
                detail: "test installation".to_string(),
                phase: compat_service::VmInstallationPhase::Completed,
                exit_code: Some(0),
                candidates: Vec::new(),
            }
        }
    }

    impl VmDelegate for RecordingVmDelegate {
        fn install_from_portal(
            &self,
            _workspace_id: &str,
            _input: VmInstallerInput,
            _source: File,
        ) -> Result<VmInstallation, windows_vm::LaunchError> {
            self.calls.fetch_or(INSTALLED, Ordering::AcqRel);
            Ok(Self::installation())
        }

        fn installation_status(&self, _workspace_id: &str) -> Option<VmInstallation> {
            self.calls.fetch_or(STATUS, Ordering::AcqRel);
            Some(Self::installation())
        }

        fn refresh_installation_status(
            &self,
            _workspace_id: &str,
        ) -> Result<Option<VmInstallation>, windows_vm::LaunchError> {
            self.calls.fetch_or(REFRESHED, Ordering::AcqRel);
            Ok(Some(Self::installation()))
        }

        fn publish_install_candidate(
            &self,
            _workspace_id: &str,
            _entry_id: &str,
        ) -> Result<VmCandidatePublication, windows_vm::LaunchError> {
            self.calls.fetch_or(PUBLISHED, Ordering::AcqRel);
            Ok(VmCandidatePublication {
                installation: Self::installation(),
                published_entry_id: "windows-vm-published".to_string(),
            })
        }

        fn uninstallation_status(&self, _workspace_id: &str) -> Option<VmUninstallation> {
            self.calls.fetch_or(UNINSTALL_STATUS, Ordering::AcqRel);
            Some(VmUninstallation {
                entry_id: "windows-vm-published".to_string(),
                detail: "test uninstall".to_string(),
                phase: compat_service::VmInstallationPhase::Completed,
                exit_code: Some(0),
            })
        }

        fn refresh_uninstallation_status(
            &self,
            _workspace_id: &str,
        ) -> Result<Option<VmUninstallation>, windows_vm::LaunchError> {
            self.calls.fetch_or(UNINSTALL_REFRESHED, Ordering::AcqRel);
            Ok(self.uninstallation_status("workspace"))
        }

        fn remove_published_application(
            &self,
            _entry_id: &str,
        ) -> Result<(), windows_vm::LaunchError> {
            self.calls.fetch_or(REMOVED, Ordering::AcqRel);
            Ok(())
        }

        fn uninstall_published_application(
            &self,
            _workspace_id: &str,
            _entry_id: &str,
        ) -> Result<VmUninstallation, windows_vm::LaunchError> {
            self.calls.fetch_or(UNINSTALLED, Ordering::AcqRel);
            Ok(self.uninstallation_status("workspace").unwrap())
        }

        fn launch(
            &self,
            _request: VmRouteRequest<'_>,
        ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError> {
            Err(windows_vm::AgentError::Unavailable.into())
        }
    }

    #[test]
    fn session_vm_delegate_forwards_vm_installer_lifecycle() {
        let inner = Arc::new(RecordingVmDelegate {
            calls: AtomicU8::new(0),
        });
        let delegate = SessionVmDelegate {
            live: Mutex::new(Some(Arc::clone(&inner) as Arc<dyn VmDelegate>)),
        };
        let input = VmInstallerInput {
            source_name: "setup.exe".to_string(),
            display_name: "Setup".to_string(),
            sha256: "a".repeat(64),
            byte_len: 1,
            kind: compat_service::VmInstallerKind::Exe,
        };

        delegate
            .install_from_portal("workspace", input, File::open("/dev/null").unwrap())
            .unwrap();
        assert!(delegate.installation_status("workspace").is_some());
        assert!(
            delegate
                .refresh_installation_status("workspace")
                .unwrap()
                .is_some()
        );
        assert!(delegate.uninstallation_status("workspace").is_some());
        assert!(
            delegate
                .refresh_uninstallation_status("workspace")
                .unwrap()
                .is_some()
        );
        delegate
            .publish_install_candidate("workspace", "candidate")
            .unwrap();
        delegate
            .remove_published_application("application")
            .unwrap();
        delegate
            .uninstall_published_application("workspace", "application")
            .unwrap();

        assert_eq!(
            inner.calls.load(Ordering::Acquire),
            INSTALLED
                | STATUS
                | REFRESHED
                | PUBLISHED
                | REMOVED
                | UNINSTALLED
                | UNINSTALL_STATUS
                | UNINSTALL_REFRESHED
        );
    }
}
