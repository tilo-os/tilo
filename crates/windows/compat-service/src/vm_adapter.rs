//! Live adapter from qOS's compatibility service to the one Windows appliance.
//!
//! The adapter owns no Windows policy.  In particular it cannot turn a host
//! path, a catalogue recipe, or a manager request into an RDP drive.  The
//! existing privileged portal/capability service supplies an already-resolved
//! [`windows_vm::VmLaunchGrants`] projection; `windows-vm` then materialises
//! and revokes the corresponding lease around the `RemoteApp` process.

use launcher::capabilities::CapabilityPlan;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::File;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{
    AudioProxyLease, VmAvailability, VmDelegate, VmInstallCandidate, VmInstallation,
    VmInstallationPhase, VmInstallerInput, VmInstallerKind, VmInventory, VmPublishedApplication,
    VmRouteRequest,
};

/// Privileged qOS authority for the capability projection used by the Windows
/// appliance.  It is intentionally an injected adapter rather than a second
/// permission database: implementations use the ordinary portal document
/// handles and workspace decision records already owned by qOS.
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
}

/// Safe bootstrap authority for deployments where the privileged qOS portal
/// bridge has not been installed yet.  `RemoteApp` itself still works, but it
/// receives no files, clipboard, microphone, or egress.
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

/// Lease broker for the app-focused SPICE fallback. That presentation cannot
/// safely carry RDP redirectors, so it materializes an explicit empty lease
/// instead of pretending requested host resources reached Windows.
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
/// graph. This SPICE fallback exposes playback only; microphone capture remains
/// unavailable until its brokered grant can be carried without weakening the
/// generic viewer's conservative redirect policy.
pub struct AudioIsolatedSpiceLauncher<R> {
    runner: R,
    root: PathBuf,
    next: AtomicU64,
    uid: u32,
}

impl<R> AudioIsolatedSpiceLauncher<R> {
    #[must_use]
    pub fn new(runner: R) -> Self {
        // SAFETY: libc get-effective-uid has no preconditions and does not
        // dereference memory; it reads process credentials.
        let uid = unsafe { libc::geteuid() };
        Self {
            runner,
            root: PathBuf::from(format!("/run/user/{uid}/qos-windows-vm")),
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

/// Concrete delegate installed in qOS's privileged compatibility host.
///
/// `WindowsAppliance` provides the singleton libvirt backend, authenticated
/// guest bridge, catalogue validation, selected RAIL/SPICE presentation, and
/// short-lived resource revocation.  This type supplies the missing service
/// glue: only the catalogue-selected published application id crosses the
/// boundary and only the normal qOS portal may project grants.
pub struct LiveVmDelegate<B, A, C, L, P> {
    appliance: windows_vm::WindowsAppliance<B, A, C, L>,
    portal: P,
    /// The host surface chosen by the authenticated guest bridge adapter.
    /// It is descriptive for availability; `WindowsAppliance::launch` still
    /// verifies the guest's actual response before a viewer is started.
    presentation: windows_vm::ApplicationPresentation,
    launch_gate: Mutex<()>,
    installer: Mutex<Option<ActiveVmInstallation>>,
}

struct ActiveVmInstallation {
    operation: windows_vm::InstallationOperationId,
    generation: u64,
    candidates: BTreeMap<String, windows_vm::DiscoveredApplication>,
    status: VmInstallation,
}

impl<B, A, C, L, P> LiveVmDelegate<B, A, C, L, P> {
    #[must_use]
    pub fn new(
        appliance: windows_vm::WindowsAppliance<B, A, C, L>,
        portal: P,
        presentation: windows_vm::ApplicationPresentation,
    ) -> Self {
        Self {
            appliance,
            portal,
            presentation,
            launch_gate: Mutex::new(()),
            installer: Mutex::new(None),
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
        match self.appliance.launch_readiness() {
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
        }
    }

    fn inventory(&self) -> VmInventory {
        let availability = self.availability();
        let applications = match self.appliance.cached_catalog() {
            Ok(Some(catalog)) => catalog
                .applications
                .iter()
                .map(|application| VmPublishedApplication {
                    entry_id: vm_entry_id(&application.id),
                    app_id: vm_app_id(&application.id),
                    published_application_id: application.id.clone(),
                    display_name: application.display_name.clone(),
                    presentation: self.presentation.to_string(),
                    uninstall_available: application.uninstall_available,
                })
                .collect(),
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
        input: VmInstallerInput,
        source: File,
    ) -> Result<VmInstallation, windows_vm::LaunchError> {
        let _launch_guard = self.launch_gate.lock().map_err(|_| {
            windows_vm::LaunchError::CapabilityBroker(
                "singleton Windows launch gate is unavailable".to_string(),
            )
        })?;
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

    fn installation_status(&self) -> Option<VmInstallation> {
        self.installer
            .lock()
            .ok()
            .and_then(|active| active.as_ref().map(|active| active.status.clone()))
    }

    fn refresh_installation_status(
        &self,
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
            .filter(|active| active.operation == operation)
            .ok_or(windows_vm::AgentError::InvalidInstaller)?;
        active.candidates = candidate_map(&guest_status);
        active.status = vm_installation_from_guest(&operation, &guest_status);
        let status = active.status.clone();
        drop(installer);
        Ok(Some(status))
    }

    fn publish_install_candidate(
        &self,
        entry_id: &str,
    ) -> Result<VmInstallation, windows_vm::LaunchError> {
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
                    .candidates
                    .get(entry_id)
                    .cloned()
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
            .filter(|active| active.operation == operation && active.generation == generation)
            .ok_or(windows_vm::AgentError::InvalidInstaller)?;
        active.generation = result.catalog_generation;
        active.candidates.remove(entry_id);
        active
            .status
            .candidates
            .retain(|candidate| candidate.entry_id != entry_id);
        let status = active.status.clone();
        drop(installer);
        Ok(status)
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

    fn uninstall_published_application(
        &self,
        entry_id: &str,
    ) -> Result<(), windows_vm::LaunchError> {
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
        let _status = self
            .appliance
            .uninstallation_status(&prepared.operation_id)?;
        Ok(())
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
}

fn vm_entry_id(published_id: &str) -> String {
    format!("windows-vm-{}", vm_identity_digest(published_id))
}

fn vm_app_id(published_id: &str) -> String {
    format!("dev.qos.windows.vm.{}", vm_identity_digest(published_id))
}

fn vm_identity_digest(published_id: &str) -> String {
    let digest = Sha256::digest(published_id.as_bytes());
    let mut encoded = String::with_capacity(32);
    for byte in digest.iter().take(16) {
        // Writing into a String cannot fail.
        let _ = write!(encoded, "{byte:02x}");
    }
    encoded
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
    format!("qos-app-{}", vm_identity_digest(candidate_id))
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

fn validate_portal_projection(
    grants: &windows_vm::VmLaunchGrants,
    capabilities: &CapabilityPlan,
) -> Result<(), windows_vm::LaunchError> {
    if grants.network && !capabilities.allow_network {
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
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_portal_projection_is_fully_denied() {
        let grants = DenyAllVmPortalAuthority
            .grants_for("app", "workspace", &CapabilityPlan::empty())
            .unwrap();
        assert!(!grants.network);
        assert!(!grants.clipboard);
        assert!(!grants.microphone);
        assert!(grants.portal_documents.is_empty());
    }

    #[test]
    fn app_focused_broker_materializes_an_explicit_empty_lease() {
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

    #[test]
    fn a_portal_cannot_project_ungranted_network_or_microphone() {
        let result = validate_portal_projection(
            &windows_vm::VmLaunchGrants {
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

    #[test]
    fn published_guest_identity_is_mapped_to_host_owned_opaque_ids() {
        let entry = vm_entry_id("guest-word");
        let app = vm_app_id("guest-word");
        assert!(entry.starts_with("windows-vm-"));
        assert!(app.starts_with("dev.qos.windows.vm."));
        assert_ne!(entry, "guest-word");
        assert_ne!(app, "guest-word");
        assert_eq!(entry, vm_entry_id("guest-word"));
        assert_ne!(entry, vm_entry_id("guest-excel"));
    }
}
