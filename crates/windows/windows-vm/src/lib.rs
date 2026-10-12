//! The security boundary around Tilo's one Windows fallback appliance.
//!
//! This crate deliberately contains no permission UI and no compatibility
//! routing policy. Its caller resolves the ordinary Tilo workspace grants and
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
        clippy::string_slice,
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
mod graphics;
mod guest;
mod guest_network;
mod https_guest;
mod libvirt;
mod lifecycle;
mod portal_files;
mod presentation;
mod publication;
mod remote_app;
mod state;

pub use appliance::{
    BrokeredLaunch, CapabilityBroker, LaunchError, LaunchRequest, PortalDocumentAccess,
    PortalDocumentGrant, RunningRemoteApp, ShareBinding, VmLaunchGrants, WindowsAppliance,
};
pub use command::{CommandError, CommandOutput, CommandRequest, CommandRunner, StdCommandRunner};
pub use config::{
    APPLIANCE_DOMAIN_NAME, ApplianceConfig, ConfigError, CpuMode, FirmwareConfig,
    GuestArchitecture, PerformanceConfig,
};
pub use graphics::{
    GraphicsAssignment, GraphicsCapabilities, GraphicsDeviceCapability, GraphicsProbeError,
    GraphicsRecommendation, MediatedTypeCapability, SriovCapability,
};
pub use guest::{
    AgentError, AgentHealth, GuestAgent, GuestLaunch, GuestSession, RdpEndpoint, Secret,
};
pub use guest_network::{
    GuestNetworkBinding, GuestNetworkError, GuestNetworkProxy, HOST_ONLY_GATEWAY,
};
pub use https_guest::{
    DiscoveredApplication, DiscoveredUninstaller, GuestBridgeConfig, HttpsGuestBridge,
    InstallationOperationId, InstallationState, InstallationStatus, InstallerKind,
    InstallerPackage, MAX_INSTALLER_BYTES, PreparedInstallation, PreparedUninstallation,
    PublicationResult, StagedInstaller, UninstallationOperationId, UninstallationStatus,
};
pub use libvirt::{
    DomainState, EgressLease, EgressPurpose, LibvirtBackend, VmBackend, VmBackendError,
    render_domain_xml,
};
pub use lifecycle::{
    LifecycleAction, LifecycleError, ManagedSessionState, VmStatus, WindowsVmService,
};
pub use portal_files::{PortalDocumentStore, PortalRailCapabilityBroker, portal_lease_root};
pub use presentation::{ApplicationPresentation, RecoveryPresentation};
pub use publication::{Association, AssociationKind, Catalog, CatalogError, PublishedApplication};
pub use remote_app::{
    AppFocusedSpiceLauncher, FreeRdpLauncher, RemoteAppError, RemoteAppLauncher, RemoteAppProcess,
};
pub use state::{
    AppliancePhase, ApplianceState, LicenseState, ProvisioningError, ProvisioningStore,
};
