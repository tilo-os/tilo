//! Subscription / watch-state sync with Grayjay — the `SyncTransport` seam.
//!
//! ## What this is (and what it is NOT)
//!
//! The research brief is explicit and load-bearing: Grayjay does **not** sync
//! subscriptions / watch-state via Polycentric. It uses **Grayjay's own "Sync"
//! protocol**, which is the **Noise Protocol Framework** over P2P with a relay
//! fallback. To interoperate with a phone running Grayjay we would have to
//! reimplement *that*.
//!
//! That is deep reverse-engineering. Per the brief we **do not implement it
//! here** — instead this module defines a clean [`SyncTransport`] seam, ships a
//! documented [`plan`], and a [`StubTransport`] that compiles and is wired
//! nowhere live. Real Noise lives behind the `grayjay-sync` cargo feature and
//! is left unimplemented on purpose.
//!
//! ## The protocol, as researched (UNVERIFIED until read from source)
//!
//! Source of truth to port from before writing any wire code:
//! * `github.com/futo-org/grayjay-android`:
//!   - `states/StateSync.kt` — pairing, peer lifecycle, package dispatch
//!   - `sync/internal/SyncSocketSession.kt` — the framed Noise transport
//!   - `GJSyncOpcodes.kt` — the opcode integers (see [`plan::opcodes`])
//! * `Grayjay.Desktop` `StateSync.cs` — a second implementation to cross-check
//! * Reference relay: `github.com/JustOptimize/Grayjay-Sync`
//!
//! Transport shape (to confirm against the above):
//! * **Noise** handshake — pattern UNVERIFIED (read `SyncSocketSession.kt`);
//!   implement with the Rust [`snow`](https://docs.rs/snow) crate.
//! * Android **listens on TCP 12315**; pairing via QR / manual key exchange.
//! * A **relay** fallback forwards opaque Noise frames when no direct P2P route
//!   exists (the reference relay above).
//! * After the handshake, framed **packages** carry the actual state — see
//!   [`plan::packages`]: `SyncSubscriptionsPackage`,
//!   `SyncSubscriptionGroupsPackage`, `SyncPlaylistsPackage`, and history.
//!   JSON-vs-binary framing is UNVERIFIED.
//!
//! ## Licensing caution
//!
//! Grayjay's *app* code is FUTO Source-First (non-commercial — do **not**
//! copy). The YouTube *plugin* is AGPL-3.0. The Sync **wire format** is an
//! interop fact we may reimplement cleanly, but we must not paste their source.
//! Keep any future implementation a clean-room reimpl from the documented
//! opcodes/packages, not a port of their files.

pub mod plan;

use crate::model::{Settings, VideoId};

/// One direction of sync state: what we would send to / receive from a paired
/// Grayjay peer. These map onto the Grayjay "packages" (see [`plan::packages`])
/// but are expressed in Tube's own model so the transport is the only thing
/// that needs to speak Grayjay's wire format.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SyncState {
    /// Channel ids the user is subscribed to (`SyncSubscriptionsPackage`).
    pub subscriptions: Vec<String>,
    /// Watched video ids, newest-first (history package).
    pub watch_history: Vec<VideoId>,
    /// User settings worth mirroring (a subset — SponsorBlock/DeArrow prefs).
    pub settings: Option<Settings>,
}

/// A paired peer's stable identity (its Noise static public key, hex). Pairing
/// establishes this; the transport authenticates against it on reconnect.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PeerId(pub String);

/// Errors a transport can surface. Stubbed transports return
/// [`SyncError::NotImplemented`] so callers can degrade gracefully.
#[derive(Clone, Debug, PartialEq)]
pub enum SyncError {
    /// The seam is wired but no real transport is compiled in.
    NotImplemented,
    /// Pairing was refused / timed out.
    Pairing(String),
    /// Transport-level failure (handshake, framing, relay).
    Transport(String),
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::NotImplemented => write!(f, "Grayjay sync is not implemented in this build"),
            SyncError::Pairing(m) => write!(f, "pairing failed: {m}"),
            SyncError::Transport(m) => write!(f, "transport error: {m}"),
        }
    }
}

impl std::error::Error for SyncError {}

pub type SyncResult<T> = Result<T, SyncError>;

/// The clean seam between Tube's state and *whatever* moves it to/from a
/// Grayjay peer. A real implementation speaks Noise over TCP 12315 (or via the
/// relay); the [`StubTransport`] does nothing. The app holds a
/// `Box<dyn SyncTransport>` so the rest of Tube never depends on `snow`,
/// sockets, or the Grayjay wire format.
///
/// Designed so the *expensive* network work (pairing, handshake, framing) is
/// fully contained behind this trait — exactly the bounded surface the brief
/// asks to leave gated/stubbed.
pub trait SyncTransport: Send {
    fn name(&self) -> &str;

    /// Whether a real transport backs this seam. The settings UI shows the
    /// sync section as "coming soon" while this is false.
    fn is_available(&self) -> bool {
        false
    }

    /// Begin pairing with a peer (e.g. by scanning its QR / entering a code).
    /// Returns the peer's stable [`PeerId`] on success.
    fn pair(&mut self, pairing_blob: &str) -> SyncResult<PeerId>;

    /// Push our state to a paired peer.
    fn push(&mut self, peer: &PeerId, state: &SyncState) -> SyncResult<()>;

    /// Pull the peer's state (merged into ours by the caller — Tube's CRDT
    /// handles the actual merge, the transport only carries bytes).
    fn pull(&mut self, peer: &PeerId) -> SyncResult<SyncState>;
}

/// The only transport compiled by default: a stub that is wired nowhere and
/// reports everything as not-implemented. Lets the settings UI and any caller
/// reference the seam without pulling in `snow` / networking, and keeps the
/// shape honest (a real transport is a drop-in replacement).
#[derive(Clone, Copy, Debug, Default)]
pub struct StubTransport;

impl SyncTransport for StubTransport {
    fn name(&self) -> &str {
        "stub (Grayjay sync not implemented)"
    }

    fn pair(&mut self, _pairing_blob: &str) -> SyncResult<PeerId> {
        Err(SyncError::NotImplemented)
    }

    fn push(&mut self, _peer: &PeerId, _state: &SyncState) -> SyncResult<()> {
        Err(SyncError::NotImplemented)
    }

    fn pull(&mut self, _peer: &PeerId) -> SyncResult<SyncState> {
        Err(SyncError::NotImplemented)
    }
}

/// The default transport for this build. Always the stub unless a future
/// `grayjay-sync` feature swaps in the Noise implementation.
pub fn default_transport() -> Box<dyn SyncTransport> {
    Box::new(StubTransport)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_reports_unavailable_and_not_implemented() {
        let mut t = StubTransport;
        assert!(!t.is_available());
        assert_eq!(t.pair("blob"), Err(SyncError::NotImplemented));
        assert_eq!(
            t.push(&PeerId("k".into()), &SyncState::default()),
            Err(SyncError::NotImplemented)
        );
        assert_eq!(t.pull(&PeerId("k".into())), Err(SyncError::NotImplemented));
    }

    #[test]
    fn default_transport_is_the_stub() {
        let t = default_transport();
        assert!(!t.is_available());
    }
}
