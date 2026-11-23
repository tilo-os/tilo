//! Online account credentials: sign-in, sealing and refresh.
//!
//! Settings connects Google Drive, `OneDrive`, Dropbox, Nextcloud, `WebDAV`
//! and `iCloud` Drive accounts; the storage daemon mounts them. Mail signs
//! in to Gmail and Outlook. Every `OAuth` sign-in uses Tilo's one client
//! registration per provider ([`ClientTable`], `oauth-clients.toml`), with
//! the scopes of its use ([`OAuthUse`]: files or mail), and every grant is
//! sealed and refreshed here. The long-lived secret never leaves this crate:
//!
//! - **Settings** runs the interactive sign-ins — `OAuth` through the shared
//!   [`tilo_oauth`] client ([`CredentialStore::begin_sign_in`]), Nextcloud Login
//!   Flow v2 ([`nextcloud`]), `iCloud` Apple ID + verification code
//!   ([`icloud`]) — and seals the result (refresh token, app password,
//!   session trust state) in the account vault.
//! - **Mail** signs in through [`CredentialStore::begin_sign_in_for`], seals the
//!   grant with [`CredentialStore::seal_oauth`] and asks
//!   [`CredentialStore::oauth_access_token`] for a current access token before
//!   each `XOAUTH2` login; refresh and resealing happen here.
//! - **The model gateway** asks [`CredentialStore::oauth_access_token_with_expiry`]
//!   for signed-in agent model providers (Google Vertex AI and the Gemini
//!   API, a `ChatGPT` or GitHub Copilot subscription, Hugging Face), from a
//!   grant Settings sealed for one provider. That grant carries only its
//!   models scope ([`Capability::Models`]), apart from any Mail or Files
//!   grant. Copilot signs in by device code
//!   ([`CredentialStore::begin_device_sign_in`]); `OpenRouter` hands back an API
//!   key instead of a grant ([`openrouter`]).
//! - **The model manager** downloads gated Hugging Face models with a
//!   Hugging Face sign-in Settings made for that alone
//!   ([`Capability::ModelDownloads`], `hugging_face`).
//! - **A drive this process reads** (`tilo migrate` bringing a cloud's files
//!   over) signs in with [`DriveSignIn`]: the access token (or `iCloud`
//!   session headers) is made in memory from the grant, renewed when due, and
//!   any rotated grant is sealed back into the vault. Nothing short-lived is
//!   written to a file for another process.
//!
//! A grant the provider no longer accepts (a revoked refresh token, an
//! `iCloud` session that needs the two-factor step again) moves the account
//! to [`accounts::AccountSyncHealth::NeedsAuthentication`], which Settings
//! shows as "Sign in again".

mod clients;
#[cfg(feature = "store")]
pub mod cloud_projects;
#[cfg(feature = "store")]
mod credential_store;
#[cfg(feature = "hints")]
pub mod hints;
#[cfg(feature = "hugging-face")]
pub mod hugging_face;
#[cfg(feature = "store")]
pub mod icloud;
#[cfg(feature = "store")]
pub mod nextcloud;
#[cfg(feature = "store")]
pub mod openrouter;
#[cfg(feature = "store")]
pub mod portable;
pub mod providers;
#[cfg(feature = "store")]
pub mod service_sessions;
#[cfg(feature = "store")]
mod spotify_identity;
#[cfg(feature = "store")]
pub mod storage;
#[cfg(feature = "store")]
pub use spotify_identity::SpotifyIdentity;

pub use clients::{COPILOT_TOKEN_ENDPOINT, ClientEntry, ClientTable, Scopes};
#[cfg(feature = "store")]
pub use credential_store::{
    CredentialError, CredentialStore, DriveSignIn, GrantState, RenewedGrant, SIGN_IN_TIMEOUT,
    copied_secret, copied_source_matches, icloud_apple_id, icloud_secret, is_cloud_type,
    is_copied_secret, oauth_secret, oauth_tokens, password_credential, protected_grant_protocol,
    requires_protected_consumer,
};
pub use providers::{Capability, OAuthCloud, OAuthIssuer, OAuthUse};
#[cfg(feature = "store")]
pub use tilo_oauth::client::Authorization;
#[cfg(feature = "store")]
pub use tilo_oauth::device::{DeviceAuthorization, DeviceCancel};
pub use tilo_oauth::loopback::LoopbackCancel;
pub use tilo_oauth::{Registration, TokenSet};

#[cfg(test)]
#[cfg(feature = "store")]
mod tests;
