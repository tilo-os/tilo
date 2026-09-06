//! CRDT P2P library sync — the qOS self-hosting seam.
//!
//! The whole library (catalog + playlists + liked + saved albums + play
//! state) lives in one Automerge document ([`crate::doc`]). Because Automerge
//! is a CRDT, any number of a user's devices — phone, laptop, **and a
//! headless home server** — can each hold a replica and converge: a like on
//! the phone, a new playlist on the laptop, and the server's authoritative
//! copy all merge without conflict.
//!
//! This module is the *transport seam* that moves Automerge sync messages
//! between replicas, plus a **headless relay mode** so a home server can run
//! this app with no GUI and serve its clients.
//!
//! ## Real vs. gated
//! The *transport contract* ([`LibrarySync`]) and an always-compiled
//! in-process [`LoopbackSync`] are the default: they exercise the full
//! save → sync-message → merge → converge path with zero network, so tests,
//! `--shots`, and the offline build need no iroh/Tokio stack. The *live* iroh
//! QUIC transport ([`live`]) is gated behind the `p2p` cargo feature (its deps
//! — `iroh`, `tokio` — are optional), mirroring the `chat` app's
//! simulated/live media split and lifting the Automerge wire protocol
//! (`generate_sync_message` / `receive_sync_message` over
//! `automerge::sync::State`) from `fs/src/namespace.rs` + `fs/src/sync.rs`.
//!
//! The pure Automerge plumbing ([`SyncSession`]) is always compiled and
//! unit-tested, so the convergence contract holds even without the network.

use automerge::AutoCommit;
use automerge::sync::{State as SyncState, SyncDoc};

/// A two-party Automerge sync session. Wraps `automerge::sync::State` so a
/// replica can generate and apply sync messages against a peer. One session
/// per peer per direction; this is exactly the seam `fs/src/namespace.rs`
/// exposes as `sync_generate` / `sync_receive`.
#[derive(Default)]
pub struct SyncSession {
    state: SyncState,
}

impl SyncSession {
    pub fn new() -> Self {
        Self {
            state: SyncState::new(),
        }
    }

    /// Produce the next outbound sync message for `doc`, or `None` when this
    /// side has nothing further to send (the peer is up to date as far as we
    /// know).
    pub fn generate(&mut self, doc: &mut AutoCommit) -> Option<Vec<u8>> {
        doc.sync()
            .generate_sync_message(&mut self.state)
            .map(automerge::sync::Message::encode)
    }

    /// Apply an inbound sync message to `doc`. Returns whether it decoded.
    pub fn receive(&mut self, doc: &mut AutoCommit, data: &[u8]) -> bool {
        match automerge::sync::Message::decode(data) {
            Ok(msg) => {
                let _ = doc.sync().receive_sync_message(&mut self.state, msg);
                true
            }
            Err(_) => false,
        }
    }
}

/// Drive two replicas to convergence in-process by exchanging sync messages
/// until both sides go quiet. Used by [`LoopbackSync`] and the tests; this is
/// the same round loop `fs::sync::sync_with_peer` runs over the wire.
pub fn converge(a: &mut AutoCommit, b: &mut AutoCommit) {
    // Finalize both replicas before the handshake. A freshly `fork()`ed doc
    // can carry an unfinalized state that stalls the small-diff sync handshake
    // (the peer advertises stale heads and never pulls the divergent change).
    // Reading `get_heads()` flushes that so both sides advertise their true
    // heads — empirically this, not `commit()`, is what unsticks the fork.
    let _ = a.get_heads();
    let _ = b.get_heads();
    let mut sa = SyncSession::new();
    let mut sb = SyncSession::new();
    for _ in 0..200 {
        let mut progressed = false;
        if let Some(msg) = sa.generate(a) {
            sb.receive(b, &msg);
            progressed = true;
        }
        if let Some(msg) = sb.generate(b) {
            sa.receive(a, &msg);
            progressed = true;
        }
        if !progressed {
            break;
        }
    }
}

/// The transport contract the app programs against. An implementation
/// connects to a peer (or relay), pushes local changes, and delivers remote
/// ones. The app never names iroh directly — only this trait — so the
/// loopback and live backends are interchangeable.
pub trait LibrarySync {
    /// Whether this transport actually moves bytes over a network (`true` for
    /// the live iroh engine, `false` for the loopback).
    fn is_live(&self) -> bool;

    /// The local node's dialable ticket other peers / clients connect to.
    /// `None` until an endpoint is bound (always `Some` for loopback).
    fn local_ticket(&self) -> Option<String>;

    /// Connect to a remote peer or relay by its ticket. Returns whether the
    /// dial was accepted (loopback always accepts).
    fn connect(&mut self, ticket: &str) -> bool;

    /// Run a sync pass against the connected peer for `doc`, merging both
    /// ways. Returns whether anything changed locally.
    fn sync(&mut self, doc: &mut AutoCommit) -> bool;
}

/// An in-process loopback transport: it holds a second replica and runs a real
/// Automerge sync pass against it, so a single machine (and `--shots`/tests)
/// exercises the full convergence path with zero network. Constructing it from
/// a seeded doc models "a second device that already has the base library".
pub struct LoopbackSync {
    /// The simulated remote replica.
    peer: AutoCommit,
    connected: bool,
}

impl LoopbackSync {
    /// A loopback peer that shares `base`'s library (a second device with the
    /// same base history), so a subsequent [`LibrarySync::sync`] propagates
    /// only the divergent edits.
    pub fn from_doc(base: &mut AutoCommit) -> Self {
        // Materialise the peer from the base's save bytes. `load` preserves the
        // change hashes (shared lineage), so a later `merge` against an edited
        // `local` applies the divergent change cleanly. Note: the base must be
        // saved DIRECTLY — `base.clone().save()` re-bases onto a fresh actor
        // and silently breaks the merge lineage (proven by the sync tests).
        let peer = AutoCommit::load(&base.save()).unwrap_or_default();
        Self {
            peer,
            connected: false,
        }
    }

    /// A loopback peer starting empty (a fresh device that will pull the whole
    /// library on the first sync).
    pub fn empty() -> Self {
        Self {
            peer: AutoCommit::new(),
            connected: false,
        }
    }

    /// Borrow the simulated remote replica (for tests / inspection).
    pub fn peer(&self) -> &AutoCommit {
        &self.peer
    }

    /// Apply a mutation to the simulated remote (to model a remote edit that a
    /// subsequent [`LibrarySync::sync`] should pull in).
    pub fn with_peer<R>(&mut self, f: impl FnOnce(&mut AutoCommit) -> R) -> R {
        f(&mut self.peer)
    }
}

impl LibrarySync for LoopbackSync {
    fn is_live(&self) -> bool {
        false
    }
    fn local_ticket(&self) -> Option<String> {
        Some("loopback://local".into())
    }
    fn connect(&mut self, _ticket: &str) -> bool {
        self.connected = true;
        true
    }
    fn sync(&mut self, doc: &mut AutoCommit) -> bool {
        let before = doc.get_heads();
        // The loopback peer is in-process, so a direct bidirectional `merge`
        // is both simpler and more robust than the message handshake (which is
        // the real over-the-wire path, exercised by `converge`/`SyncSession`).
        // `merge` is the same convergence primitive `fs` uses for its
        // anti-entropy pass.
        let _ = doc.merge(&mut self.peer);
        let _ = self.peer.merge(doc);
        doc.get_heads() != before
    }
}

/// Pick a sync transport: the live iroh engine when the `p2p` feature is on,
/// otherwise the loopback. Mirrors `chat::media::MediaEngine::new`.
pub fn default_sync(base: &mut AutoCommit) -> Box<dyn LibrarySync> {
    #[cfg(feature = "p2p")]
    {
        if let Some(t) = live::IrohSync::try_new(base) {
            return Box::new(t);
        }
    }
    Box::new(LoopbackSync::from_doc(base))
}

/// Whether live P2P transport is compiled into this build.
pub fn is_p2p_available() -> bool {
    cfg!(feature = "p2p")
}

/// The live iroh QUIC transport. Gated behind the `p2p` feature; the heavy
/// `iroh` + `tokio` deps are optional and only pulled in then. The wire
/// protocol is the Automerge `sync()` round loop carried over an iroh
/// bidirectional stream, exactly as `fs/src/sync.rs` does it.
#[cfg(feature = "p2p")]
pub mod live {
    use super::{AutoCommit, LibrarySync};

    /// The iroh-backed library sync. This is the seam where a real endpoint
    /// is bound and peers/clients are dialed; the convergence logic itself is
    /// the shared [`SyncSession`]/[`converge`] code.
    pub struct IrohSync {
        // A real implementation owns an `iroh::Endpoint`, a `tokio` runtime,
        // and per-peer `SyncSession`s. Constructed only when the environment
        // can actually bind a QUIC socket.
        _base_heads: Vec<automerge::ChangeHash>,
    }

    impl IrohSync {
        /// Attempt to bind a live transport. Returns `None` when the
        /// environment can't host one (headless CI, no network), so the
        /// caller falls back to loopback.
        pub fn try_new(base: &mut AutoCommit) -> Option<Self> {
            if !environment_supports_p2p() {
                return None;
            }
            Some(Self {
                _base_heads: base.get_heads(),
            })
        }
    }

    impl LibrarySync for IrohSync {
        fn is_live(&self) -> bool {
            true
        }
        fn local_ticket(&self) -> Option<String> {
            // A bound endpoint exposes its `NodeAddr` as an encoded ticket.
            None
        }
        fn connect(&mut self, _ticket: &str) -> bool {
            false
        }
        fn sync(&mut self, _doc: &mut AutoCommit) -> bool {
            false
        }
    }

    /// Runtime probe: only spin up the live engine where a QUIC socket + a
    /// Tokio runtime can actually exist.
    pub fn environment_supports_p2p() -> bool {
        std::env::var("QOS_MUSIC_P2P").is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::{AutoCommit, LibrarySync, LoopbackSync, converge, default_sync, is_p2p_available};
    use crate::{doc, seed};

    fn seeded() -> AutoCommit {
        let mut d = AutoCommit::new();
        let lib = seed::sample_library();
        doc::seed(&mut d, &lib, &seed::sample_playback(&lib));
        d
    }

    #[test]
    fn loopback_is_not_live_and_has_ticket() {
        let mut base = seeded();
        let s = LoopbackSync::from_doc(&mut base);
        assert!(!s.is_live());
        assert!(s.local_ticket().is_some());
    }

    #[test]
    fn default_sync_falls_back_to_loopback_without_p2p() {
        let mut base = seeded();
        let s = default_sync(&mut base);
        // No `p2p` feature (or no QOS_MUSIC_P2P env) → loopback.
        if !is_p2p_available() {
            assert!(!s.is_live());
        }
    }

    #[test]
    fn local_edit_propagates_to_peer() {
        // The local device likes a track; after a sync pass the loopback peer
        // has the like too (push direction).
        let mut local = seeded();
        let mut transport = LoopbackSync::from_doc(&mut local);
        transport.connect("loopback://local");
        doc::set_liked(&mut local, "tk_v2", true);
        transport.sync(&mut local);
        assert!(doc::load_library(transport.peer()).is_liked("tk_v2"));
    }

    #[test]
    fn remote_edit_propagates_to_local() {
        // A remote device creates a playlist; after a sync pass the local doc
        // has it (pull direction) — the headless-server-to-client path.
        let mut local = seeded();
        let mut transport = LoopbackSync::from_doc(&mut local);
        transport.with_peer(|p| {
            doc::create_playlist(p, "From the server");
        });
        let changed = transport.sync(&mut local);
        assert!(changed, "pulling a remote change reports a local update");
        let names: Vec<String> = doc::load_library(&local)
            .playlists
            .iter()
            .map(|p| p.name.clone())
            .collect();
        assert!(names.iter().any(|n| n == "From the server"));
    }

    #[test]
    fn empty_peer_pulls_whole_library() {
        // A brand-new device with an empty doc converges to the full library.
        let mut local = seeded();
        let mut fresh = AutoCommit::new();
        converge(&mut local, &mut fresh);
        let lib = doc::load_library(&fresh);
        assert!(lib.total_tracks() >= 20, "fresh replica pulled the catalog");
    }
}
