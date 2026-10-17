//! Consented account discovery and connection. Discovery never reads cookie,
//! token or password databases. Session files remain private migration data;
//! a foreign app's grant is not a grant for Tilo's replacement app.

pub mod actions;
mod connect;
pub(crate) mod dav_readiness;
mod discovery;
#[cfg(target_os = "linux")]
mod goa_source;
#[cfg(target_os = "linux")]
mod goa_variant;
#[cfg(target_os = "linux")]
mod nextcloud_source;
mod portable;
#[cfg(unix)]
mod rclone_oauth;
pub mod spotify_identity;
mod tilo_sources;
mod windows_source;
pub use windows_source::{
    WindowsProfile, discover_windows_sources, native_windows_profiles, private_windows_hints,
};

pub use actions::{
    AccountActions, AccountJob, AccountJobState, ActionError, ConnectionProvider, ConsentLink,
    DavConnection, DestinationGuard,
};
pub use connect::{ConnectionError, Connector};
pub use discovery::discover_home_profiles;
pub use discovery::{Candidate, Inventory, Origin, Provider, SourceRoots, discover};
pub use portable::{PasswordSealedAccounts, SealedAccounts};
pub use tilo_sources::TiloCandidate;

#[cfg(test)]
mod tests;
