//! Custody of cloud account credentials, owned by Settings.
//!
//! Settings connects Google Drive, `OneDrive`, Dropbox, Nextcloud, `WebDAV`
//! and `iCloud` Drive accounts; the storage daemon mounts them. The two sides
//! never share a long-lived secret:
//!
//! - **Settings** runs the interactive sign-ins — `OAuth` through the shared
//!   [`tilo_oauth`] client ([`Custodian::begin_sign_in`]), Nextcloud Login
//!   Flow v2 ([`nextcloud`]), `iCloud` Apple ID + verification code
//!   ([`icloud`]) — and seals the result (refresh token, app password,
//!   session trust state) in the account vault.
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

mod custodian;
pub mod icloud;
pub mod nextcloud;
pub mod providers;

pub use custodian::{
    Custodian, CustodyError, SIGN_IN_TIMEOUT, icloud_apple_id, icloud_secret, is_cloud_type,
    oauth_secret,
};
pub use providers::OAuthCloud;
pub use tilo_oauth::client::Authorization;
pub use tilo_oauth::loopback::LoopbackCancel;

#[cfg(test)]
mod tests;
