//! qOS's single broker for launching Windows software.
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

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::string_slice,
        clippy::panic,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]
#![allow(
    clippy::missing_errors_doc,
    reason = "boundary errors are represented by and documented on typed error enums"
)]

mod catalogue;
mod execute;
mod plan;
mod protocol;
mod resources;
mod service;
mod session;
mod store;

pub use catalogue::{
    CatalogueLoadError, CatalogueLoadReport, ShippedCataloguePaths, load_shipped_catalogue,
};
pub use execute::{
    BwrapExecutor, ExecutionError, ExecutionOutcome, SECCOMP_CHILD_FD, SandboxExecutor,
};
pub use plan::{
    AudioBinding, AudioProtocol, CapsuleCommandPlan, HostSession, LaunchLease,
    LaunchResourceBroker, NetworkShape, PlanError, ResourceError, ResourceRequest, RuntimeImage,
    RuntimeRegistry, SandboxPlanner, SystemRuntimeRegistry,
};
pub use protocol::{
    ManagedApplication, PROTOCOL_VERSION, ProtocolError, ProtocolRequest, ProtocolResponse,
    ResolveRequest,
};
pub use resources::SystemResourceBroker;
pub use service::{
    AccessSummary, CompatibilityService, ExplainStatus, LaunchInput, LaunchReceipt, MatchReason,
    RouteKind, ServiceError, VmDelegate, VmRouteRequest,
};
pub use session::{
    CapabilityAuthority, CompatSessionHandle, LauncherAuthenticator, ManagedCompatibility,
    SessionClient, SessionError, SessionHandoff, TrustedLauncherRegistry, spawn_session_service,
};
pub use store::{
    CAPSULE_SCHEMA_VERSION, CapsuleError, CapsuleProgram, CapsuleState, CapsuleStore,
    RegisteredProgram,
};
