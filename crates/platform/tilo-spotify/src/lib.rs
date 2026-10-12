//! Platform entry point for Spotify credential custody and native streaming.
//! Music depends on this facade; credentials remain in the system account vault.

pub mod custody {
    pub use account_custody::{
        Authorization, Capability, ClientTable, Custodian, OAuthIssuer, OAuthUse, TokenSet,
    };
}
pub use accounts::AccountId;
pub const STREAMING_ACCOUNT: &str = "music.spotify.streaming";
pub const STREAMING_USE: account_custody::OAuthUse = account_custody::OAuthUse::new(
    account_custody::OAuthIssuer::Spotify,
    account_custody::Capability::MusicStreaming,
);

#[cfg(feature = "native")]
mod native;
#[cfg(feature = "native")]
pub use native::{CaptureOutput, NativeAudio, Output, self_check};
