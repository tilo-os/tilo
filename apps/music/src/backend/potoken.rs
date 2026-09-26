//! `PoToken` — the swappable "proof-of-origin token" seam.
//!
//! `YouTube` gates *some* clients' media streams (and increasingly the player
//! response itself) behind a **`PoToken`** minted by its `BotGuard` challenge, and
//! behind SABR streaming. This module keeps minting swappable so the live
//! backend never hard-codes how a token is obtained.
//!
//! ## Why the default provider is enough for audio
//!
//! rustypipe resolves streams through the **iOS** and **TV** player clients
//! when no `BotGuard` is configured (`player_client_order = [Ios, Tv]`). Those
//! clients return direct `googlevideo` URLs **without** a `PoToken`. So
//! [`NullPoToken`] — which mints nothing — is sufficient for real audio
//! playback today, with zero extra runtime deps. The seam exists so that if
//! `YouTube` tightens iOS/TV extraction, a BotGuard-backed provider can be dropped
//! in ([`PoTokenProvider::can_stream`] flips to `true`) to unlock the Desktop /
//! `DesktopMusic` clients — without touching the backend.

/// What a token is being minted for. `BotGuard` issues distinct tokens for the
/// player request vs the media (streaming) request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoTokenKind {
    /// Token bound into the `/player` `InnerTube` request.
    Player,
    /// Token appended to the media (googlevideo) stream URL.
    Streaming,
}

/// The context a provider needs to mint a token.
#[derive(Clone, Debug, Default)]
pub struct PoTokenContext {
    /// Video being resolved (`BotGuard` binds the streaming token to this).
    pub video_id: String,
    /// `InnerTube` `visitorData` for this session, if one has been issued. The
    /// player token is bound to this rather than to a video id.
    pub visitor_data: Option<String>,
}

/// A minted token plus how long it should be considered valid.
#[derive(Clone, Debug)]
pub struct PoToken {
    pub token: String,
    /// Server-suggested time-to-live in seconds, if known.
    pub ttl_secs: Option<u64>,
}

/// The swappable seam. A provider mints `PoTokens` for the live backend.
///
/// `Send + Sync` so it can live behind the backend on the worker thread.
/// Minting may be expensive (it can run a JS/BotGuard challenge), so callers
/// treat it as blocking and cache the result for the token's lifetime.
pub trait PoTokenProvider: Send + Sync {
    fn name(&self) -> &str;

    /// Mint a token of `kind` for `ctx`, or `None` if this provider cannot
    /// (e.g. the null provider, or a transient failure — the backend then
    /// resolves via the iOS/TV clients, which need no token).
    fn mint(&self, kind: PoTokenKind, ctx: &PoTokenContext) -> Option<PoToken>;

    /// Whether this provider can produce streaming tokens. When `false`, the
    /// backend keeps rustypipe's `BotGuard` path disabled (`no_botguard`) and
    /// relies on the iOS/TV clients for direct stream URLs.
    fn can_stream(&self) -> bool {
        false
    }
}

/// The default no-op provider: mints nothing. Audio resolution still works via
/// the iOS/TV player clients. Zero dependencies — the only provider the offline
/// build links.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullPoToken;

impl PoTokenProvider for NullPoToken {
    fn name(&self) -> &'static str {
        "null (iOS/TV clients, no token)"
    }

    fn mint(&self, _kind: PoTokenKind, _ctx: &PoTokenContext) -> Option<PoToken> {
        None
    }
}

/// The provider the current build uses by default.
pub fn default_provider() -> Box<dyn PoTokenProvider> {
    Box::new(NullPoToken)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one scenario test keeps each merged phase and its fresh fixture visible"
    )]
    fn default_null_provider_mints_nothing_and_cannot_stream() {
        // Phase `null_provider_mints_nothing_and_cannot_stream`.
        {
            let p = NullPoToken;
            let ctx = PoTokenContext {
                video_id: "abc".into(),
                visitor_data: None,
            };
            assert!(p.mint(PoTokenKind::Streaming, &ctx).is_none());
            assert!(!p.can_stream());
        }

        // Phase `default_provider_is_null`.
        {
            assert_eq!(default_provider().name(), "null (iOS/TV clients, no token)");
        }
    }
}
