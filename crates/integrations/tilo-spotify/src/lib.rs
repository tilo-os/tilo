//! Platform entry point for Spotify credential storage and native streaming.
//! Music depends on this facade; credentials remain in the system account vault.

pub mod ownership {
    pub use account_credentials::{
        Authorization, Capability, ClientTable, CredentialError, CredentialStore, OAuthIssuer,
        OAuthUse, Provider, Refresh, Registration, Revocation, TokenSet,
    };
}
pub use accounts::AccountId;
pub const STREAMING_ACCOUNT: &str = "music.spotify.streaming";
pub const STREAMING_USE: account_credentials::OAuthUse = account_credentials::OAuthUse::new(
    account_credentials::OAuthIssuer::Spotify,
    account_credentials::Capability::MusicStreaming,
);

#[cfg(feature = "native")]
mod native;
#[cfg(feature = "native")]
pub use native::{CaptureOutput, NativeAudio, Output, self_check};
