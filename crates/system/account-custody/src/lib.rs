//! Custody of online account credentials.
//!
//! Settings connects Google Drive, `OneDrive`, Dropbox, Nextcloud, `WebDAV`
//! and `iCloud` Drive accounts; the storage daemon mounts them. Mail signs
//! in to Gmail and Outlook. Every `OAuth` sign-in uses Tilo's one client
//! registration per provider ([`ClientTable`], `oauth-clients.toml`), with
//! the scopes of its use ([`OAuthUse`]: files or mail), and every grant is
//! sealed and refreshed here. The long-lived secret never leaves custody:
//!
//! - **Settings** runs the interactive sign-ins — `OAuth` through the shared
//!   [`tilo_oauth`] client ([`Custodian::begin_sign_in`]), Nextcloud Login
//!   Flow v2 ([`nextcloud`]), `iCloud` Apple ID + verification code
//!   ([`icloud`]) — and seals the result (refresh token, app password,
//!   session trust state) in the account vault.
//! - **Mail** signs in through [`Custodian::begin_sign_in_for`], seals the
//!   grant with [`Custodian::seal_oauth`] and asks
//!   [`Custodian::oauth_access_token`] for a current access token before
//!   each `XOAUTH2` login; refresh and resealing happen here.
//! - **The daemon** reads one account's short-lived credential from the
//!   handoff store ([`accounts::handoff`]) through
//!   [`accounts::HandoffCredentialSource`]. When it is missing, due or
//!   rejected, the daemon runs `tilo-account-credential refresh <id>`, a
//!   separate process that calls [`Custodian::publish`]: it refreshes the
//!   access token (or derives `iCloud` session headers), seals any rotated
//!   grant back into the vault and publishes only the short-lived part.
//!
//! A grant the provider no longer accepts (a revoked refresh token, an
//! `iCloud` session that needs the two-factor step again) moves the account
//! to [`accounts::AccountSyncHealth::NeedsAuthentication`], which Settings
//! shows as "Sign in again".

mod clients;
#[cfg(feature = "custody")]
mod custodian;
#[cfg(feature = "hints")]
pub mod hints;
#[cfg(feature = "custody")]
pub mod icloud;
#[cfg(feature = "custody")]
pub mod nextcloud;
#[cfg(feature = "custody")]
pub mod portable;
pub mod providers;
#[cfg(feature = "custody")]
pub mod service_sessions;
#[cfg(feature = "custody")]
mod spotify_identity;
#[cfg(feature = "custody")]
pub mod storage;
#[cfg(feature = "custody")]
pub use spotify_identity::{SpotifyIdentity, with_verified_spotify_access_token};

pub use clients::{ClientEntry, ClientTable, Scopes};
#[cfg(feature = "custody")]
pub use custodian::{
    Custodian, CustodyError, ImportedRegistration, SIGN_IN_TIMEOUT, copied_secret,
    copied_source_matches, icloud_apple_id, icloud_secret, imported_oauth_matches,
    imported_oauth_secret, is_cloud_type, is_copied_secret, oauth_secret, password_credential,
    protected_grant_protocol, requires_protected_consumer,
};
pub use providers::{Capability, OAuthCloud, OAuthIssuer, OAuthUse};
#[cfg(feature = "custody")]
pub use tilo_oauth::client::Authorization;
pub use tilo_oauth::loopback::LoopbackCancel;
pub use tilo_oauth::{Registration, TokenSet};

#[cfg(test)]
#[cfg(feature = "custody")]
mod tests;
