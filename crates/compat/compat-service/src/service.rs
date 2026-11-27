use std::fs::File;
use std::io::{Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use compat_catalogue::{
    ApplicationQuery, Architecture, Catalogue, CatalogueError, LaunchPlan, Layer, MatchKind,
    Runtime, VmFallback,
};
use launcher::capabilities::CapabilityPlan;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tilo_permissions::caps::CapOverride;

use crate::{
    CapsuleCommandPlan, CapsuleError, CapsuleProgram, CapsuleState, CapsuleStore, ExecutionError,
    ExecutionOutcome, InstallationRecord, InstallationStatus, InstallerError, InstallerInspection,
    InstallerKind, LaunchDisplay, LaunchDisplays, LaunchResourceBroker, MAX_INSTALLER_BYTES,
    NetworkShape, PlanError, ProgramKind, ResourceError, ResourceRequest, RuntimeRegistry,
    SandboxExecutor, SandboxPlanner, StartedProgram, inspect_installer,
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
    pub summary: String,
}

#[derive(Clone, Debug)]
pub struct LaunchInput {
    pub app_id: String,
    pub workspace_id: String,
    pub program_id: String,
    /// The capsule an installed program was registered in; `None` for a program its own app
    /// starts, whose capsule follows from the app.
    pub capsule_id: Option<String>,
    pub query: ApplicationQuery,
    pub vm_fallback: VmFallback,
    /// Canonical Tilo capability records, not a compatibility-local policy.
    pub capabilities: Vec<CapOverride>,
    pub now_unix: Option<u64>,
}

/// A program started: how it was started. Its end is reported apart, once it came
/// ([`crate::ProgramEnd`]).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LaunchReceipt {
    pub explanation: ExplainStatus,
}

/// A program [`CompatibilityService::launch`] started, which goes on after the call returns.
pub struct Launched {
    pub receipt: LaunchReceipt,
    pub program: Box<dyn StartedProgram>,
    /// Gives back what the launch was lent (its audio graph, its network proxy) once the program
    /// ended.
    pub release: Box<dyn FnOnce() + Send>,
}

/// A setup or a vendor uninstaller a manager started, which goes on after the call returns:
/// [`CompatibilityService::finish_setup`] closes its transaction once it ended.
pub(crate) struct StartedSetup {
    /// What the manager is answered: the transaction open.
    pub receipt: InstallReceipt,
    /// The entry it is kept under through a desktop restart.
    pub entry_id: String,
    pub display_name: String,
    pub program: Box<dyn StartedProgram>,
    /// Gives back what it was lent once it ended.
    pub release: Box<dyn FnOnce() + Send>,
}

/// Who a program is started for.
struct Started<'a> {
    app_id: &'a str,
    entry_id: &'a str,
    workspace_id: &'a str,
    capabilities: &'a CapabilityPlan,
    lease: &'a crate::LaunchLease,
}

/// The program id of a capsule's setup, and of its vendor uninstaller.
const INSTALLER_CONTROL: &str = "installer-control";
const VENDOR_UNINSTALLER: &str = "vendor-uninstaller";

/// A program that had ended by the time its launch returned.
struct Ended(ExecutionOutcome);

impl StartedProgram for Ended {
    fn wait(&self) -> Result<ExecutionOutcome, ExecutionError> {
        Ok(self.0.clone())
    }

    fn stop(&self) {}
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

pub struct CompatibilityService<R, B, E, V> {
    catalogue: Catalogue,
    store: CapsuleStore,
    runtimes: R,
    resources: Arc<B>,
    executor: E,
    vm: V,
    /// Where each launch's program shows its windows, asked as it starts.
    displays: Arc<dyn LaunchDisplays>,
    capsule_lock: Mutex<()>,
    /// Setup/removal is a mutation transaction. It does not block ordinary
    /// application launches, but two managers cannot concurrently replace or
    /// retire the same private prefix.
    installation_lock: Mutex<()>,
    /// The capsules whose setup or uninstaller is open: it goes on after the request that
    /// started it, and nothing else changes the capsule until it ended.
    setups: Mutex<std::collections::BTreeSet<String>>,
    /// The setups the desktop before left open are closed: once, as this desktop starts fresh.
    lost_setups_settled: std::sync::atomic::AtomicBool,
}

impl<R, B, E, V> CompatibilityService<R, B, E, V>
where
    R: RuntimeRegistry,
    B: LaunchResourceBroker + 'static,
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
        displays: Arc<dyn LaunchDisplays>,
    ) -> Result<Self, ServiceError> {
        catalogue.validate()?;
        Ok(Self {
            catalogue,
            store,
            runtimes,
            resources: Arc::new(resources),
            executor,
            vm,
            displays,
            capsule_lock: Mutex::new(()),
            installation_lock: Mutex::new(()),
            setups: Mutex::default(),
            lost_setups_settled: std::sync::atomic::AtomicBool::new(false),
        })
    }

    /// The programs started before the desktop restarted in place, as the caller found them;
    /// each is handed out once.
    pub(crate) fn taken_back(&self) -> Vec<crate::TakenBack> {
        self.displays.taken_back()
    }

    /// What a program taken back after a restart gives back when it ends: the lease the desktop
    /// before this one lent it, which this one knows only as one whose program ended.
    pub(crate) fn release_taken_back(&self) -> Box<dyn FnOnce() + Send> {
        // Also what a desktop gives back as it starts: the leases of programs that ended while
        // no desktop was there, such as every program a logout ended.
        let resources = Arc::clone(&self.resources);
        Box::new(move || resources.release_ended())
    }

    /// The display a program launched into `workspace_id` shows its windows
    /// on, as the caller gives it now.
    fn display_for(&self, workspace_id: &str) -> Result<LaunchDisplay, PlanError> {
        self.displays
            .display_for(workspace_id)
            .map_err(PlanError::NoDisplay)
    }

    /// Resolve only. It performs no prefix, broker, VM, or process mutation.
    pub fn explain(
        &self,
        query: &ApplicationQuery,
        vm_fallback: VmFallback,
        app_id: &str,
        capsule_id: Option<&str>,
        capabilities: &[CapOverride],
        now_unix: Option<u64>,
    ) -> Result<ExplainStatus, ServiceError> {
        validate_launch_id("app id", app_id)?;
        let route = self.resolve_route(query, vm_fallback, app_id, capsule_id)?;
        let resolved = CapabilityPlan::for_app(app_id, capabilities, now_unix);
        Ok(explanation(&route, vm_fallback, &resolved, false))
    }

    /// How `program` of the capsule `capsule_id` opens as `app_id`, the way a person chose it to
    /// when they did, and the way it opens unless someone chooses otherwise.
    pub fn explain_program(
        &self,
        query: &ApplicationQuery,
        app_id: &str,
        capsule_id: &str,
        program: &CapsuleProgram,
        capabilities: &[CapOverride],
        now_unix: Option<u64>,
    ) -> Result<(ExplainStatus, RouteKind), ServiceError> {
        validate_launch_id("app id", app_id)?;
        let route = self.resolve_route(query, VmFallback::Disabled, app_id, Some(capsule_id))?;
        let recommended = RouteKind::from(route.runtime.layer);
        let route = match program.opens_with {
            Some(layer) => route.opened_with(layer)?,
            None => route,
        };
        let resolved = CapabilityPlan::for_app(app_id, capabilities, now_unix);
        Ok((
            explanation(&route, VmFallback::Disabled, &resolved, false),
            recommended,
        ))
    }

    /// What `program_id` of `capsule_id` opens with at its next start: the layer a person chose
    /// for it, or its capsule's.
    pub fn opens_with(&self, capsule_id: &str, program_id: &str) -> Result<Layer, ServiceError> {
        let state = self
            .store
            .load(capsule_id)?
            .ok_or_else(|| ServiceError::ProgramNotRegistered(program_id.to_owned()))?;
        let program = state
            .program(program_id)
            .ok_or_else(|| ServiceError::ProgramNotRegistered(program_id.to_owned()))?;
        Ok(program.opens_with.unwrap_or(state.layer))
    }

    /// Open `program_id` of `capsule_id` with `route` from now on, or as its capsule opens
    /// (`None`). The program keeps its files: Wine and Proton open the capsule's one prefix.
    pub fn set_opens_with(
        &self,
        capsule_id: &str,
        program_id: &str,
        route: Option<RouteKind>,
    ) -> Result<(), ServiceError> {
        let layer = match route {
            None => None,
            Some(RouteKind::Wine) => Some(Layer::Wine),
            Some(RouteKind::Proton) => Some(Layer::Proton),
            Some(RouteKind::Vm) => return Err(CatalogueError::InvalidOpensWith.into()),
        };
        let _guard = self
            .capsule_lock
            .lock()
            .map_err(|_| ServiceError::StateLock)?;
        let state = self
            .store
            .load(capsule_id)?
            .ok_or_else(|| ServiceError::ProgramNotRegistered(program_id.to_owned()))?;
        if layer == Some(Layer::Proton) && state.settings.architecture == Architecture::Win32 {
            return Err(ServiceError::ProtonNeeds64Bit);
        }
        self.store.set_opens_with(capsule_id, program_id, layer)?;
        Ok(())
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
            .capsule_lock
            .lock()
            .map_err(|_| ServiceError::StateLock)?;
        self.store.register_program(capsule_id, program)?;
        Ok(())
    }

    /// Start the program `input` names. The call returns once it started; the program goes on,
    /// and [`Launched::program`] says when and how it ended.
    pub fn launch(&self, input: &LaunchInput) -> Result<Launched, ServiceError> {
        validate_launch_id("app id", &input.app_id)?;
        validate_launch_id("workspace id", &input.workspace_id)?;
        validate_launch_id("program id", &input.program_id)?;
        let route = self.resolve_route(
            &input.query,
            input.vm_fallback,
            &input.app_id,
            input.capsule_id.as_deref(),
        )?;
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
            return Ok(Launched {
                receipt: LaunchReceipt {
                    explanation: explanation(&route, input.vm_fallback, &capabilities, false),
                },
                program: Box::new(Ended(ExecutionOutcome::exited(running.process.exit_code))),
                release: Box::new(|| {}),
            });
        }

        // Serialize only state/prefix maintenance. Do not hold this guard
        // while a Windows application is alive: unrelated launches continue.
        let (capsule, program) = {
            let _guard = self
                .capsule_lock
                .lock()
                .map_err(|_| ServiceError::StateLock)?;
            let capsule = self.store.ensure_for_plan(&route)?;
            let program = capsule
                .program(&input.program_id)
                .cloned()
                .ok_or_else(|| ServiceError::ProgramNotRegistered(input.program_id.clone()))?;
            (capsule, program)
        };
        let (route, capsule) = opened_as_chosen(&route, &capsule, &program)?;
        let lease = self.resources.materialize(ResourceRequest {
            app_id: &input.app_id,
            workspace_id: &input.workspace_id,
            capabilities: &capabilities,
        })?;
        let entry_id = crate::store::managed_program_id(&capsule.id, &program.id);
        let started = self.start_program(
            &route,
            &capsule,
            &program,
            &Started {
                app_id: &input.app_id,
                entry_id: &entry_id,
                workspace_id: &input.workspace_id,
                capabilities: &capabilities,
                lease: &lease,
            },
        );
        let (command, program) = match started {
            Ok(started) => started,
            Err(error) => {
                self.resources.revoke(&lease.id);
                return Err(error);
            }
        };
        let resources = Arc::clone(&self.resources);
        Ok(Launched {
            receipt: LaunchReceipt {
                explanation: explanation(
                    &route,
                    input.vm_fallback,
                    &capabilities,
                    command.audio_output,
                ),
            },
            program,
            release: Box::new(move || resources.revoke(&lease.id)),
        })
    }

    /// Start `program` of `capsule` behind a display of its own where the caller keeps programs:
    /// it stays open while the desktop restarts in place, a setup as much as an application.
    fn start_program(
        &self,
        route: &LaunchPlan,
        capsule: &CapsuleState,
        program: &CapsuleProgram,
        started: &Started<'_>,
    ) -> Result<(CapsuleCommandPlan, Box<dyn StartedProgram>), ServiceError> {
        let kept = self
            .displays
            .kept_display(&crate::KeptProgram {
                app_id: started.app_id,
                display_name: &program.display_name,
                entry_id: started.entry_id,
                workspace_id: started.workspace_id,
                x11: crate::plan::shows_through_x11(route.runtime.layer),
                setup: [INSTALLER_CONTROL, VENDOR_UNINSTALLER].contains(&program.id.as_str()),
            })
            .transpose()
            .map_err(PlanError::NoDisplay)?;
        let display = match &kept {
            Some(kept) => kept.display().clone(),
            None => self.display_for(started.workspace_id)?,
        };
        let planner = SandboxPlanner::new(&self.store, &self.runtimes, &display)?;
        // A kept program's proxy answers the session that adopts it after a restart through
        // a control socket in its keeper's directory.
        let planner = match &kept {
            Some(kept) => planner
                .with_network_control(kept.display().runtime_dir.join(launcher::netproxy::CONTROL)),
            None => planner,
        };
        let plan = planner.plan(
            route,
            capsule,
            program,
            started.capabilities,
            started.workspace_id,
            started.lease,
        )?;
        let command = self.executor.command(&plan)?;
        let program = match kept {
            Some(kept) => kept.start(command)?,
            None => command.spawn()?,
        };
        Ok((plan, program))
    }

    /// Claim `capsule_id` for a setup or an uninstaller: one at a time replaces or retires a
    /// private prefix.
    fn claim_setup(&self, capsule_id: &str) -> Result<(), ServiceError> {
        let claimed = self
            .setups
            .lock()
            .map_err(|_| ServiceError::StateLock)?
            .insert(capsule_id.to_owned());
        if claimed {
            Ok(())
        } else {
            Err(ServiceError::SetupOpen)
        }
    }

    fn unclaim_setup(&self, capsule_id: &str) {
        if let Ok(mut setups) = self.setups.lock() {
            setups.remove(capsule_id);
        }
    }

    /// Refuse a change to `capsule_id` while its setup or uninstaller is open.
    fn not_in_setup(&self, capsule_id: &str) -> Result<(), ServiceError> {
        let open = self
            .setups
            .lock()
            .map_err(|_| ServiceError::StateLock)?
            .contains(capsule_id);
        if open {
            Err(ServiceError::SetupOpen)
        } else {
            Ok(())
        }
    }

    /// The capsule whose setup or uninstaller `entry_id` names, while its transaction is open: a
    /// kept one the desktop before a restart started, which this service takes up and finishes.
    pub(crate) fn setup_entry(&self, entry_id: &str) -> Option<String> {
        let states = self.store.installations().ok()?;
        let capsule = states.into_iter().find(|state| {
            state.installation.as_ref().is_some_and(|installation| {
                matches!(
                    installation.status,
                    InstallationStatus::Installing | InstallationStatus::Uninstalling
                )
            }) && [INSTALLER_CONTROL, VENDOR_UNINSTALLER]
                .iter()
                .any(|program| crate::store::managed_program_id(&state.id, program) == entry_id)
        })?;
        self.claim_setup(&capsule.id).ok()?;
        Some(capsule.id)
    }

    /// The setup or uninstaller of `capsule_id` ended as `outcome` says: what it installed is
    /// discovered and the transaction closed, whether this service or the one before a desktop
    /// restart started it.
    pub(crate) fn finish_setup(
        &self,
        capsule_id: &str,
        outcome: Result<ExecutionOutcome, String>,
    ) -> Result<InstallationStatus, ServiceError> {
        let finished = (|| {
            let _mutation = self
                .installation_lock
                .lock()
                .map_err(|_| ServiceError::StateLock)?;
            let state = self
                .store
                .load(capsule_id)?
                .ok_or_else(|| CapsuleError::NotFound(capsule_id.to_string()))?;
            match state
                .installation
                .as_ref()
                .map(|installation| installation.status)
            {
                Some(InstallationStatus::Installing) => self.setup_ended(state, outcome),
                Some(InstallationStatus::Uninstalling) => self.uninstall_ended(state, &outcome),
                _ => Err(ServiceError::NotManagedInstallation),
            }
        })();
        self.unclaim_setup(capsule_id);
        finished
    }

    /// Close the setup and uninstaller transactions the desktop before left open, once this
    /// desktop knows it started fresh: such a setup ended with that desktop (a logout, a crash),
    /// nothing else finishes it, and it would show as in progress forever, with nothing to retry
    /// or remove. After an in-place restart each is kept, and finishes as it ends.
    pub(crate) fn settle_lost_setups(&self) {
        use std::sync::atomic::Ordering;
        if self.lost_setups_settled.load(Ordering::Acquire)
            || self.displays.started_fresh() != Some(true)
            || self.lost_setups_settled.swap(true, Ordering::AcqRel)
        {
            return;
        }
        let Ok(states) = self.store.installations() else {
            return;
        };
        for state in states {
            let open = state.installation.as_ref().is_some_and(|installation| {
                matches!(
                    installation.status,
                    InstallationStatus::Installing | InstallationStatus::Uninstalling
                )
            });
            if !open || self.claim_setup(&state.id).is_err() {
                continue;
            }
            let ended = Err("It ended when the desktop stopped".to_string());
            if let Err(error) = self.finish_setup(&state.id, ended) {
                tracing::warn!(%error, "a lost Windows setup is not closed");
            }
        }
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
    ) -> Result<ExecutionOutcome, ServiceError> {
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
        Ok(ExecutionOutcome::exited(running.process.exit_code))
    }

    pub fn launch_published_vm_with_portal_file(
        &self,
        application: &VmPublishedApplication,
        workspace_id: &str,
        capabilities: &[CapOverride],
        now_unix: Option<u64>,
        source: File,
        access: windows_vm::PortalDocumentAccess,
    ) -> Result<ExecutionOutcome, ServiceError> {
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
        Ok(ExecutionOutcome::exited(running.process.exit_code))
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
        let route = self.resolve_route(
            &installer_query(&inspection),
            VmFallback::Explicit,
            &app_id,
            None,
        )?;
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

    pub(crate) fn install_in_capsule(
        &self,
        mut source: File,
        source_name: &str,
        workspace_id: &str,
        capabilities: &[CapOverride],
        now_unix: Option<u64>,
        expected: &VmInstallerInput,
    ) -> Result<StartedSetup, ServiceError> {
        validate_launch_id("workspace id", workspace_id)?;
        let inspection = inspect_installer(&mut source, source_name)?;
        if &vm_installer_input(&inspection) != expected {
            return Err(ServiceError::VmInstallConfirmationMismatch);
        }
        let _mutation = self
            .installation_lock
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
    pub(crate) fn retry_install(
        &self,
        capsule_id: &str,
        workspace_id: &str,
        capabilities: &[CapOverride],
        now_unix: Option<u64>,
    ) -> Result<StartedSetup, ServiceError> {
        validate_launch_id("capsule id", capsule_id)?;
        validate_launch_id("workspace id", workspace_id)?;
        let _mutation = self
            .installation_lock
            .lock()
            .map_err(|_| ServiceError::StateLock)?;
        self.not_in_setup(capsule_id)?;
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
            &inspection,
            workspace_id,
            capabilities,
            now_unix,
        )
    }

    pub fn escalate_install_to_vm(
        &self,
        capsule_id: &str,
        workspace_id: &str,
    ) -> Result<VmInstallation, ServiceError> {
        validate_launch_id("capsule id", capsule_id)?;
        validate_launch_id("workspace id", workspace_id)?;
        let _mutation = self
            .installation_lock
            .lock()
            .map_err(|_| ServiceError::StateLock)?;
        self.not_in_setup(capsule_id)?;
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
            .installation_lock
            .lock()
            .map_err(|_| ServiceError::StateLock)?;
        self.not_in_setup(capsule_id)?;
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
            .installation_lock
            .lock()
            .map_err(|_| ServiceError::StateLock)?;
        self.not_in_setup(capsule_id)?;
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
    pub(crate) fn vendor_uninstall(
        &self,
        capsule_id: &str,
        workspace_id: &str,
        capabilities: &[CapOverride],
        now_unix: Option<u64>,
    ) -> Result<StartedSetup, ServiceError> {
        validate_launch_id("capsule id", capsule_id)?;
        validate_launch_id("workspace id", workspace_id)?;
        let _mutation = self
            .installation_lock
            .lock()
            .map_err(|_| ServiceError::StateLock)?;
        self.not_in_setup(capsule_id)?;
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
        if let Some(journal) = state.installation.as_mut() {
            journal.status = InstallationStatus::Uninstalling;
            journal.last_error = None;
        }
        let program = CapsuleProgram {
            id: VENDOR_UNINSTALLER.to_string(),
            display_name: format!("Uninstall {}", installation.display_name),
            prefix_relative_executable: uninstaller,
            query,
            kind: ProgramKind::InstallerControl,
            arguments: Vec::new(),
            opens_with: None,
        };
        self.start_setup(
            state,
            &route,
            &program,
            workspace_id,
            capabilities,
            now_unix,
        )
    }

    fn install_inspected(
        &self,
        source: &mut File,
        inspection: &InstallerInspection,
        workspace_id: &str,
        capabilities: &[CapOverride],
        now_unix: Option<u64>,
    ) -> Result<StartedSetup, ServiceError> {
        let query = installer_query(inspection);
        let app_id = imported_app_id(&inspection.sha256);
        let route = self.resolve_route(&query, VmFallback::Disabled, &app_id, None)?;
        if route.runtime.layer == Layer::Vm {
            return Err(ServiceError::VmInstallNeedsExplicitFallback);
        }
        let mut state = self.store.ensure_for_plan(&route)?;
        self.not_in_setup(&state.id)?;
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
            inspection,
            workspace_id,
            capabilities,
            now_unix,
        )
    }

    fn run_setup(
        &self,
        mut state: CapsuleState,
        route: &LaunchPlan,
        inspection: &InstallerInspection,
        workspace_id: &str,
        granted: &[CapOverride],
        now_unix: Option<u64>,
    ) -> Result<StartedSetup, ServiceError> {
        let control = installer_control(&state, inspection)?;
        if let Some(installation) = state.installation.as_mut() {
            installation.status = InstallationStatus::Installing;
            installation.last_error = None;
        }
        self.start_setup(state, route, &control, workspace_id, granted, now_unix)
    }

    /// Start `program`, the setup or the vendor uninstaller of `state`, whose open transaction
    /// `state` holds: answered once it started, as a launch is. It goes on behind a display of
    /// its own, through a desktop restart, and [`Self::finish_setup`] closes the transaction
    /// when it ended.
    fn start_setup(
        &self,
        mut state: CapsuleState,
        route: &LaunchPlan,
        program: &CapsuleProgram,
        workspace_id: &str,
        granted: &[CapOverride],
        now_unix: Option<u64>,
    ) -> Result<StartedSetup, ServiceError> {
        let app_id = imported_app_id(
            &state
                .installation
                .as_ref()
                .ok_or(ServiceError::NotManagedInstallation)?
                .sha256,
        );
        self.claim_setup(&state.id)?;
        let capabilities = CapabilityPlan::for_app(&app_id, granted, now_unix);
        let entry_id = crate::store::managed_program_id(&state.id, &program.id);
        let started = self
            .store
            .save(&state)
            .map_err(ServiceError::from)
            .and_then(|()| {
                let lease = self.resources.materialize(ResourceRequest {
                    app_id: &app_id,
                    workspace_id,
                    capabilities: &capabilities,
                })?;
                match self.start_program(
                    route,
                    &state,
                    program,
                    &Started {
                        app_id: &app_id,
                        entry_id: &entry_id,
                        workspace_id,
                        capabilities: &capabilities,
                        lease: &lease,
                    },
                ) {
                    Ok((_, started)) => Ok((started, lease)),
                    Err(error) => {
                        self.resources.revoke(&lease.id);
                        Err(error)
                    }
                }
            });
        let (started, lease) = match started {
            Ok(started) => started,
            Err(error) => {
                self.unclaim_setup(&state.id);
                if let Some(installation) = state.installation.as_mut() {
                    // An uninstaller that did not start leaves the program installed.
                    installation.status = match installation.status {
                        InstallationStatus::Uninstalling => InstallationStatus::NeedsAttention,
                        _ => InstallationStatus::Failed,
                    };
                    installation.last_error = Some(bounded_error(&error.to_string()));
                }
                self.store.save(&state)?;
                return Err(error);
            }
        };
        let installation = state
            .installation
            .as_ref()
            .ok_or(ServiceError::NotManagedInstallation)?;
        let resources = Arc::clone(&self.resources);
        Ok(StartedSetup {
            receipt: InstallReceipt {
                capsule_id: state.id.clone(),
                display_name: installation.display_name.clone(),
                route: RouteKind::from(route.runtime.layer),
                status: installation.status,
                discovered_programs: 0,
                exit_code: None,
                detail: install_detail(installation.status, 0),
            },
            entry_id,
            display_name: program.display_name.clone(),
            program: started,
            release: Box::new(move || resources.revoke(&lease.id)),
        })
    }

    /// The setup of `state` ended as `outcome` says: the programs it installed are its launchers.
    fn setup_ended(
        &self,
        mut state: CapsuleState,
        outcome: Result<ExecutionOutcome, String>,
    ) -> Result<InstallationStatus, ServiceError> {
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
        let outcome = match outcome {
            Ok(outcome) => outcome,
            Err(error) => {
                if let Some(installation) = state.installation.as_mut() {
                    installation.status = InstallationStatus::Failed;
                    installation.last_error = Some(bounded_error(&error));
                }
                self.store.save(&state)?;
                return Ok(InstallationStatus::Failed);
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
            programs.push(CapsuleProgram {
                id: "portable-application".to_string(),
                display_name: inspection.display_name.clone(),
                prefix_relative_executable: installation.installer_relative_path,
                query: installer_query(&inspection),
                kind: ProgramKind::Application,
                arguments: Vec::new(),
                opens_with: None,
            });
        }
        // A program found again keeps how a person chose to open it.
        for program in &mut programs {
            program.opens_with = state
                .programs
                .get(&program.id)
                .and_then(|before| before.opens_with);
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
        Ok(status)
    }

    /// The vendor uninstaller of `state` ended as `outcome` says: a success removes the launchers
    /// and keeps the prefix, so leftover data is deleted only as a separate choice.
    fn uninstall_ended(
        &self,
        mut state: CapsuleState,
        outcome: &Result<ExecutionOutcome, String>,
    ) -> Result<InstallationStatus, ServiceError> {
        let (success, exit_code) = match outcome {
            Ok(outcome) => (outcome.success, outcome.exit_code),
            Err(_) => (false, None),
        };
        let status = if success {
            state
                .programs
                .retain(|_, program| program.kind == ProgramKind::InstallerControl);
            InstallationStatus::Uninstalled
        } else {
            InstallationStatus::NeedsAttention
        };
        if let Some(journal) = state.installation.as_mut() {
            journal.status = status;
            journal.last_exit_code = exit_code;
            journal.last_error = (!success).then(|| match outcome {
                Err(error) => bounded_error(error),
                Ok(_) => "The vendor uninstaller did not complete successfully".to_string(),
            });
        }
        self.store.save(&state)?;
        Ok(status)
    }

    /// An installed program starts in the capsule its setup put it in (`capsule_id`): one setup
    /// installs several programs into one capsule, while each program has its own app id.
    fn resolve_route(
        &self,
        query: &ApplicationQuery,
        vm_fallback: VmFallback,
        app_id: &str,
        capsule_id: Option<&str>,
    ) -> Result<LaunchPlan, ServiceError> {
        let mut route = self.catalogue.resolver()?.resolve(query, vm_fallback);
        if let Some(capsule_id) = capsule_id
            && matches!(route.runtime.layer, Layer::Wine | Layer::Proton)
        {
            route.runtime.id = capsule_id.to_string();
        } else if route.record_id.is_none() && route.runtime.layer == Layer::Wine {
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

/// The route and capsule `program` opens with: its capsule's, or the layer a person chose for
/// it over the same prefix. A Wine prefix opens under Proton as it is: Proton takes a prefix
/// without its version stamp as one to bring up to date, and the 64-bit prefix Wine made is
/// one it opens. A 32-bit prefix is not, so that choice is refused.
fn opened_as_chosen(
    route: &LaunchPlan,
    capsule: &CapsuleState,
    program: &CapsuleProgram,
) -> Result<(LaunchPlan, CapsuleState), ServiceError> {
    let Some(layer) = program.opens_with.filter(|layer| *layer != capsule.layer) else {
        return Ok((route.clone(), capsule.clone()));
    };
    if layer == Layer::Proton && capsule.settings.architecture == Architecture::Win32 {
        return Err(ServiceError::ProtonNeeds64Bit);
    }
    let route = route.opened_with(layer)?;
    let capsule = CapsuleState {
        layer,
        runner: route.runner.clone(),
        ..capsule.clone()
    };
    Ok((route, capsule))
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
        id: INSTALLER_CONTROL.to_string(),
        display_name: format!("Install {}", inspection.display_name),
        prefix_relative_executable: executable,
        query: installer_query(inspection),
        kind: ProgramKind::InstallerControl,
        arguments,
        opens_with: None,
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
            .contains(&tilo_permissions::caps::DeviceCap::Microphone),
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
            RouteKind::Wine | RouteKind::Proton => format!(
                "Runs in isolated capsule {} with {runner}",
                route.runtime.id
            ),
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
    #[error("this application's setup is still open; finish or close it first")]
    SetupOpen,
    #[error("Proton opens only programs set up for 64-bit Windows; this one opens with Wine")]
    ProtonNeeds64Bit,
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

    use crate::{LaunchLease, ResourceError, SandboxCommand, SystemRuntimeRegistry};

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
        fn command(&self, _: &CapsuleCommandPlan) -> Result<SandboxCommand, ExecutionError> {
            Ok(SandboxCommand::new("true", [""; 0]))
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
            Arc::new(LaunchDisplay {
                uid: 1000,
                runtime_dir: PathBuf::from("/xdg/1000"),
                wayland_socket: PathBuf::from("/xdg/1000/tilo-ws-test"),
                render_node: Some(PathBuf::from("/dev/dri/renderD128")),
            }),
        )
        .unwrap()
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn vm_routing_is_explicit_visible_and_publication_bound() {
        {
            let directory = TempDir::new().unwrap();
            let service = service(&directory);
            let query = ApplicationQuery::executable("special.exe");
            let status = service
                .explain(&query, VmFallback::Disabled, "app", None, &[], Some(1))
                .unwrap();
            assert_eq!(status.route, RouteKind::Wine);

            let result = service.launch(&LaunchInput {
                app_id: "app".to_string(),
                workspace_id: "workspace".to_string(),
                program_id: "special".to_string(),
                capsule_id: None,
                query,
                vm_fallback: VmFallback::Disabled,
                capabilities: Vec::new(),
                now_unix: Some(1),
            });
            assert!(matches!(result, Err(ServiceError::ProgramNotRegistered(_))));
            assert_eq!(service.vm.calls.load(Ordering::SeqCst), 0);
        }
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
            let setup = service
                .install_in_capsule(
                    installer.reopen().unwrap(),
                    "Favorite_App.exe",
                    "workspace",
                    &[],
                    Some(1),
                    &input,
                )
                .unwrap();
            let capsule = setup.receipt.capsule_id;
            assert!(
                matches!(
                    service.escalate_install_to_vm(&capsule, "workspace"),
                    Err(ServiceError::SetupOpen)
                ),
                "nothing else changes a capsule while its setup is open"
            );
            let ended = setup.program.wait().map_err(|error| error.to_string());
            service.finish_setup(&capsule, ended).unwrap();

            let status = service
                .escalate_install_to_vm(&capsule, "workspace")
                .unwrap();

            assert_eq!(status.phase, VmInstallationPhase::Starting);
            assert_eq!(service.vm.install_calls.load(Ordering::SeqCst), 1);
        }
        {
            let directory = TempDir::new().unwrap();
            let service = service(&directory);
            let status = service
                .explain(
                    &ApplicationQuery::executable("special.exe"),
                    VmFallback::Explicit,
                    "app",
                    None,
                    &[],
                    Some(1),
                )
                .unwrap();
            assert_eq!(status.route, RouteKind::Vm);
            assert!(status.vm_fallback_was_explicit);
        }
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
                Arc::new(LaunchDisplay {
                    uid: 1000,
                    runtime_dir: PathBuf::from("/xdg/1000"),
                    wayland_socket: PathBuf::from("/xdg/1000/tilo-ws-test"),
                    render_node: Some(PathBuf::from("/dev/dri/renderD128")),
                }),
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

    /// A catalogue routing `probe.exe` to Wine.
    fn probe_catalogue() -> Catalogue {
        Catalogue::from_json(
            r#"{
              "schema_version": 1,
              "records": [{
                "id": "probe",
                "version": 1,
                "provenance": "built_in",
                "signed": true,
                "matches": [{"executable": "probe.exe"}],
                "recipe": {
                  "layer": "wine",
                  "runner": {"family": "wine", "version": "system"},
                  "settings": {},
                  "security_profile": "windows-app"
                }
              }]
            }"#,
        )
        .unwrap()
    }

    /// The display a program shows its windows on is the one its launch finds: none before the
    /// session bound one, and the bound one after.
    #[test]
    fn a_program_is_given_the_display_its_launch_finds() {
        struct Later(Mutex<Option<LaunchDisplay>>);

        impl LaunchDisplays for Later {
            fn display_for(&self, _: &str) -> Result<LaunchDisplay, String> {
                self.0
                    .lock()
                    .unwrap()
                    .clone()
                    .ok_or_else(|| "not bound yet".to_string())
            }
        }

        struct Seen(Arc<Mutex<Vec<String>>>);

        impl SandboxExecutor for Seen {
            fn command(&self, plan: &CapsuleCommandPlan) -> Result<SandboxCommand, ExecutionError> {
                plan.bwrap_argv.clone_into(&mut self.0.lock().unwrap());
                Ok(SandboxCommand::new("true", [""; 0]))
            }
        }

        let directory = TempDir::new().unwrap();
        let catalogue = probe_catalogue();
        let query = ApplicationQuery::executable("probe.exe");
        let store = CapsuleStore::new(directory.path()).unwrap();
        let route = catalogue
            .resolver()
            .unwrap()
            .resolve(&query, VmFallback::Disabled);
        let capsule = store.ensure_for_plan(&route).unwrap();
        store
            .register_program(
                &capsule.id,
                CapsuleProgram {
                    id: "probe".to_string(),
                    display_name: "Probe".to_string(),
                    prefix_relative_executable: PathBuf::from("drive_c/probe.exe"),
                    query: query.clone(),
                    kind: crate::ProgramKind::Application,
                    arguments: Vec::new(),
                    opens_with: None,
                },
            )
            .unwrap();
        let later = Arc::new(Later(Mutex::new(None)));
        let argv = Arc::new(Mutex::new(Vec::new()));
        let service = CompatibilityService::new(
            catalogue,
            store,
            SystemRuntimeRegistry::assuming_provider(),
            Resources,
            Seen(Arc::clone(&argv)),
            Vm::default(),
            Arc::clone(&later) as Arc<dyn LaunchDisplays>,
        )
        .unwrap();
        let input = LaunchInput {
            app_id: "app".to_string(),
            workspace_id: "workspace".to_string(),
            program_id: "probe".to_string(),
            capsule_id: None,
            query,
            vm_fallback: VmFallback::Disabled,
            capabilities: Vec::new(),
            now_unix: Some(1),
        };

        assert!(matches!(
            service.launch(&input),
            Err(ServiceError::Plan(PlanError::NoDisplay(_)))
        ));
        *later.0.lock().unwrap() = Some(LaunchDisplay {
            uid: 1000,
            runtime_dir: PathBuf::from("/xdg/1000"),
            wayland_socket: PathBuf::from("/xdg/1000/wayland-3"),
            render_node: Some(PathBuf::from("/dev/dri/renderD128")),
        });
        let launched = service.launch(&input).unwrap();
        assert!(launched.program.wait().unwrap().success);
        (launched.release)();
        let argv = argv.lock().unwrap().clone();
        assert!(
            argv.windows(3).any(|bind| bind
                == [
                    "--ro-bind",
                    "/xdg/1000/wayland-3",
                    "/run/user/1000/wayland-0"
                ]),
            "the bound display is the program's: {argv:?}"
        );
    }

    /// One setup installs several programs into one capsule, and each program gets its own app
    /// id: a program launches in the capsule its setup filled, not in one its app id would name.
    #[test]
    fn an_installed_program_launches_in_the_capsule_its_setup_filled() {
        let directory = TempDir::new().unwrap();
        let service = service(&directory);
        let mut setup = service
            .resolve_route(
                &ApplicationQuery::executable("setup.exe"),
                VmFallback::Disabled,
                &imported_app_id("setup"),
                None,
            )
            .unwrap();
        assert_eq!(setup.runtime.layer, Layer::Wine);
        setup.runtime.id = "wine-setup".to_string();
        let capsule = service.store.ensure_for_plan(&setup).unwrap();
        for (id, name) in [("tool", "Tool"), ("helper", "Helper")] {
            service
                .store
                .register_program(
                    &capsule.id,
                    CapsuleProgram {
                        id: id.to_string(),
                        display_name: name.to_string(),
                        prefix_relative_executable: PathBuf::from(format!("drive_c/{id}.exe")),
                        query: ApplicationQuery::executable(format!("{id}.exe")),
                        kind: crate::ProgramKind::Application,
                        arguments: Vec::new(),
                        opens_with: None,
                    },
                )
                .unwrap();
        }

        for entry in service.store.registered_programs().unwrap() {
            assert_ne!(entry.app_id, capsule.id);
            let status = service
                .explain(
                    &entry.query,
                    VmFallback::Disabled,
                    &entry.app_id,
                    Some(&entry.capsule_id),
                    &[],
                    Some(1),
                )
                .unwrap();
            assert_eq!(status.route, RouteKind::Wine);
            let launched = service
                .launch(&LaunchInput {
                    app_id: entry.app_id.clone(),
                    workspace_id: "workspace".to_string(),
                    program_id: entry.program.id.clone(),
                    capsule_id: Some(entry.capsule_id.clone()),
                    query: entry.query.clone(),
                    vm_fallback: VmFallback::Disabled,
                    capabilities: Vec::new(),
                    now_unix: Some(1),
                })
                .unwrap_or_else(|error| panic!("{} launches: {error}", entry.program.id));
            assert!(launched.program.wait().unwrap().success);
            (launched.release)();
        }
        assert_eq!(
            service.store.registered_programs().unwrap().len(),
            2,
            "no launch made a capsule of its own"
        );
    }

    /// A program added from a file opens with Proton once a person chose it: over the prefix its
    /// setup filled, with the recipe for any program, while the program beside it in
    /// that capsule keeps opening with Wine. The choice is saved with the program, and back to
    /// the recommendation opens it with Wine again. The appliance and a 32-bit prefix are no
    /// choice for it.
    #[test]
    #[cfg_attr(
        not(target_arch = "x86_64"),
        ignore = "Proton is offered on x86 computers only: an ARM computer refuses it, keeps \
                  Wine, and plan's own test holds that"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one program's choice is followed through every launch it changes"
    )]
    fn a_program_opens_with_proton_once_a_person_chose_it() {
        struct Seen(Arc<Mutex<Vec<String>>>);

        impl SandboxExecutor for Seen {
            fn command(&self, plan: &CapsuleCommandPlan) -> Result<SandboxCommand, ExecutionError> {
                plan.bwrap_argv.clone_into(&mut self.0.lock().unwrap());
                Ok(SandboxCommand::new("true", [""; 0]))
            }
        }

        let directory = TempDir::new().unwrap();
        let argv = Arc::new(Mutex::new(Vec::new()));
        let service = CompatibilityService::new(
            catalogue(),
            CapsuleStore::new(directory.path()).unwrap(),
            SystemRuntimeRegistry::assuming_provider(),
            Resources,
            Seen(Arc::clone(&argv)),
            Vm::default(),
            Arc::new(LaunchDisplay {
                uid: 1000,
                runtime_dir: PathBuf::from("/xdg/1000"),
                wayland_socket: PathBuf::from("/xdg/1000/tilo-ws-test"),
                render_node: Some(PathBuf::from("/dev/dri/renderD128")),
            }),
        )
        .unwrap();
        let mut setup = service
            .resolve_route(
                &ApplicationQuery::executable("setup.exe"),
                VmFallback::Disabled,
                &imported_app_id("setup"),
                None,
            )
            .unwrap();
        setup.runtime.id = "wine-setup".to_string();
        let capsule = service.store.ensure_for_plan(&setup).unwrap();
        assert_eq!(capsule.layer, Layer::Wine);
        for id in ["tool", "helper"] {
            service
                .store
                .register_program(
                    &capsule.id,
                    CapsuleProgram {
                        id: id.to_string(),
                        display_name: id.to_string(),
                        prefix_relative_executable: PathBuf::from(format!("drive_c/{id}.exe")),
                        query: ApplicationQuery::executable(format!("{id}.exe")),
                        kind: crate::ProgramKind::Application,
                        arguments: Vec::new(),
                        opens_with: None,
                    },
                )
                .unwrap();
        }
        let entry = |id: &str| {
            service
                .store
                .registered_programs()
                .unwrap()
                .into_iter()
                .find(|entry| entry.program.id == id)
                .unwrap()
        };
        let launch = |id: &str| {
            let entry = entry(id);
            let launched = service
                .launch(&LaunchInput {
                    app_id: entry.app_id.clone(),
                    workspace_id: "workspace".to_string(),
                    program_id: entry.program.id.clone(),
                    capsule_id: Some(entry.capsule_id.clone()),
                    query: entry.query.clone(),
                    vm_fallback: VmFallback::Disabled,
                    capabilities: Vec::new(),
                    now_unix: Some(1),
                })
                .unwrap_or_else(|error| panic!("{id} launches: {error}"));
            assert!(launched.program.wait().unwrap().success);
            (launched.release)();
            let argv = argv.lock().unwrap().clone();
            let explained = service
                .explain_program(
                    &entry.query,
                    &entry.app_id,
                    &entry.capsule_id,
                    &entry.program,
                    &[],
                    Some(1),
                )
                .unwrap();
            (argv, explained)
        };
        // Proton's own script starts a Proton program.
        let proton = |argv: &[String]| {
            argv.windows(3)
                .any(|words| words[0] == "/usr/bin/python3" && words[1].ends_with("/proton"))
        };
        let set = |environment: &str| -> Option<String> {
            let argv = argv.lock().unwrap();
            argv.windows(3)
                .find(|words| words[0] == "--setenv" && words[1] == environment)
                .map(|words| words[2].clone())
        };

        service
            .set_opens_with(&capsule.id, "tool", Some(RouteKind::Proton))
            .unwrap();
        let (tool, (status, recommended)) = launch("tool");
        assert_eq!(status.route, RouteKind::Proton, "it opens with Proton now");
        assert_eq!(
            recommended,
            RouteKind::Wine,
            "Wine stays the recommendation"
        );
        assert_eq!(status.runner, "umu-proton UMU-Proton-10.0-4");
        assert!(proton(&tool), "Proton starts it: {tool:?}");
        assert_eq!(
            set("GAMEID").as_deref(),
            Some(compat_catalogue::PROTON_RECIPE_FOR_ANY_PROGRAM)
        );
        let prefix = (set("WINEPREFIX"), set("STEAM_COMPAT_DATA_PATH"));
        let bound = |argv: &[String]| -> Vec<String> {
            argv.windows(3)
                .filter(|words| words[0] == "--bind-fd")
                .map(|words| words[2].clone())
                .collect()
        };
        let tool_files = bound(&tool);
        assert!(
            tool.last()
                .is_some_and(|word| word.ends_with("drive_c/tool.exe")),
            "{tool:?}"
        );

        let (helper, (status, _)) = launch("helper");
        assert_eq!(status.route, RouteKind::Wine, "its neighbour keeps Wine");
        assert_eq!(
            (set("WINEPREFIX"), set("STEAM_COMPAT_DATA_PATH")),
            prefix,
            "Proton opens the prefix Wine does"
        );
        assert_eq!(bound(&helper), tool_files, "over the capsule's own files");
        assert!(!proton(&helper));
        assert_eq!(set("GAMEID"), None);

        let reloaded = CapsuleStore::new(directory.path())
            .unwrap()
            .load(&capsule.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            reloaded.program("tool").unwrap().opens_with,
            Some(Layer::Proton),
            "the choice is saved with the program"
        );
        assert_eq!(reloaded.layer, Layer::Wine, "the capsule is unchanged");

        service.set_opens_with(&capsule.id, "tool", None).unwrap();
        let (tool, (status, _)) = launch("tool");
        assert_eq!(status.route, RouteKind::Wine);
        assert!(!proton(&tool));
        service
            .set_opens_with(&capsule.id, "tool", Some(RouteKind::Wine))
            .unwrap();
        assert_eq!(
            entry("tool").program.opens_with,
            None,
            "choosing what it is recommended to open with is no choice to keep"
        );

        assert!(matches!(
            service.set_opens_with(&capsule.id, "tool", Some(RouteKind::Vm)),
            Err(ServiceError::Catalogue(CatalogueError::InvalidOpensWith))
        ));
        let mut state = service.store.load(&capsule.id).unwrap().unwrap();
        state.settings.architecture = Architecture::Win32;
        service.store.save(&state).unwrap();
        assert!(matches!(
            service.set_opens_with(&capsule.id, "tool", Some(RouteKind::Proton)),
            Err(ServiceError::ProtonNeeds64Bit)
        ));
    }
}
