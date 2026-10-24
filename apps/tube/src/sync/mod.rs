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
//! * [`transport`] (feature `grayjay-sync`) — the live [`GrayjaySyncTransport`](transport::GrayjaySyncTransport):
//!   a blocking TCP client that runs the real `Noise_IK_25519_ChaChaPoly_BLAKE2b`
//!   handshake (with a `Noise_N` pairing sub-handshake), the length-prefixed +
//!   STREAM-chunked framing, the authorize exchange, and the gzip'd-JSON
//!   subscription/history packages. Built on the [`snow`](https://docs.rs/snow)
//!   Noise crate.
//!
//! ## Product lifecycle
//!
//! The shipped build enables `grayjay-sync`.  The app derives its long-lived
//! Noise identity from a seed in its secret store, keeps only public peer
//! routes and revocation tombstones, as a setting of this device, requires
//! explicit fingerprint approval,
//! and runs bounded background sync.  Builds without that feature retain a
//! hermetic unavailable seam for tests and constrained downstream packages.
//!
//! ## Licensing
//!
//! Grayjay's app code is FUTO Source-First (non-commercial) and the `YouTube`
//! plugin is AGPL-3.0. This is our own Rust, reimplementing the *interoperable
//! wire facts* (opcode ints, pattern strings, framing) — not a port of their
//! source. See [`protocol`] for the per-fact citations.

pub mod protocol;

#[cfg(feature = "grayjay-sync")]
mod lifecycle;

#[cfg(feature = "grayjay-sync")]
pub mod transport;

#[cfg(feature = "grayjay-sync")]
pub use lifecycle::state_from_model as lifecycle_state;
#[cfg(feature = "grayjay-sync")]
pub use lifecycle::{DeviceSync, PairedDevices, PeerRecord, SyncStatus, SyncView};

#[cfg(not(feature = "grayjay-sync"))]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SyncStatus {
    #[default]
    Unavailable,
}

#[cfg(not(feature = "grayjay-sync"))]
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SyncView {
    pub status: SyncStatus,
    pub summary: String,
    pub device_public_key: Option<String>,
    pub peers: Vec<PeerRecord>,
}

#[cfg(not(feature = "grayjay-sync"))]
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeerRecord {
    pub id: String,
    pub name: String,
    pub pending_approval: bool,
    pub revoked_at: Option<i64>,
    pub last_sync_at: Option<i64>,
    pub last_error: Option<String>,
}

#[cfg(not(feature = "grayjay-sync"))]
impl PeerRecord {
    pub fn is_active(&self) -> bool {
        self.revoked_at.is_none()
    }
    pub fn short_id(&self) -> String {
        self.id.chars().take(10).collect()
    }
}

use crate::model::{Settings, VideoId};

/// Normalize public channel URL shapes emitted by YouTube/Grayjay without
/// accepting arbitrary web URLs into the durable subscription document.
///
/// The transport treats channel URLs as opaque package data. The app needs one
/// stable spelling, however, so `youtube.com`, `www.youtube.com`, and
/// `m.youtube.com` converge on the same action/resource identifier.
pub fn normalized_subscription_url(value: &str) -> Option<String> {
    const PREFIXES: [&str; 3] = [
        "https://youtube.com/",
        "https://www.youtube.com/",
        "https://m.youtube.com/",
    ];
    let value = value.trim();
    let path = PREFIXES
        .iter()
        .find_map(|prefix| value.strip_prefix(prefix))?;
    (!path.is_empty() && path.len() <= 232 && !path.chars().any(char::is_control))
        .then(|| format!("https://youtube.com/{path}"))
}

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

/// Errors a transport can surface. A build without the transport reports an
/// explicit unavailable capability; it never pretends a missing backend is a
/// temporary pairing or network failure.
#[derive(Clone, Debug, PartialEq)]
pub enum SyncError {
    /// This build intentionally omits the real Grayjay transport.
    Unavailable(String),
    /// Pairing was refused / timed out.
    Pairing(String),
    /// Transport-level failure (handshake, framing, relay).
    Transport(String),
    /// The host secret broker/keyring is absent or locked. Device sync fails
    /// closed; Tube never creates a plaintext private-key fallback.
    KeyUnavailable(String),
    /// Durable, device-local peer metadata could not be validated or written.
    Persistence(String),
    /// The peer has been locally revoked and cannot be resurrected by replay.
    Revoked,
}

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SyncError::Unavailable(message) => write!(f, "Grayjay sync unavailable: {message}"),
            SyncError::Pairing(m) => write!(f, "pairing failed: {m}"),
            SyncError::Transport(m) => write!(f, "transport error: {m}"),
            SyncError::KeyUnavailable(m) => write!(f, "device key unavailable: {m}"),
            SyncError::Persistence(m) => write!(f, "peer store error: {m}"),
            SyncError::Revoked => write!(f, "peer was revoked on this device"),
        }
    }
}

impl std::error::Error for SyncError {}

pub type SyncResult<T> = Result<T, SyncError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SyncCapability {
    pub available: bool,
    pub summary: &'static str,
}

/// Product-facing status for device sync. Availability still depends on the
/// host secret broker at runtime; [`DeviceSync::production`] fails closed when
/// that broker cannot provide app-scoped key material.
pub const fn capability() -> SyncCapability {
    #[cfg(feature = "grayjay-sync")]
    return SyncCapability {
        available: true,
        summary: "Encrypted Grayjay sync is available when the desktop's secret store is unlocked.",
    };
    #[cfg(not(feature = "grayjay-sync"))]
    SyncCapability {
        available: false,
        summary: "This build does not include Grayjay device sync.",
    }
}

/// Non-secret connection metadata needed to restore an authenticated peer.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PeerDescriptor {
    pub public_key: String,
    pub addresses: Vec<String>,
    pub port: i32,
}

/// The clean seam between Tube's state and *whatever* moves it to/from a
/// Grayjay peer. A real implementation speaks Noise over TCP 12315 (or via the
/// relay). The app holds a
/// `Box<dyn SyncTransport>` so the rest of Tube never depends on `snow`,
/// sockets, or the Grayjay wire format.
///
/// Designed so the *expensive* network work (pairing, handshake, framing) is
/// fully contained behind this trait, which is what lets it stay gated/stubbed
/// in the default build.
pub trait SyncTransport: Send {
    /// Begin pairing with a peer (e.g. by scanning its QR / entering a code).
    /// Returns the peer's stable [`PeerId`] on success.
    fn pair(&mut self, pairing_blob: &str) -> SyncResult<PeerId>;

    /// Push our state to a paired peer.
    fn push(&mut self, peer: &PeerId, state: &SyncState) -> SyncResult<()>;

    /// Pull the peer's state (merged into ours by the caller — Tube's CRDT
    /// handles the actual merge, the transport only carries bytes).
    ///
    /// # Errors
    ///
    /// Returns `Err` if the peer is unreachable or its state can't be fetched.
    fn pull(&mut self, peer: &PeerId) -> SyncResult<SyncState>;

    /// Restore public connection metadata after an app restart.
    ///
    /// # Errors
    ///
    /// Returns `Err` when the transport cannot restore the peer's route.
    fn restore_peer(&mut self, _peer: &PeerDescriptor) -> SyncResult<()> {
        Err(SyncError::Unavailable(
            "this sync transport cannot restore peer routes".into(),
        ))
    }

    /// Remove all in-memory routes for a revoked peer.
    fn forget_peer(&mut self, _peer: &PeerId) {}

    /// Export only public route data after a successful pair. Pairing codes and
    /// this device's private key are intentionally absent.
    fn peer_descriptor(&self, _peer: &PeerId) -> Option<PeerDescriptor> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subscription_urls_converge_without_accepting_other_sites() {
        assert_eq!(
            normalized_subscription_url("https://www.youtube.com/@tilo"),
            Some("https://youtube.com/@tilo".into())
        );
        assert_eq!(
            normalized_subscription_url("https://m.youtube.com/channel/UC123"),
            Some("https://youtube.com/channel/UC123".into())
        );
        assert_eq!(
            normalized_subscription_url("https://example.com/@tilo"),
            None
        );
        assert_eq!(
            normalized_subscription_url("https://youtube.com/\nunsafe"),
            None
        );
    }
}
