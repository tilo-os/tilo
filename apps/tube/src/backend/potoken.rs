//! `PoToken` — the swappable "proof-of-origin token" module.
//!
//! `YouTube` gates video *streams* (and increasingly the player response itself)
//! behind a **`PoToken`** minted by its `BotGuard` challenge, and behind SABR
//! streaming — a *swappable module* so the rest of the live backend never
//! hard-codes how a token is obtained.
//!
//! This module is that seam. A [`PoTokenProvider`] mints tokens; the live
//! rustypipe backend asks the configured provider for one when it resolves a
//! player / stream URL. Swapping the provider (`BotGuard` today, a remote token
//! service, a cached token, or [`NullPoToken`] for metadata-only browsing)
//! never touches the backend.
//!
//! ## Providers
//!
//! * [`NullPoToken`] — the default. Mints nothing. Search / channel / trending
//!   / related / video *metadata* all work without a `PoToken`; only stream URL
//!   resolution needs one, so a metadata-only client is fully useful with this.
//!   This is also the only provider compiled into the offline / `--shots`
//!   build (it has zero deps).
//! * `BotGuardPoToken` (`live` feature) — wraps `rustypipe-botguard`, which
//!   solves the `BotGuard` challenge in-process (no external browser, no
//!   yt-dlp/ffmpeg). Lives in [`crate::backend::live`] so the heavy dep only
//!   compiles under `--features live`.
//!
//! ## Why a context, not just a string
//!
//! `BotGuard` binds a token to a *visitor/identifier context*; [`PoTokenContext`]
//! carries that so a provider can mint the right token kind (a "player" token
//! vs a "streaming" token are different). Today only the video id and a
//! visitor-data string are needed; the struct leaves room to grow without
//! changing the trait.

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
    /// Video being resolved (the identifier `BotGuard` binds the token to for a
    /// streaming token).
    pub video_id: String,
    /// `InnerTube` `visitorData` for this session, if one has been issued. The
    /// player token is bound to this rather than to a video id.
    pub visitor_data: Option<String>,
}

/// A minted token plus how long it should be considered valid. Providers may
/// cache and reuse a token within its lifetime.
#[derive(Clone, Debug)]
pub struct PoToken {
    pub token: String,
    /// Server-suggested time-to-live in seconds, if known. `None` = treat as
    /// single-use / unknown.
    pub ttl_secs: Option<u64>,
}

/// The swappable seam. A provider mints `PoTokens` for the live backend.
///
/// `Send + Sync` so it can live behind the backend on the worker thread. Minting
/// may be expensive (it can run a JS/BotGuard challenge), so callers should
/// treat it as blocking and cache the result for the token's lifetime.
pub trait PoTokenProvider: Send + Sync {
    fn name(&self) -> &str;

    /// Mint a token of `kind` for `ctx`, or `None` if this provider cannot
    /// (e.g. the null provider, or a transient failure — the backend then
    /// degrades to metadata-only for that request).
    fn mint(&self, kind: PoTokenKind, ctx: &PoTokenContext) -> Option<PoToken>;

    /// Whether this provider can actually produce streaming tokens. The UI can
    /// surface "playback may be unavailable" when this is false.
    fn can_stream(&self) -> bool {
        false
    }
}

/// The default no-op provider: mints nothing. Metadata browsing works; stream
/// resolution returns "no token" and the backend falls back to poster-only.
/// Zero dependencies — this is what the offline build links.
#[derive(Clone, Copy, Debug, Default)]
pub struct NullPoToken;

impl PoTokenProvider for NullPoToken {
    fn name(&self) -> &'static str {
        "null (metadata-only)"
    }

    fn mint(&self, _kind: PoTokenKind, _ctx: &PoTokenContext) -> Option<PoToken> {
        None
    }
}

/// The provider the current build uses by default. Offline builds get
/// [`NullPoToken`]; the live build overrides this in
/// [`crate::backend::live`] when constructing the rustypipe backend.
pub fn default_provider() -> Box<dyn PoTokenProvider> {
    Box::new(NullPoToken)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offline_build_uses_the_null_potoken_provider() {
        // Case `null_provider_mints_nothing_and_cannot_stream`.
        {
            let p = NullPoToken;
            let ctx = PoTokenContext {
                video_id: "abc".into(),
                visitor_data: None,
            };
            assert!(p.mint(PoTokenKind::Streaming, &ctx).is_none());
            assert!(!p.can_stream());
        }
        // Case `default_provider_is_null_in_offline_build`.
        {
            // In the default (non-`live`) build the swappable default is the null
            // provider, keeping the hermetic path dependency-free.
            let p = default_provider();
            assert_eq!(p.name(), "null (metadata-only)");
        }
    }
}
