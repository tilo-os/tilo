//! Tilo's single broker for launching Windows software.
//!
//! The catalogue chooses a route, but grants no authority. This service
//! resolves the ordinary [`collab_types::caps::CapOverride`] grants through
//! [`launcher::capabilities::CapabilityPlan`], then projects that result into
//! a fixed Wine/UMU Bubblewrap shape or passes it unchanged to the managed VM
//! delegate. It deliberately defines no parallel permission vocabulary.
//!
//! Catalogue strings can choose only registered runners and allow-listed
//! settings. They can never supply a host path, environment variable name,
//! executable path, Bubblewrap flag, or VM command.

#![allow(
    clippy::missing_errors_doc,
    reason = "boundary errors are represented by and documented on typed error enums"
)]

mod assembly;
mod catalogue;
mod dxvk;
mod execute;
mod installer;
mod plan;
mod programs;
mod protocol;
mod resources;
mod saved;
mod service;
mod session;
mod store;
mod translated;
mod vm_adapter;
mod vm_network;

pub use assembly::{Assembly, LazyVmDelegate, SessionSide, serve};
pub use catalogue::{
    CatalogueLoadError, CatalogueLoadReport, ShippedCataloguePaths, load_shipped_catalogue,
};
pub use execute::{
    BwrapExecutor, ChildProgram, ExecutionError, ExecutionOutcome, SECCOMP_CHILD_FD,
    SandboxCommand, SandboxExecutor, StartedProgram, last_words,
};
pub use installer::{
    InstallerError, InstallerInspection, MAX_INSTALLER_BYTES, PeMachine, inspect_installer,
};
#[cfg(feature = "test-support")]
pub use plan::test_lease_root_at;
pub use plan::{
    AudioBinding, AudioProtocol, CapsuleCommandPlan, KeptDisplay, KeptProgram, LaunchDisplay,
    LaunchDisplays, LaunchLease, LaunchResourceBroker, NetworkShape, PlanError, ResourceError,
    ResourceRequest, RuntimeImage, RuntimeRegistry, SandboxPlanner, SystemRuntimeRegistry,
    TakenBack, is_render_node,
};
pub use protocol::{
    ManagedApplication, ManagedInstallation, OPEN_WITH_OTHER_RUNNER, PROTOCOL_VERSION, ProgramEnd,
    ProtocolError, ProtocolRequest, ProtocolResponse, ResolveRequest,
};
pub use resources::SystemResourceBroker;
pub use saved::{EntryDirectory, ReopenAnswer, ReopenAsk, Reopenings};
pub use service::{
    AccessSummary, CompatibilityService, ExplainStatus, InstallOutcome, InstallReceipt,
    LaunchInput, LaunchReceipt, Launched, MatchReason, RouteKind, ServiceError, VmAvailability,
    VmCandidatePublication, VmDelegate, VmInstallCandidate, VmInstallConfirmation, VmInstallation,
    VmInstallationPhase, VmInstallerInput, VmInstallerKind, VmInventory, VmPresentation,
    VmPublishedApplication, VmRouteRequest, VmSharedSessionContributor, VmSharedSessionGrantKind,
    VmSharedSessionLeaseSummary, VmUninstallation,
};
pub use session::{
    CapabilityAuthority, CompatSessionHandle, LauncherAuthenticator, ManagedCompatibility,
    SessionClient, SessionError, SessionHandoff, TrustedLauncherRegistry,
    account_administers_computer, spawn_session_service,
};
pub use store::{
    CAPSULE_SCHEMA_VERSION, CapsuleError, CapsuleProgram, CapsuleState, CapsuleStore,
    InstallationRecord, InstallationStatus, InstallerKind, ProgramKind, RegisteredProgram,
};
pub use translated::{run_translated_command, wine_is_translated};
pub use vm_adapter::{
    AudioIsolatedRailLauncher, AudioIsolatedSpiceLauncher, CanonicalRailPortalAuthority,
    LiveVmDelegate, PortalRailPortalAuthority, RAIL_PRESENTER, SharedSessionCapabilityBroker,
    VmPortalAuthority, VmSharedSessionTracker, vm_app_id, vm_entry_id,
};
pub use vm_network::VmNetworkLeases;
