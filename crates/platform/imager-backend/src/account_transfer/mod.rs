//! Consented account discovery and connection. Discovery never reads cookie,
//! token or password databases. Session files remain private migration data;
//! a foreign app's grant is not a grant for Tilo's replacement app.

pub mod actions;
mod connect;
pub(crate) mod dav_readiness;
mod discovery;
#[cfg(target_os = "linux")]
pub mod goa_pending;
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
mod thunderbird_source;
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
#[cfg(target_os = "linux")]
pub use goa_pending::{AccountImportReceipt, import_selected as import_selected_goa};
pub use portable::{PasswordSealedAccounts, SealedAccounts};
pub use thunderbird_source::{
    MAX_THUNDERBIRD_ARTIFACT, PasswordSealedThunderbird, SealedThunderbird,
    ThunderbirdAccountReceipt, ThunderbirdAccountSelection, ThunderbirdFolderReceipt,
    ThunderbirdImportReceipt, ThunderbirdSelection, import_reviewed, import_selected,
    seal_selected,
};
pub use tilo_sources::TiloCandidate;

#[cfg(test)]
mod tests;
