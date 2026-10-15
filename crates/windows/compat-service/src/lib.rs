//! Tilo's single broker for launching Windows software.
//!
//! The catalogue chooses a route, but grants no authority. This service
//! resolves the ordinary [`collab_types::caps::CapOverride`] records through
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

mod audio_proxy;
mod catalogue;
mod dxvk;
mod execute;
mod installer;
mod plan;
mod protocol;
mod resources;
mod service;
mod session;
mod store;
mod translated;
mod vm_adapter;

pub use audio_proxy::{AudioProxyError, AudioProxyLease};
pub use catalogue::{
    CatalogueLoadError, CatalogueLoadReport, ShippedCataloguePaths, load_shipped_catalogue,
};
pub use execute::{
    BwrapExecutor, ExecutionError, ExecutionOutcome, SECCOMP_CHILD_FD, SandboxExecutor,
};
pub use installer::{
    InstallerError, InstallerInspection, MAX_INSTALLER_BYTES, PeMachine, inspect_installer,
};
pub use plan::{
    AudioBinding, AudioProtocol, CapsuleCommandPlan, HostSession, LaunchLease,
    LaunchResourceBroker, NetworkShape, PlanError, ResourceError, ResourceRequest, RuntimeImage,
    RuntimeRegistry, SandboxPlanner, SystemRuntimeRegistry,
};
pub use protocol::{
    ManagedApplication, ManagedInstallation, PROTOCOL_VERSION, ProtocolError, ProtocolRequest,
    ProtocolResponse, ResolveRequest,
};
pub use resources::SystemResourceBroker;
pub use service::{
    AccessSummary, CompatibilityService, ExplainStatus, InstallOutcome, InstallReceipt,
    LaunchInput, LaunchReceipt, MatchReason, RouteKind, ServiceError, UnavailableVmDelegate,
    VmAvailability, VmCandidatePublication, VmDelegate, VmInstallCandidate, VmInstallConfirmation,
    VmInstallation, VmInstallationPhase, VmInstallerInput, VmInstallerKind, VmInventory,
    VmPresentation, VmPublishedApplication, VmRouteRequest, VmSharedSessionContributor,
    VmSharedSessionGrantKind, VmSharedSessionLeaseSummary, VmUninstallation,
};
pub use session::{
    CapabilityAuthority, CompatSessionHandle, LauncherAuthenticator, ManagedCompatibility,
    SessionClient, SessionError, SessionHandoff, TrustedLauncherRegistry, spawn_session_service,
};
pub use store::{
    CAPSULE_SCHEMA_VERSION, CapsuleError, CapsuleProgram, CapsuleState, CapsuleStore,
    InstallationRecord, InstallationStatus, InstallerKind, ProgramKind, RegisteredProgram,
};
pub use translated::run_translated_command;
pub use vm_adapter::{
    AudioIsolatedRailLauncher, AudioIsolatedSpiceLauncher, CanonicalRailCapabilityBroker,
    CanonicalRailPortalAuthority, DenyAllVmCapabilityBroker, DenyAllVmPortalAuthority,
    LiveVmDelegate, PortalRailPortalAuthority, SharedSessionCapabilityBroker, VmPortalAuthority,
    VmSharedSessionTracker,
};
