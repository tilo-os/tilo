//! The security boundary around qOS's one Windows fallback appliance.
//!
//! This crate deliberately contains no permission UI and no compatibility
//! routing policy. Its caller resolves the ordinary qOS workspace grants and
//! passes an immutable projection to [`WindowsAppliance::launch`]. A
//! [`CapabilityBroker`] turns that projection into short-lived host resources;
//! neither libvirt nor `FreeRDP` is allowed to infer broader access.
//!
//! Normal operation publishes individual Windows applications as `RemoteApp`
//! windows. The Windows desktop is not a launch target. A full console is a
//! separate, administrator-authorised recovery operation and is intentionally
//! absent from the public launch API.

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
    reason = "public traits expose typed boundary failures documented by their error enums"
)]

mod appliance;
mod command;
mod config;
mod guest;
mod libvirt;
mod publication;
mod remote_app;
mod state;

pub use appliance::{
    BrokeredLaunch, CapabilityBroker, LaunchError, LaunchRequest, RunningRemoteApp, ShareBinding,
    VmLaunchGrants, WindowsAppliance,
};
pub use command::{CommandError, CommandOutput, CommandRequest, CommandRunner, StdCommandRunner};
pub use config::{
    APPLIANCE_DOMAIN_NAME, ApplianceConfig, ConfigError, CpuMode, FirmwareConfig, PerformanceConfig,
};
pub use guest::{
    AgentError, AgentHealth, GuestAgent, GuestLaunch, GuestSession, RdpEndpoint, Secret,
};
pub use libvirt::{DomainState, LibvirtBackend, VmBackend, VmBackendError, render_domain_xml};
pub use publication::{Association, AssociationKind, Catalog, CatalogError, PublishedApplication};
pub use remote_app::{FreeRdpLauncher, RemoteAppError, RemoteAppLauncher, RemoteAppProcess};
pub use state::{
    AppliancePhase, ApplianceState, LicenseState, ProvisioningError, ProvisioningStore,
};
