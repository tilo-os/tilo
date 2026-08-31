//! Subscription / watch-state sync with Grayjay — the `SyncTransport` seam.
//!
//! ## What this is
//!
//! Grayjay does **not** sync subscriptions / watch-state via Polycentric. It
//! uses **Grayjay's own "Sync" protocol** = the **Noise Protocol Framework**
//! over TCP (Android listens on port 12315; QR/manual pairing; relay fallback).
//! This module implements a real client for that protocol.
//!
//! * [`protocol`] — the wire facts (Noise pattern strings, opcode integers,
//!   framing layout, JSON package models), each **cited to the exact
//!   grayjay-android source file** it was read from. Pure data + codec, always
//!   compiled, hermetically unit-tested.
//! * [`transport`] (feature `grayjay-sync`) — the live [`GrayjaySyncTransport`]:
//!   a blocking TCP client that runs the real `Noise_IK_25519_ChaChaPoly_BLAKE2b`
//!   handshake (with a `Noise_N` pairing sub-handshake), the length-prefixed +
//!   STREAM-chunked framing, the authorize exchange, and the gzip'd-JSON
//!   subscription/history packages. Built on the [`snow`](https://docs.rs/snow)
//!   Noise crate.
//!
//! ## Hermetic by construction
//!
//! The default build compiles only [`protocol`] and the [`StubTransport`]; the
//! socket/Noise/gzip stack is entirely behind `grayjay-sync`. [`default_transport`]
//! is always the stub, so nothing here can touch the `--shots`/test path or dial
//! the network unless the app *explicitly* constructs a [`GrayjaySyncTransport`].
//!
//! ## Licensing
//!
//! Grayjay's app code is FUTO Source-First (non-commercial) and the `YouTube`
//! plugin is AGPL-3.0. This is our own Rust, reimplementing the *interoperable
//! wire facts* (opcode ints, pattern strings, framing) — not a port of their
//! source. See [`protocol`] for the per-fact citations.

pub mod protocol;

#[cfg(feature = "grayjay-sync")]
pub mod transport;

#[cfg(feature = "grayjay-sync")]
pub use transport::{DeviceKey, GrayjaySession, GrayjaySyncTransport};

use crate::model::{Settings, VideoId};

/// One direction of sync state: what we would send to / receive from a paired
/// Grayjay peer. These map onto Grayjay's own sync "packages" but are
/// expressed in Tube's own model so the transport is the only thing that
/// needs to speak Grayjay's wire format.
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
/// fully contained behind this trait, which is what lets it stay gated/stubbed
/// in the default build.
pub trait SyncTransport: Send {
    fn name(&self) -> &str;

    /// Whether a real transport backs this seam. The settings UI shows the
    /// sync section as "coming soon" while this is false.
    fn is_available(&self) -> bool {
        false
    }

    /// Begin pairing with a peer (e.g. by scanning its QR / entering a code).
    /// Returns the peer's stable [`PeerId`] on success.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the pairing blob is invalid or the handshake fails.
    fn pair(&mut self, pairing_blob: &str) -> SyncResult<PeerId>;

    /// Push our state to a paired peer.
    ///
    /// # Errors
    ///
    /// Returns `Err` if the peer is unreachable or rejects the push.
    fn push(&mut self, peer: &PeerId, state: &SyncState) -> SyncResult<()>;

    /// Pull the peer's state (merged into ours by the caller — Tube's CRDT
    /// handles the actual merge, the transport only carries bytes).
    ///
    /// # Errors
    ///
    /// Returns `Err` if the peer is unreachable or its state can't be fetched.
    fn pull(&mut self, peer: &PeerId) -> SyncResult<SyncState>;
}

/// The only transport compiled by default: a stub that is wired nowhere and
/// reports everything as not-implemented. Lets the settings UI and any caller
/// reference the seam without pulling in `snow` / networking, and keeps the
/// shape honest (a real transport is a drop-in replacement).
#[derive(Clone, Copy, Debug, Default)]
pub struct StubTransport;

impl SyncTransport for StubTransport {
    fn name(&self) -> &'static str {
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
