use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use collab_types::caps::CapOverride;
use compat_catalogue::{
    ApplicationQuery, Catalogue, CatalogueError, LaunchPlan, Layer, MatchKind, Runtime, VmFallback,
};
use launcher::capabilities::CapabilityPlan;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{
    CapsuleCommandPlan, CapsuleError, CapsuleProgram, CapsuleState, CapsuleStore, ExecutionError,
    ExecutionOutcome, HostSession, InstallationRecord, InstallationStatus, InstallerError,
    InstallerInspection, InstallerKind, LaunchResourceBroker, MAX_INSTALLER_BYTES, NetworkShape,
    PlanError, ProgramKind, ResourceError, ResourceRequest, RuntimeRegistry, SandboxExecutor,
    SandboxPlanner, inspect_installer,
};

/// One application published by the authenticated singleton Windows guest.
/// All identifiers are minted by Tilo from the guest's opaque publication ID;
/// they are never executable paths or client-selected launch arguments.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmPublishedApplication {
    pub entry_id: String,
    pub app_id: String,
    pub published_application_id: String,
    pub display_name: String,
    /// The authenticated presentation selected by the appliance.  This is a
    /// capability fact, not display text: only a native RAIL surface can
    /// carry the portal's per-file redirector.
    pub presentation: VmPresentation,
    /// Guest-persisted capability; it is never inferred from a transient
    /// installer operation or a host-side command.
    pub uninstall_available: bool,
}

/// Capability-relevant presentation of a published Windows application.
///
/// Keep this separate from the display string so clients cannot accidentally
/// enable file opening merely because a human-facing runner name changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmPresentation {
    NativeRail,
    AppFocusedSpice,
}

impl VmPresentation {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::NativeRail => "native RAIL",
            Self::AppFocusedSpice => "app-focused whole-surface SPICE",
        }
    }

    #[must_use]
    pub const fn supports_portal_files(self) -> bool {
        matches!(self, Self::NativeRail)
    }
}

/// Read-only manager snapshot of the singleton appliance. Implementations
/// must obtain this from cached authenticated state and must not start a guest
/// merely to enumerate applications.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmInventory {
    pub availability: VmAvailability,
    pub applications: Vec<VmPublishedApplication>,
}

/// The inspected, digest-bound package metadata passed to the singleton VM.
/// It contains neither a host path nor an installer command line.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmInstallerInput {
    pub source_name: String,
    pub display_name: String,
    pub sha256: String,
    pub byte_len: u64,
    pub kind: VmInstallerKind,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VmInstallerKind {
    Exe,
    Msi,
}

/// Opaque, server-owned candidate from the guest's discovered application
/// list. Its ID is only meaningful to the delegate that issued it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmInstallCandidate {
    pub entry_id: String,
    pub display_name: String,
    pub publisher: Option<String>,
    pub version: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmInstallationPhase {
    Starting,
    Running,
    Completed,
    Failed,
}

/// One bounded, singleton installer operation. The guest operation handle is
/// retained by the privileged delegate; manager clients see only this opaque
/// status and candidate entry IDs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmInstallation {
    pub detail: String,
    pub phase: VmInstallationPhase,
    pub exit_code: Option<u32>,
    pub candidates: Vec<VmInstallCandidate>,
}

/// The host-owned entry created by publishing one installer candidate.  The
/// entry ID is intentionally returned only to the authenticated workspace
/// that owns the active installer transaction.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VmCandidatePublication {
    pub installation: VmInstallation,
    pub published_entry_id: String,
}

/// Status of one guest-authorized vendor uninstall. The guest operation ID
/// remains in the privileged delegate; clients use only the published host
/// entry ID that they were already authorized to manage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmUninstallation {
    pub entry_id: String,
    pub detail: String,
    pub phase: VmInstallationPhase,
    pub exit_code: Option<u32>,
}

/// A local confirmation is required before the exact portal file is streamed
/// to Windows. It is the only outcome that can select the VM installer path.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmInstallConfirmation {
    pub display_name: String,
    pub detail: String,
    pub recommended_route: RouteKind,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum InstallOutcome {
    Confirmation {
        confirmation: VmInstallConfirmation,
        input: VmInstallerInput,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteKind {
    Wine,
    Proton,
    Vm,
}

impl From<Layer> for RouteKind {
    fn from(value: Layer) -> Self {
        match value {
            Layer::Wine => Self::Wine,
            Layer::Proton => Self::Proton,
            Layer::Vm => Self::Vm,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchReason {
    DefaultWine,
    ProductPublisher,
    Executable,
    ExecutableDigest,
    GameId,
    StoreProduct,
    AppId,
    PackageId,
}

impl From<MatchKind> for MatchReason {
    fn from(value: MatchKind) -> Self {
        match value {
            MatchKind::DefaultWine => Self::DefaultWine,
            MatchKind::ProductPublisher => Self::ProductPublisher,
            MatchKind::Executable => Self::Executable,
            MatchKind::ExecutableDigest => Self::ExecutableDigest,
            MatchKind::GameId => Self::GameId,
            MatchKind::StoreProduct => Self::StoreProduct,
            MatchKind::AppId => Self::AppId,
            MatchKind::PackageId => Self::PackageId,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccessSummary {
    pub files_read_only: usize,
    pub files_read_write: usize,
    pub network: String,
    pub microphone: bool,
    pub audio_output: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ExplainStatus {
    pub route: RouteKind,
    pub catalogue_record: Option<String>,
    pub match_reason: MatchReason,
    pub capsule_id: Option<String>,
    pub runner: String,
    pub security_profile: String,
    pub vm_fallback_was_explicit: bool,
    pub access: AccessSummary,
    /// Human-facing reason suitable for the Windows manager details panel.
    pub summary: String,
}

#[derive(Clone, Debug)]
pub struct LaunchInput {
    pub app_id: String,
    pub workspace_id: String,
    pub program_id: String,
    pub query: ApplicationQuery,
    pub vm_fallback: VmFallback,
    /// Canonical Tilo capability records, not a compatibility-local policy.
    pub capabilities: Vec<CapOverride>,
    pub now_unix: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LaunchReceipt {
    pub explanation: ExplainStatus,
    pub success: bool,
    pub exit_code: Option<i32>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InstallReceipt {
    pub capsule_id: String,
    pub display_name: String,
    pub route: RouteKind,
    pub status: InstallationStatus,
    pub discovered_programs: usize,
    pub exit_code: Option<i32>,
    pub detail: String,
}

pub struct VmRouteRequest<'a> {
    pub route: RouteKind,
    pub app_id: &'a str,
    pub workspace_id: &'a str,
    pub published_application_id: &'a str,
    pub capabilities: &'a [CapOverride],
    pub resolved_capabilities: &'a CapabilityPlan,
}

/// Current readiness of the one Windows fallback appliance.  This is a
/// status value, not a permission: a successful launch still needs the
/// explicit VM fallback action and an authenticated guest catalogue match.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmAvailability {
    pub available: bool,
    pub detail: String,
    /// Server-derived union of resources currently visible inside the one
    /// shared Windows user/session. It is never accepted from a manager
    /// request, because Windows applications in that session are not mutually
    /// isolated from one another.
    #[serde(default)]
    pub shared_session: VmSharedSessionLeaseSummary,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VmSharedSessionGrantKind {
    AudioPlayback,
    Clipboard,
    Microphone,
    NetworkEgress,
    PortalFilesReadOnly,
    PortalFilesReadWrite,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmSharedSessionContributor {
    pub published_application_id: String,
    pub display_name: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct VmSharedSessionLeaseSummary {
    pub active: bool,
    pub effective_grants: Vec<VmSharedSessionGrantKind>,
    pub contributors: Vec<VmSharedSessionContributor>,
}

impl VmAvailability {
    #[must_use]
    pub fn ready(detail: impl Into<String>) -> Self {
        Self {
            available: true,
            detail: detail.into(),
            shared_session: VmSharedSessionLeaseSummary::default(),
        }
    }

    #[must_use]
    pub fn unavailable(detail: impl Into<String>) -> Self {
        Self {
            available: false,
            detail: detail.into(),
            shared_session: VmSharedSessionLeaseSummary::default(),
        }
    }
}

/// Adapter boundary implemented by the single managed `windows-vm`
/// appliance. Raw canonical grants accompany the resolved launcher view so
/// the adapter need not reconstruct or invent permission semantics.
pub trait VmDelegate: Send + Sync {
    /// Read-only readiness check used by the manager.  It must not launch a
    /// desktop or implicitly select the VM route.
    fn availability(&self) -> VmAvailability {
        VmAvailability::unavailable(
            "The privileged authenticated Windows guest adapter is not installed in this session.",
        )
    }

    /// Enumerate only the persisted authenticated guest catalogue. The
    /// default is deliberately empty: an adapter cannot make an arbitrary
    /// Windows application appear merely because it claims the VM is ready.
    fn inventory(&self) -> VmInventory {
        VmInventory {
            availability: self.availability(),
            applications: Vec::new(),
        }
    }

    /// Starts a VM installer from the exact descriptor selected through the
    /// Tilo portal. The implementation owns the bridge operation handle and
    /// returns only bounded, client-safe status.
    fn install_from_portal(
        &self,
        _workspace_id: &str,
        _input: VmInstallerInput,
        _source: File,
    ) -> Result<VmInstallation, windows_vm::LaunchError> {
        Err(windows_vm::AgentError::InstallerUnsupported.into())
    }

    /// Passive status query for the one managed installer operation. It must
    /// not accept a client operation ID or wake the appliance.
    fn installation_status(&self, _workspace_id: &str) -> Option<VmInstallation> {
        None
    }

    /// Explicitly refreshes the active installation. It accepts no operation
    /// handle from the client, so it cannot become a guest-process oracle.
    fn refresh_installation_status(
        &self,
        workspace_id: &str,
    ) -> Result<Option<VmInstallation>, windows_vm::LaunchError> {
        Ok(self.installation_status(workspace_id))
    }

    /// Publishes a candidate selected by its server-owned entry ID. The
    /// delegate rechecks that the candidate belongs to the active operation.
    fn publish_install_candidate(
        &self,
        _workspace_id: &str,
        _entry_id: &str,
    ) -> Result<VmCandidatePublication, windows_vm::LaunchError> {
        Err(windows_vm::AgentError::InstallerUnsupported.into())
    }

    /// Passive status query for an active guest uninstaller. It must not
    /// accept a guest operation ID or wake the appliance.
    fn uninstallation_status(&self, _workspace_id: &str) -> Option<VmUninstallation> {
        None
    }

    /// Explicitly refreshes the active guest uninstaller without allowing a
    /// client to probe arbitrary guest operation IDs.
    fn refresh_uninstallation_status(
        &self,
        workspace_id: &str,
    ) -> Result<Option<VmUninstallation>, windows_vm::LaunchError> {
        Ok(self.uninstallation_status(workspace_id))
    }

    /// Removes only the guest publication, preserving the Windows install.
    fn remove_published_application(&self, _entry_id: &str) -> Result<(), windows_vm::LaunchError> {
        Err(windows_vm::AgentError::InstallerUnsupported.into())
    }

    /// Runs the guest-authorized persistent uninstaller for one published
    /// application. The client never supplies a path or uninstall command.
    fn uninstall_published_application(
        &self,
        _workspace_id: &str,
        _entry_id: &str,
    ) -> Result<VmUninstallation, windows_vm::LaunchError> {
        Err(windows_vm::AgentError::InstallerUnsupported.into())
    }

    fn launch(
        &self,
        request: VmRouteRequest<'_>,
    ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError>;

    fn launch_with_portal_file(
        &self,
        request: VmRouteRequest<'_>,
        _source: File,
        _access: windows_vm::PortalDocumentAccess,
    ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError> {
        let _ = request;
        Err(windows_vm::LaunchError::CapabilityBroker(
            "portal file handoff is unavailable".to_string(),
        ))
    }
}

/// Smart pointers to a delegate are delegates, forwarding every method so
/// wrappers never fall back to a default implementation.
macro_rules! forward_vm_delegate {
    ($($pointer:ident),*) => {$(
        impl<T: VmDelegate + ?Sized> VmDelegate for $pointer<T> {
        fn availability(&self) -> VmAvailability {
            (**self).availability()
        }

        fn inventory(&self) -> VmInventory {
            (**self).inventory()
        }

        fn install_from_portal(
            &self,
            workspace_id: &str,
            input: VmInstallerInput,
            source: File,
        ) -> Result<VmInstallation, windows_vm::LaunchError> {
            (**self).install_from_portal(workspace_id, input, source)
        }

        fn installation_status(&self, workspace_id: &str) -> Option<VmInstallation> {
            (**self).installation_status(workspace_id)
        }

        fn refresh_installation_status(
            &self,
            workspace_id: &str,
        ) -> Result<Option<VmInstallation>, windows_vm::LaunchError> {
            (**self).refresh_installation_status(workspace_id)
        }

        fn publish_install_candidate(
            &self,
            workspace_id: &str,
            entry_id: &str,
        ) -> Result<VmCandidatePublication, windows_vm::LaunchError> {
            (**self).publish_install_candidate(workspace_id, entry_id)
        }

        fn uninstallation_status(&self, workspace_id: &str) -> Option<VmUninstallation> {
            (**self).uninstallation_status(workspace_id)
        }

        fn refresh_uninstallation_status(
            &self,
            workspace_id: &str,
        ) -> Result<Option<VmUninstallation>, windows_vm::LaunchError> {
            (**self).refresh_uninstallation_status(workspace_id)
        }

        fn remove_published_application(&self, entry_id: &str) -> Result<(), windows_vm::LaunchError> {
            (**self).remove_published_application(entry_id)
        }

        fn uninstall_published_application(
            &self,
            workspace_id: &str,
            entry_id: &str,
        ) -> Result<VmUninstallation, windows_vm::LaunchError> {
            (**self).uninstall_published_application(workspace_id, entry_id)
        }

        fn launch(
            &self,
            request: VmRouteRequest<'_>,
        ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError> {
            (**self).launch(request)
        }

        fn launch_with_portal_file(
            &self,
            request: VmRouteRequest<'_>,
            source: File,
            access: windows_vm::PortalDocumentAccess,
        ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError> {
            (**self).launch_with_portal_file(request, source, access)
        }
    }
    )*};
}

forward_vm_delegate!(Box, Arc);

/// Fail-closed adapter used until the privileged authenticated guest bridge is
/// available in the compositor session. The protocol exposes this state as a
/// disabled route instead of pretending the VM can launch.
#[derive(Clone, Copy, Debug, Default)]
pub struct UnavailableVmDelegate;

impl VmDelegate for UnavailableVmDelegate {
    fn availability(&self) -> VmAvailability {
        VmAvailability::unavailable(
            "The privileged authenticated Windows guest adapter is not installed in this session.",
        )
    }

    fn launch(
        &self,
        _request: VmRouteRequest<'_>,
    ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError> {
        Err(windows_vm::AgentError::Unavailable.into())
    }
}

pub struct CompatibilityService<R, B, E, V> {
    catalogue: Catalogue,
    store: CapsuleStore,
    runtimes: R,
    resources: B,
    executor: E,
    vm: V,
    session: HostSession,
    /// Prefix setup and recipe checks are serialized by the singleton service.
    capsule_gate: Mutex<()>,
    /// Setup/removal is a mutation transaction. It does not block ordinary
    /// application launches, but two managers cannot concurrently replace or
    /// retire the same private prefix.
    installation_gate: Mutex<()>,
}

impl<R, B, E, V> CompatibilityService<R, B, E, V>
where
    R: RuntimeRegistry,
    B: LaunchResourceBroker,
    E: SandboxExecutor,
    V: VmDelegate,
{
    pub fn new(
        catalogue: Catalogue,
        store: CapsuleStore,
        runtimes: R,
        resources: B,
        executor: E,
        vm: V,
        session: HostSession,
    ) -> Result<Self, ServiceError> {
        catalogue.validate()?;
        session.validate()?;
        Ok(Self {
            catalogue,
            store,
            runtimes,
            resources,
            executor,
            vm,
            session,
            capsule_gate: Mutex::new(()),
            installation_gate: Mutex::new(()),
        })
    }

    /// Resolve only. It performs no prefix, broker, VM, or process mutation.
    pub fn explain(
        &self,
        query: &ApplicationQuery,
        vm_fallback: VmFallback,
        app_id: &str,
        capabilities: &[CapOverride],
        now_unix: Option<u64>,
    ) -> Result<ExplainStatus, ServiceError> {
        validate_launch_id("app id", app_id)?;
        let route = self.resolve_route(query, vm_fallback, app_id)?;
        let resolved = CapabilityPlan::for_app(app_id, capabilities, now_unix);
        Ok(explanation(&route, vm_fallback, &resolved, false))
    }

    /// Read-only VM readiness for the manager.  This asks the installed
    /// delegate only for its state; it never starts a guest or relaxes the
    /// explicit-fallback requirement used by [`Self::launch`].
    #[must_use]
    pub fn vm_availability(&self) -> VmAvailability {
        self.vm.availability()
    }

    /// Cached, authenticated inventory for the shared Windows appliance. It
    /// is safe to use for a list refresh because the delegate must not start
    /// the appliance here.
    #[must_use]
    pub fn vm_inventory(&self) -> VmInventory {
        self.vm.inventory()
    }

    #[must_use]
    pub fn vm_installation_status(&self, workspace_id: &str) -> Option<VmInstallation> {
        self.vm.installation_status(workspace_id)
    }

    pub fn refresh_vm_installation_status(
        &self,
        workspace_id: &str,
    ) -> Result<Option<VmInstallation>, ServiceError> {
        self.vm
            .refresh_installation_status(workspace_id)
            .map_err(Into::into)
    }

    #[must_use]
    pub fn vm_uninstallation_status(&self, workspace_id: &str) -> Option<VmUninstallation> {
        self.vm.uninstallation_status(workspace_id)
    }

    pub fn refresh_vm_uninstallation_status(
        &self,
        workspace_id: &str,
    ) -> Result<Option<VmUninstallation>, ServiceError> {
        self.vm
            .refresh_uninstallation_status(workspace_id)
            .map_err(Into::into)
    }

    pub fn register_program(
        &self,
        capsule_id: &str,
        program: CapsuleProgram,
    ) -> Result<(), ServiceError> {
        let _guard = self
            .capsule_gate
            .lock()
            .map_err(|_| ServiceError::StateLock)?;
        self.store.register_program(capsule_id, program)?;
        Ok(())
    }

    pub fn launch(&self, input: &LaunchInput) -> Result<LaunchReceipt, ServiceError> {
        validate_launch_id("app id", &input.app_id)?;
        validate_launch_id("workspace id", &input.workspace_id)?;
        validate_launch_id("program id", &input.program_id)?;
        let route = self.resolve_route(&input.query, input.vm_fallback, &input.app_id)?;
        let capabilities =
            CapabilityPlan::for_app(&input.app_id, &input.capabilities, input.now_unix);
        if route.runtime.layer == Layer::Vm {
            if input.vm_fallback != VmFallback::Explicit {
                return Err(ServiceError::VmWasNotExplicit);
            }
            let running = self.vm.launch(VmRouteRequest {
                route: RouteKind::Vm,
                app_id: &input.app_id,
                workspace_id: &input.workspace_id,
                published_application_id: &input.program_id,
                capabilities: &input.capabilities,
                resolved_capabilities: &capabilities,
            })?;
            return Ok(LaunchReceipt {
                explanation: explanation(&route, input.vm_fallback, &capabilities, false),
                success: running.process.exit_code == Some(0),
                exit_code: running.process.exit_code,
            });
        }

        // Serialize only state/prefix maintenance. Do not hold this guard
        // while a Windows application is alive: unrelated launches continue.
        let (capsule, program) = {
            let _guard = self
                .capsule_gate
                .lock()
                .map_err(|_| ServiceError::StateLock)?;
            let capsule = self.store.ensure_for_plan(&route)?;
            let program = capsule
                .program(&input.program_id)
                .cloned()
                .ok_or_else(|| ServiceError::ProgramNotRegistered(input.program_id.clone()))?;
            (capsule, program)
        };
        let lease = self.resources.materialize(ResourceRequest {
            app_id: &input.app_id,
            workspace_id: &input.workspace_id,
            capabilities: &capabilities,
        })?;
        let result: Result<(CapsuleCommandPlan, ExecutionOutcome), ServiceError> = (|| {
            let planner = SandboxPlanner::new(&self.store, &self.runtimes, &self.session)?;
            let plan = planner.plan(
                &route,
                &capsule,
                &program,
                &capabilities,
                &input.workspace_id,
                &lease,
            )?;
            let outcome = self.executor.execute(&plan)?;
            Ok((plan, outcome))
        })();
        self.resources.revoke(&lease.id);
        let (command, outcome): (CapsuleCommandPlan, ExecutionOutcome) = result?;
        Ok(LaunchReceipt {
            explanation: explanation(
                &route,
                input.vm_fallback,
                &capabilities,
                command.audio_output,
            ),
            success: outcome.success,
            exit_code: outcome.exit_code,
        })
    }

    /// Launch a row from the authenticated guest publication. This bypasses
    /// compatibility recipe resolution because a published VM app is not a
    /// Wine/Proton capsule and has no host-controlled executable query.
    pub fn launch_published_vm(
        &self,
        application: &VmPublishedApplication,
        workspace_id: &str,
        capabilities: &[CapOverride],
        now_unix: Option<u64>,
    ) -> Result<LaunchReceipt, ServiceError> {
        validate_launch_id("app id", &application.app_id)?;
        validate_launch_id("workspace id", workspace_id)?;
        validate_launch_id(
            "published application id",
            &application.published_application_id,
        )?;
        let resolved = CapabilityPlan::for_app(&application.app_id, capabilities, now_unix);
        let running = self.vm.launch(VmRouteRequest {
            route: RouteKind::Vm,
            app_id: &application.app_id,
            workspace_id,
            published_application_id: &application.published_application_id,
            capabilities,
            resolved_capabilities: &resolved,
        })?;
        Ok(LaunchReceipt {
            explanation: published_vm_explanation(application.presentation.label()),
            success: running.process.exit_code == Some(0),
            exit_code: running.process.exit_code,
        })
    }

    pub fn launch_published_vm_with_portal_file(
        &self,
        application: &VmPublishedApplication,
        workspace_id: &str,
        capabilities: &[CapOverride],
        now_unix: Option<u64>,
        source: File,
        access: windows_vm::PortalDocumentAccess,
    ) -> Result<LaunchReceipt, ServiceError> {
        validate_launch_id("app id", &application.app_id)?;
        validate_launch_id("workspace id", workspace_id)?;
        validate_launch_id(
            "published application id",
            &application.published_application_id,
        )?;
        let resolved = CapabilityPlan::for_app(&application.app_id, capabilities, now_unix);
        let running = self.vm.launch_with_portal_file(
            VmRouteRequest {
                route: RouteKind::Vm,
                app_id: &application.app_id,
                workspace_id,
                published_application_id: &application.published_application_id,
                capabilities,
                resolved_capabilities: &resolved,
            },
            source,
            access,
        )?;
        Ok(LaunchReceipt {
            explanation: published_vm_explanation(application.presentation.label()),
            success: running.process.exit_code == Some(0),
            exit_code: running.process.exit_code,
        })
    }

    /// Import and run one portal-selected installer. The caller transfers an
    /// already-open descriptor; no host path or runner choice crosses this
    /// boundary. The inspected digest is resolved through the catalogue and
    /// binds the managed copy that setup later executes.
    pub fn install(
        &self,
        source: &mut File,
        source_name: &str,
        workspace_id: &str,
        _capabilities: &[CapOverride],
        _now_unix: Option<u64>,
    ) -> Result<InstallOutcome, ServiceError> {
        validate_launch_id("workspace id", workspace_id)?;
        let inspection = inspect_installer(source, source_name)?;
        let app_id = imported_app_id(&inspection.sha256);
        let route =
            self.resolve_route(&installer_query(&inspection), VmFallback::Explicit, &app_id)?;
        let input = vm_installer_input(&inspection);
        let recommended_route = RouteKind::from(route.runtime.layer);
        let detail = if recommended_route == RouteKind::Vm {
            "Tilo recommends the managed Windows appliance for this installer. Confirm the recommended choice or choose a private Wine/Proton capsule."
        } else {
            "Tilo recommends a private compatibility capsule. You can instead explicitly use the managed Windows appliance."
        };
        Ok(InstallOutcome::Confirmation {
            confirmation: VmInstallConfirmation {
                display_name: input.display_name.clone(),
                detail: detail.to_string(),
                recommended_route,
            },
            input,
        })
    }

    /// Starts the ordinary Wine/Proton transaction after a second local
    /// confirmation. The portal descriptor is re-inspected against the
    /// session-owned pending digest before anything reaches a capsule.
    pub fn install_in_capsule(
        &self,
        mut source: File,
        source_name: &str,
        workspace_id: &str,
        capabilities: &[CapOverride],
        now_unix: Option<u64>,
        expected: &VmInstallerInput,
    ) -> Result<InstallReceipt, ServiceError> {
        validate_launch_id("workspace id", workspace_id)?;
        let inspection = inspect_installer(&mut source, source_name)?;
        if &vm_installer_input(&inspection) != expected {
            return Err(ServiceError::VmInstallConfirmationMismatch);
        }
        let _mutation = self
            .installation_gate
            .lock()
            .map_err(|_| ServiceError::StateLock)?;
        self.install_inspected(
            &mut source,
            &inspection,
            workspace_id,
            capabilities,
            now_unix,
        )
    }

    /// Starts a catalogued Windows-VM installation after the manager's second
    /// explicit confirmation. The same already-open portal descriptor is
    /// re-inspected and rewound before it crosses the authenticated bridge.
    pub fn install_in_vm(
        &self,
        mut source: File,
        source_name: &str,
        workspace_id: &str,
        expected: &VmInstallerInput,
    ) -> Result<VmInstallation, ServiceError> {
        validate_launch_id("workspace id", workspace_id)?;
        let inspection = inspect_installer(&mut source, source_name)?;
        let input = vm_installer_input(&inspection);
        if &input != expected {
            return Err(ServiceError::VmInstallConfirmationMismatch);
        }
        source
            .seek(SeekFrom::Start(0))
            .map_err(InstallerError::from)?;
        self.vm
            .install_from_portal(workspace_id, input, source)
            .map_err(Into::into)
    }

    pub fn publish_vm_install_candidate(
        &self,
        workspace_id: &str,
        entry_id: &str,
    ) -> Result<VmCandidatePublication, ServiceError> {
        validate_launch_id("workspace id", workspace_id)?;
        validate_launch_id("VM installer candidate", entry_id)?;
        self.vm
            .publish_install_candidate(workspace_id, entry_id)
            .map_err(Into::into)
    }

    pub fn remove_vm_published_application(&self, entry_id: &str) -> Result<(), ServiceError> {
        validate_launch_id("Windows application", entry_id)?;
        self.vm
            .remove_published_application(entry_id)
            .map_err(Into::into)
    }

    pub fn uninstall_vm_published_application(
        &self,
        workspace_id: &str,
        entry_id: &str,
    ) -> Result<VmUninstallation, ServiceError> {
        validate_launch_id("workspace id", workspace_id)?;
        validate_launch_id("Windows application", entry_id)?;
        self.vm
            .uninstall_published_application(workspace_id, entry_id)
            .map_err(Into::into)
    }

    /// Retry the exact digest-pinned installer retained in a capsule. The
    /// management client cannot substitute bytes, executable arguments, or a
    /// route during repair.
    pub fn retry_install(
        &self,
        capsule_id: &str,
        workspace_id: &str,
        capabilities: &[CapOverride],
        now_unix: Option<u64>,
    ) -> Result<InstallReceipt, ServiceError> {
        validate_launch_id("capsule id", capsule_id)?;
        validate_launch_id("workspace id", workspace_id)?;
        let _mutation = self
            .installation_gate
            .lock()
            .map_err(|_| ServiceError::StateLock)?;
        let state = self
            .store
            .load(capsule_id)?
            .ok_or_else(|| CapsuleError::NotFound(capsule_id.to_string()))?;
        let installation = state
            .installation
            .clone()
            .ok_or(ServiceError::NotManagedInstallation)?;
        let inspection = InstallerInspection {
            source_name: installation.source_name.clone(),
            display_name: installation.display_name.clone(),
            sha256: installation.sha256.clone(),
            size: 0,
            kind: installation.kind,
            machine: None,
        };
        let query = installer_query(&inspection);
        let route = launch_plan_from_state(&state, &query);
        self.run_setup(
            state,
            &route,
            inspection,
            workspace_id,
            capabilities,
            now_unix,
        )
    }

    /// Move a failed or unsatisfactory Wine/Proton setup into the singleton
    /// Windows appliance without asking the user to find the installer again.
    /// The retained file is reopened through the capsule's no-follow directory
    /// chain and fully re-inspected before the authenticated guest upload.
    pub fn escalate_install_to_vm(
        &self,
        capsule_id: &str,
        workspace_id: &str,
    ) -> Result<VmInstallation, ServiceError> {
        validate_launch_id("capsule id", capsule_id)?;
        validate_launch_id("workspace id", workspace_id)?;
        let _mutation = self
            .installation_gate
            .lock()
            .map_err(|_| ServiceError::StateLock)?;
        let state = self
            .store
            .load(capsule_id)?
            .ok_or_else(|| CapsuleError::NotFound(capsule_id.to_string()))?;
        let installation = state
            .installation
            .as_ref()
            .ok_or(ServiceError::NotManagedInstallation)?;
        if matches!(
            installation.status,
            InstallationStatus::Installing
                | InstallationStatus::Uninstalling
                | InstallationStatus::Uninstalled
        ) {
            return Err(ServiceError::VmEscalationUnavailable);
        }
        let mut source = self
            .store
            .open_retained_installer(capsule_id, installation)?;
        let inspection = inspect_installer(&mut source, &installation.source_name)?;
        if inspection.sha256 != installation.sha256
            || inspection.kind != installation.kind
            || inspection.display_name != installation.display_name
        {
            return Err(ServiceError::CapsuleInstallerMismatch);
        }
        let input = vm_installer_input(&inspection);
        source
            .seek(SeekFrom::Start(0))
            .map_err(InstallerError::from)?;
        self.vm
            .install_from_portal(workspace_id, input, source)
            .map_err(Into::into)
    }

    /// Remove an installed application by retiring its entire isolated
    /// capsule. Running a vendor uninstaller would add code execution without
    /// improving host cleanup: all private registry/files already live below
    /// this one capsule and disappear atomically with it.
    pub fn delete_capsule(&self, capsule_id: &str) -> Result<(), ServiceError> {
        validate_launch_id("capsule id", capsule_id)?;
        let _mutation = self
            .installation_gate
            .lock()
            .map_err(|_| ServiceError::StateLock)?;
        let state = self
            .store
            .load(capsule_id)?
            .ok_or_else(|| CapsuleError::NotFound(capsule_id.to_string()))?;
        if state.installation.is_none() {
            return Err(ServiceError::NotManagedInstallation);
        }
        self.store.retire_capsule(capsule_id)?;
        Ok(())
    }

    /// Remove one Tilo launcher entry while retaining the Windows installation
    /// and every byte in its capsule. The server-owned entry ID is resolved by
    /// the session layer before this method is called.
    pub fn remove_launcher(&self, capsule_id: &str, program_id: &str) -> Result<(), ServiceError> {
        validate_launch_id("capsule id", capsule_id)?;
        validate_launch_id("program id", program_id)?;
        let _mutation = self
            .installation_gate
            .lock()
            .map_err(|_| ServiceError::StateLock)?;
        let mut state = self
            .store
            .load(capsule_id)?
            .ok_or_else(|| CapsuleError::NotFound(capsule_id.to_string()))?;
        let removed = state
            .programs
            .get(program_id)
            .is_some_and(|program| program.kind == ProgramKind::Application)
            .then(|| state.programs.remove(program_id))
            .flatten();
        if removed.is_none() {
            return Err(ServiceError::ProgramNotRegistered(program_id.to_string()));
        }
        self.store.save(&state)?;
        Ok(())
    }

    /// Run the installer-recorded vendor uninstaller inside the same sandbox.
    /// Success removes Tilo launchers but deliberately retains the prefix so
    /// users can inspect or later delete leftover data as a separate action.
    pub fn vendor_uninstall(
        &self,
        capsule_id: &str,
        workspace_id: &str,
        capabilities: &[CapOverride],
        now_unix: Option<u64>,
    ) -> Result<InstallReceipt, ServiceError> {
        validate_launch_id("capsule id", capsule_id)?;
        validate_launch_id("workspace id", workspace_id)?;
        let _mutation = self
            .installation_gate
            .lock()
            .map_err(|_| ServiceError::StateLock)?;
        let mut state = self
            .store
            .load(capsule_id)?
            .ok_or_else(|| CapsuleError::NotFound(capsule_id.to_string()))?;
        let installation = state
            .installation
            .clone()
            .ok_or(ServiceError::NotManagedInstallation)?;
        let uninstaller = installation
            .uninstaller_relative_path
            .clone()
            .ok_or(ServiceError::VendorUninstallerUnavailable)?;
        let query = ApplicationQuery {
            executable: uninstaller
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::to_string),
            installer_sha256: Some(installation.sha256.clone()),
            product: Some(installation.display_name.clone()),
            ..ApplicationQuery::default()
        };
        let route = launch_plan_from_state(&state, &query);
        let app_id = imported_app_id(&installation.sha256);
        let resolved = CapabilityPlan::for_app(&app_id, capabilities, now_unix);
        if let Some(journal) = state.installation.as_mut() {
            journal.status = InstallationStatus::Uninstalling;
            journal.last_error = None;
        }
        self.store.save(&state)?;
        let program = CapsuleProgram {
            id: "vendor-uninstaller".to_string(),
            display_name: format!("Uninstall {}", installation.display_name),
            prefix_relative_executable: uninstaller,
            query,
            kind: ProgramKind::InstallerControl,
            arguments: Vec::new(),
        };
        let lease = self.resources.materialize(ResourceRequest {
            app_id: &app_id,
            workspace_id,
            capabilities: &resolved,
        })?;
        let execution = (|| {
            let planner = SandboxPlanner::new(&self.store, &self.runtimes, &self.session)?;
            let command =
                planner.plan(&route, &state, &program, &resolved, workspace_id, &lease)?;
            self.executor.execute(&command).map_err(ServiceError::from)
        })();
        self.resources.revoke(&lease.id);
        let outcome = execution?;
        let status = if outcome.success {
            state
                .programs
                .retain(|_, program| program.kind == ProgramKind::InstallerControl);
            InstallationStatus::Uninstalled
        } else {
            InstallationStatus::NeedsAttention
        };
        if let Some(journal) = state.installation.as_mut() {
            journal.status = status;
            journal.last_exit_code = outcome.exit_code;
            journal.last_error = (!outcome.success)
                .then(|| "The vendor uninstaller did not complete successfully".to_string());
        }
        self.store.save(&state)?;
        Ok(InstallReceipt {
            capsule_id: state.id,
            display_name: installation.display_name,
            route: RouteKind::from(route.runtime.layer),
            status,
            discovered_programs: state
                .programs
                .values()
                .filter(|program| program.kind == ProgramKind::Application)
                .count(),
            exit_code: outcome.exit_code,
            detail: if outcome.success {
                "Vendor uninstall completed. Private leftover data is retained until you choose Delete data."
                    .to_string()
            } else {
                "Vendor uninstall needs attention; the private capsule was retained.".to_string()
            },
        })
    }

    fn install_inspected(
        &self,
        source: &mut File,
        inspection: &InstallerInspection,
        workspace_id: &str,
        capabilities: &[CapOverride],
        now_unix: Option<u64>,
    ) -> Result<InstallReceipt, ServiceError> {
        let query = installer_query(inspection);
        let app_id = imported_app_id(&inspection.sha256);
        let route = self.resolve_route(&query, VmFallback::Disabled, &app_id)?;
        if route.runtime.layer == Layer::Vm {
            return Err(ServiceError::VmInstallNeedsExplicitFallback);
        }
        let mut state = self.store.ensure_for_plan(&route)?;
        if let Some(existing) = &state.installation
            && existing.sha256 != inspection.sha256
        {
            return Err(ServiceError::CapsuleInstallerMismatch);
        }
        let relative = self.store.copy_installer(
            &state.id,
            source,
            &inspection.sha256,
            inspection.kind,
            MAX_INSTALLER_BYTES,
        )?;
        state.installation = Some(InstallationRecord {
            source_name: inspection.source_name.clone(),
            display_name: inspection.display_name.clone(),
            sha256: inspection.sha256.clone(),
            kind: inspection.kind,
            installer_relative_path: relative,
            status: InstallationStatus::Installing,
            last_exit_code: None,
            last_error: None,
            uninstaller_relative_path: None,
        });
        let control = installer_control(&state, inspection)?;
        state.programs.insert(control.id.clone(), control);
        self.store.save(&state)?;
        self.run_setup(
            state,
            &route,
            inspection.clone(),
            workspace_id,
            capabilities,
            now_unix,
        )
    }

    fn run_setup(
        &self,
        mut state: CapsuleState,
        route: &LaunchPlan,
        inspection: InstallerInspection,
        workspace_id: &str,
        capability_records: &[CapOverride],
        now_unix: Option<u64>,
    ) -> Result<InstallReceipt, ServiceError> {
        let app_id = imported_app_id(&inspection.sha256);
        let capabilities = CapabilityPlan::for_app(&app_id, capability_records, now_unix);
        let control = installer_control(&state, &inspection)?;
        if let Some(installation) = state.installation.as_mut() {
            installation.status = InstallationStatus::Installing;
            installation.last_error = None;
        }
        self.store.save(&state)?;
        let lease = self.resources.materialize(ResourceRequest {
            app_id: &app_id,
            workspace_id,
            capabilities: &capabilities,
        })?;
        let execution = (|| {
            let planner = SandboxPlanner::new(&self.store, &self.runtimes, &self.session)?;
            let plan =
                planner.plan(route, &state, &control, &capabilities, workspace_id, &lease)?;
            self.executor.execute(&plan).map_err(ServiceError::from)
        })();
        self.resources.revoke(&lease.id);
        let outcome = match execution {
            Ok(outcome) => outcome,
            Err(error) => {
                if let Some(installation) = state.installation.as_mut() {
                    installation.status = InstallationStatus::Failed;
                    installation.last_error = Some(bounded_error(&error.to_string()));
                }
                self.store.save(&state)?;
                return Err(error);
            }
        };

        let (mut programs, uninstaller) = self
            .store
            .discover_programs(&state.id, &installer_query(&inspection))?;
        if programs.is_empty()
            && outcome.success
            && inspection.kind == InstallerKind::Exe
            && !looks_like_setup(&inspection.source_name)
        {
            let installation = state
                .installation
                .as_ref()
                .ok_or(ServiceError::NotManagedInstallation)?;
            programs.push(CapsuleProgram {
                id: "portable-application".to_string(),
                display_name: inspection.display_name.clone(),
                prefix_relative_executable: installation.installer_relative_path.clone(),
                query: installer_query(&inspection),
                kind: ProgramKind::Application,
                arguments: Vec::new(),
            });
        }
        state
            .programs
            .retain(|_, program| program.kind == ProgramKind::InstallerControl);
        for program in programs.iter().cloned() {
            state.programs.insert(program.id.clone(), program);
        }
        let status = if outcome.success && !programs.is_empty() {
            InstallationStatus::Installed
        } else if outcome.success {
            InstallationStatus::NeedsAttention
        } else {
            InstallationStatus::Failed
        };
        if let Some(installation) = state.installation.as_mut() {
            installation.status = status;
            installation.last_exit_code = outcome.exit_code;
            installation.last_error = (!outcome.success).then(|| {
                format!(
                    "Installer exited with status {}",
                    outcome
                        .exit_code
                        .map_or_else(|| "unknown".to_string(), |code| code.to_string())
                )
            });
            installation.uninstaller_relative_path = uninstaller;
        }
        self.store.save(&state)?;
        Ok(InstallReceipt {
            capsule_id: state.id,
            display_name: inspection.display_name,
            route: RouteKind::from(route.runtime.layer),
            status,
            discovered_programs: programs.len(),
            exit_code: outcome.exit_code,
            detail: install_detail(status, programs.len()),
        })
    }

    fn resolve_route(
        &self,
        query: &ApplicationQuery,
        vm_fallback: VmFallback,
        app_id: &str,
    ) -> Result<LaunchPlan, ServiceError> {
        let mut route = self.catalogue.resolver()?.resolve(query, vm_fallback);
        if route.record_id.is_none() && route.runtime.layer == Layer::Wine {
            let digest = Sha256::digest(app_id.as_bytes());
            // The first 16 digest bytes, i.e. 32 hex digits.
            let mut suffix = hex::encode(digest);
            suffix.truncate(32);
            route.runtime.id = format!("wine-{suffix}");
        }
        Ok(route)
    }
}

fn installer_query(inspection: &InstallerInspection) -> ApplicationQuery {
    ApplicationQuery {
        executable: Some(inspection.source_name.clone()),
        installer_sha256: Some(inspection.sha256.clone()),
        product: Some(inspection.display_name.clone()),
        ..ApplicationQuery::default()
    }
}

fn vm_installer_input(inspection: &InstallerInspection) -> VmInstallerInput {
    VmInstallerInput {
        source_name: inspection.source_name.clone(),
        display_name: inspection.display_name.clone(),
        sha256: inspection.sha256.clone(),
        byte_len: inspection.size,
        kind: match inspection.kind {
            InstallerKind::Exe => VmInstallerKind::Exe,
            InstallerKind::Msi => VmInstallerKind::Msi,
        },
    }
}

fn imported_app_id(sha256: &str) -> String {
    format!(
        "so.tilo.windows.import.{}",
        sha256.get(..24).unwrap_or(sha256)
    )
}

fn launch_plan_from_state(state: &CapsuleState, query: &ApplicationQuery) -> LaunchPlan {
    LaunchPlan {
        record_id: None,
        match_kind: if query.installer_sha256.is_some() {
            MatchKind::ExecutableDigest
        } else {
            MatchKind::Executable
        },
        runtime: Runtime {
            id: state.id.clone(),
            layer: state.layer,
        },
        runner: state.runner.clone(),
        settings: state.settings.clone(),
        security_profile: state.security_profile.clone(),
        proton_recipe: None,
        compatibility_grade: None,
    }
}

fn installer_control(
    state: &CapsuleState,
    inspection: &InstallerInspection,
) -> Result<CapsuleProgram, ServiceError> {
    let installation = state
        .installation
        .as_ref()
        .ok_or(ServiceError::NotManagedInstallation)?;
    if installation.sha256 != inspection.sha256 || installation.kind != inspection.kind {
        return Err(ServiceError::CapsuleInstallerMismatch);
    }
    let (executable, arguments) = match inspection.kind {
        InstallerKind::Exe => (installation.installer_relative_path.clone(), Vec::new()),
        InstallerKind::Msi => {
            let file_name = installation
                .installer_relative_path
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or(ServiceError::CapsuleInstallerMismatch)?;
            (
                PathBuf::from("drive_c/windows/system32/msiexec.exe"),
                vec!["/i".to_string(), format!(r"C:\tilo-installers\{file_name}")],
            )
        }
    };
    Ok(CapsuleProgram {
        id: "installer-control".to_string(),
        display_name: format!("Install {}", inspection.display_name),
        prefix_relative_executable: executable,
        query: installer_query(inspection),
        kind: ProgramKind::InstallerControl,
        arguments,
    })
}

fn looks_like_setup(source_name: &str) -> bool {
    let stem = Path::new(source_name)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    ["setup", "install", "installer", "bootstrap", "update"]
        .iter()
        .any(|word| stem.contains(word))
}

fn bounded_error(error: &str) -> String {
    error.chars().take(2_048).collect()
}

fn install_detail(status: InstallationStatus, programs: usize) -> String {
    match status {
        InstallationStatus::Installed => format!(
            "Installation completed and {programs} application launcher(s) were discovered."
        ),
        InstallationStatus::NeedsAttention => {
            "Setup exited normally, but no application launcher was discovered. You can retry setup or delete its private data."
                .to_string()
        }
        InstallationStatus::Failed => {
            "Setup did not complete. Its private capsule and exact installer were retained for retry."
                .to_string()
        }
        InstallationStatus::Uninstalled => {
            "Vendor uninstall completed; private leftover data is retained.".to_string()
        }
        InstallationStatus::Installing => "Installation is running.".to_string(),
        InstallationStatus::Uninstalling => "Vendor uninstall is running.".to_string(),
    }
}

fn explanation(
    route: &LaunchPlan,
    vm_fallback: VmFallback,
    capabilities: &CapabilityPlan,
    audio_output: bool,
) -> ExplainStatus {
    let network_shape = NetworkShape::from_capabilities(capabilities);
    let route_kind = RouteKind::from(route.runtime.layer);
    let runner = format!("{} {}", route.runner.family, route.runner.version);
    let access = AccessSummary {
        files_read_only: capabilities
            .ro_paths
            .iter()
            .filter(|path| !path.starts_with("/dev"))
            .count(),
        files_read_write: capabilities
            .rw_paths
            .iter()
            .filter(|path| !path.starts_with("/dev"))
            .count(),
        network: match network_shape {
            NetworkShape::Offline => "none".to_string(),
            NetworkShape::Filtered => format!("{} approved host(s)", capabilities.net_hosts.len()),
            NetworkShape::Unrestricted => "unrestricted".to_string(),
        },
        microphone: capabilities
            .devices
            .iter()
            .any(|device| device == "microphone"),
        audio_output,
    };
    ExplainStatus {
        route: route_kind,
        catalogue_record: route.record_id.clone(),
        match_reason: MatchReason::from(route.match_kind),
        capsule_id: (route.runtime.layer != Layer::Vm).then(|| route.runtime.id.clone()),
        runner: runner.clone(),
        security_profile: route.security_profile.as_str().to_string(),
        vm_fallback_was_explicit: vm_fallback == VmFallback::Explicit,
        summary: match route_kind {
            RouteKind::Wine => format!(
                "Runs in isolated capsule {} with {runner}",
                route.runtime.id
            ),
            RouteKind::Proton => {
                format!(
                    "Runs through UMU in isolated capsule {} with {runner}",
                    route.runtime.id
                )
            }
            RouteKind::Vm => {
                "Runs as one published app in the managed Windows fallback".to_string()
            }
        },
        access,
    }
}

pub(crate) fn published_vm_explanation(presentation: &str) -> ExplainStatus {
    ExplainStatus {
        route: RouteKind::Vm,
        catalogue_record: None,
        match_reason: MatchReason::AppId,
        capsule_id: None,
        runner: format!("managed Windows appliance · {presentation}"),
        security_profile: "windows-app".to_string(),
        vm_fallback_was_explicit: true,
        // The current integrated SPICE presentation is intentionally
        // playback-only and carries no generic host redirectors. Tilo's normal
        // portal broker may later project a narrower per-launch grant, but a
        // catalogue row never claims that a file, network, or microphone is
        // already visible to the shared guest.
        access: AccessSummary {
            files_read_only: 0,
            files_read_write: 0,
            network: "none".to_string(),
            microphone: false,
            audio_output: true,
        },
        summary: format!(
            "Runs as a published application in the one shared Windows appliance via {presentation}."
        ),
    }
}

fn validate_launch_id(label: &'static str, value: &str) -> Result<(), ServiceError> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(ServiceError::InvalidRequest(label));
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ServiceError {
    #[error(transparent)]
    Catalogue(#[from] CatalogueError),
    #[error(transparent)]
    Capsule(#[from] CapsuleError),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Resources(#[from] ResourceError),
    #[error(transparent)]
    Execution(#[from] ExecutionError),
    #[error(transparent)]
    Installer(#[from] InstallerError),
    #[error(transparent)]
    Vm(#[from] windows_vm::LaunchError),
    #[error("invalid launch field {0}")]
    InvalidRequest(&'static str),
    #[error("program {0:?} is not registered in the selected capsule")]
    ProgramNotRegistered(String),
    #[error("VM route was selected without explicit fallback authorization")]
    VmWasNotExplicit,
    #[error(
        "this installer is catalogued for the Windows VM and needs explicit fallback confirmation"
    )]
    VmInstallNeedsExplicitFallback,
    #[error("this installer has no approved managed Windows appliance route")]
    VmInstallNotCatalogued,
    #[error("the confirmed portal installer no longer matches the inspected bytes")]
    VmInstallConfirmationMismatch,
    #[error("capsule is not a portal-managed installation")]
    NotManagedInstallation,
    #[error("the retained installer does not match this capsule journal")]
    CapsuleInstallerMismatch,
    #[error("this installation did not publish a vendor uninstaller")]
    VendorUninstallerUnavailable,
    #[error("this installation cannot be escalated while its current lifecycle action is active")]
    VmEscalationUnavailable,
    #[error("capsule service state lock is poisoned")]
    StateLock,
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tempfile::{NamedTempFile, TempDir};

    use crate::{ExecutionOutcome, LaunchLease, ResourceError, SystemRuntimeRegistry};

    use super::*;

    struct Resources;

    impl LaunchResourceBroker for Resources {
        fn materialize(&self, _: ResourceRequest<'_>) -> Result<LaunchLease, ResourceError> {
            Ok(LaunchLease::deny_all("lease"))
        }

        fn revoke(&self, _: &str) {}
    }

    struct Executor;

    impl SandboxExecutor for Executor {
        fn execute(&self, _: &CapsuleCommandPlan) -> Result<ExecutionOutcome, ExecutionError> {
            Ok(ExecutionOutcome {
                success: true,
                exit_code: Some(0),
            })
        }
    }

    #[derive(Default)]
    struct Vm {
        calls: Arc<AtomicUsize>,
        install_calls: Arc<AtomicUsize>,
    }

    impl VmDelegate for Vm {
        fn launch(
            &self,
            _: VmRouteRequest<'_>,
        ) -> Result<windows_vm::RunningRemoteApp, windows_vm::LaunchError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(windows_vm::LaunchError::Agent(
                windows_vm::AgentError::Unavailable,
            ))
        }

        fn install_from_portal(
            &self,
            workspace_id: &str,
            input: VmInstallerInput,
            mut source: File,
        ) -> Result<VmInstallation, windows_vm::LaunchError> {
            if workspace_id != "workspace" || input.source_name != "Favorite_App.exe" {
                return Err(windows_vm::LaunchError::InvalidRequest(
                    "test retained installer identity",
                ));
            }
            let mut bytes = Vec::new();
            source.read_to_end(&mut bytes).map_err(|error| {
                windows_vm::LaunchError::CapabilityBroker(format!(
                    "could not read retained test installer: {error}"
                ))
            })?;
            if hex::encode(Sha256::digest(&bytes)) != input.sha256 {
                return Err(windows_vm::LaunchError::InvalidRequest(
                    "test retained installer digest",
                ));
            }
            self.install_calls.fetch_add(1, Ordering::SeqCst);
            Ok(VmInstallation {
                detail: "Windows setup is starting.".to_string(),
                phase: VmInstallationPhase::Starting,
                exit_code: None,
                candidates: Vec::new(),
            })
        }
    }

    fn pe_installer() -> NamedTempFile {
        let mut file = NamedTempFile::new().unwrap();
        let mut bytes = vec![0_u8; 0x200];
        bytes[..2].copy_from_slice(b"MZ");
        bytes[0x3c..0x40].copy_from_slice(&0x80_u32.to_le_bytes());
        bytes[0x80..0x84].copy_from_slice(b"PE\0\0");
        bytes[0x84..0x86].copy_from_slice(&0x8664_u16.to_le_bytes());
        file.write_all(&bytes).unwrap();
        file.flush().unwrap();
        file
    }

    fn catalogue() -> Catalogue {
        Catalogue::from_json(
            r#"{
              "schema_version": 1,
              "records": [{
                "id": "vm-only",
                "version": 1,
                "provenance": "built_in",
                "signed": true,
                "matches": [{"executable": "special.exe"}],
                "recipe": {
                  "layer": "vm",
                  "runner": {"family": "windows-vm", "version": "managed"},
                  "settings": {},
                  "security_profile": "windows-app"
                }
              }]
            }"#,
        )
        .unwrap()
    }

    fn service(
        directory: &TempDir,
    ) -> CompatibilityService<SystemRuntimeRegistry, Resources, Executor, Vm> {
        CompatibilityService::new(
            catalogue(),
            CapsuleStore::new(directory.path()).unwrap(),
            SystemRuntimeRegistry::assuming_provider(),
            Resources,
            Executor,
            Vm::default(),
            HostSession {
                uid: 1000,
                wayland_socket: PathBuf::from("/run/user/1000/tilo-ws-test"),
                render_node: PathBuf::from("/dev/dri/renderD128"),
            },
        )
        .unwrap()
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn vm_routing_is_explicit_visible_and_publication_bound() {
        // Phase `matching_vm_recipe_does_not_auto_route_without_explicit_action`.
        {
            let directory = TempDir::new().unwrap();
            let service = service(&directory);
            let query = ApplicationQuery::executable("special.exe");
            let status = service
                .explain(&query, VmFallback::Disabled, "app", &[], Some(1))
                .unwrap();
            assert_eq!(status.route, RouteKind::Wine);

            let result = service.launch(&LaunchInput {
                app_id: "app".to_string(),
                workspace_id: "workspace".to_string(),
                program_id: "special".to_string(),
                query,
                vm_fallback: VmFallback::Disabled,
                capabilities: Vec::new(),
                now_unix: Some(1),
            });
            assert!(matches!(result, Err(ServiceError::ProgramNotRegistered(_))));
            assert_eq!(service.vm.calls.load(Ordering::SeqCst), 0);
        }
        // Phase `explicit_vm_escalation_reuses_the_digest_pinned_capsule_installer`.
        {
            let directory = TempDir::new().unwrap();
            let service = service(&directory);
            let installer = pe_installer();
            let outcome = service
                .install(
                    &mut installer.reopen().unwrap(),
                    "Favorite_App.exe",
                    "workspace",
                    &[],
                    Some(1),
                )
                .unwrap();
            let InstallOutcome::Confirmation { input, .. } = outcome;
            let receipt = service
                .install_in_capsule(
                    installer.reopen().unwrap(),
                    "Favorite_App.exe",
                    "workspace",
                    &[],
                    Some(1),
                    &input,
                )
                .unwrap();

            let status = service
                .escalate_install_to_vm(&receipt.capsule_id, "workspace")
                .unwrap();

            assert_eq!(status.phase, VmInstallationPhase::Starting);
            assert_eq!(service.vm.install_calls.load(Ordering::SeqCst), 1);
        }
        // Phase `explicit_vm_action_is_visible_before_delegate_is_called`.
        {
            let directory = TempDir::new().unwrap();
            let service = service(&directory);
            let status = service
                .explain(
                    &ApplicationQuery::executable("special.exe"),
                    VmFallback::Explicit,
                    "app",
                    &[],
                    Some(1),
                )
                .unwrap();
            assert_eq!(status.route, RouteKind::Vm);
            assert!(status.vm_fallback_was_explicit);
        }
        // Phase `protocol_launch_has_no_client_supplied_capability_field`.
        {
            let request = serde_json::json!({
                "method": "launch",
                "version": 1,
                "app_id": "app",
                "workspace_id": "workspace",
                "program_id": "special",
                "query": {"executable": "special.exe"},
                "explicit_vm_fallback": false,
                "capabilities": [{"effect": "grant"}]
            });
            assert!(serde_json::from_value::<crate::ProtocolRequest>(request).is_err());
        }
        // Phase `published_vm_launch_uses_only_the_authenticated_publication_id`.
        {
            let directory = TempDir::new().unwrap();
            let calls = Arc::new(AtomicUsize::new(0));
            let service = CompatibilityService::new(
                catalogue(),
                CapsuleStore::new(directory.path()).unwrap(),
                SystemRuntimeRegistry::assuming_provider(),
                Resources,
                Executor,
                Vm {
                    calls: Arc::clone(&calls),
                    install_calls: Arc::new(AtomicUsize::new(0)),
                },
                HostSession {
                    uid: 1000,
                    wayland_socket: PathBuf::from("/run/user/1000/tilo-ws-test"),
                    render_node: PathBuf::from("/dev/dri/renderD128"),
                },
            )
            .unwrap();
            let application = VmPublishedApplication {
                entry_id: "windows-vm-opaque".to_string(),
                app_id: "so.tilo.windows.vm.opaque".to_string(),
                published_application_id: "published-word".to_string(),
                display_name: "Word".to_string(),
                presentation: VmPresentation::NativeRail,
                uninstall_available: false,
            };

            assert!(matches!(
                service.launch_published_vm(&application, "workspace", &[], Some(1)),
                Err(ServiceError::Vm(windows_vm::LaunchError::Agent(
                    windows_vm::AgentError::Unavailable
                )))
            ));
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }
}
