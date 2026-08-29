//! Headless relay mode — a music library server with no GUI.
//!
//! The qOS self-hosting model: a **home server** runs this same app headless
//! (`music --relay`) holding the authoritative library replica, and the user's
//! GUI clients (phone, laptop) sync to it. No window, no compositor — just the
//! Automerge document, on-disk persistence ([`crate::storage`]), and the sync
//! transport seam ([`crate::sync`]).
//!
//! Live P2P transport is gated behind the `p2p` cargo feature (so the headless
//! binary still builds + runs in CI without iroh/Tokio); with the feature off
//! the relay binds the loopback transport and logs its ticket, demonstrating
//! the full seam without a network. This is the documented gate.

use automerge::AutoCommit;

use crate::model::{Library, Playback};
use crate::{doc, storage, sync};

/// The headless server: an authoritative document + its sync transport.
pub struct Relay {
    doc: AutoCommit,
    transport: Box<dyn sync::LibrarySync>,
    path: std::path::PathBuf,
}

impl Relay {
    /// Load (or initialise) the authoritative document from disk and bind a
    /// sync transport. The transport is the live iroh engine when the `p2p`
    /// feature is on and the environment supports it, otherwise the loopback
    /// (so the relay always starts, even headless in CI).
    ///
    /// A brand-new server starts with an **empty** library — it never
    /// fabricates a catalog. Real tracks arrive when clients sync their library
    /// up to it.
    pub fn new() -> Self {
        let path = storage::doc_path();
        let mut doc = if let Some(d) = storage::load_from(&path) {
            d
        } else {
            let mut d = AutoCommit::new();
            doc::seed(&mut d, &Library::default(), &Playback::default());
            d
        };
        let transport = sync::default_sync(&mut doc);
        Self {
            doc,
            transport,
            path,
        }
    }

    /// The dialable ticket clients connect to (loopback or live).
    pub fn ticket(&self) -> Option<String> {
        self.transport.local_ticket()
    }

    /// Whether the bound transport is the live (network) engine.
    pub fn is_live(&self) -> bool {
        self.transport.is_live()
    }

    /// Number of tracks in the served library (for status logging / tests).
    pub fn track_count(&self) -> usize {
        doc::load_library(&self.doc).total_tracks()
    }

    /// Run one service tick: sync against connected peers, then persist the
    /// authoritative copy. Returns whether the document changed.
    pub fn tick(&mut self) -> bool {
        let changed = self.transport.sync(&mut self.doc);
        if changed {
            let _ = storage::save_to(&self.path, &mut self.doc);
        }
        changed
    }

    /// Run the relay loop until interrupted. Headless: no event loop, no GUI.
    /// Persists on start so a freshly-seeded server has a durable copy.
    pub fn run(&mut self) {
        let _ = storage::save_to(&self.path, &mut self.doc);
        tracing::info!(
            tracks = self.track_count(),
            live = self.is_live(),
            ticket = self.ticket().as_deref().unwrap_or("<none>"),
            "music relay started (headless); clients sync the library document"
        );
        loop {
            self.tick();
            std::thread::sleep(std::time::Duration::from_millis(500));
        }
    }
}

impl Default for Relay {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Run the relay against an isolated data dir so the real user library is
    /// never touched. Each call gets a **unique** dir (process id + a global
    /// counter) so tests running in parallel never share a path or race on
    /// removing each other's dir. Returns the relay, its dir, and the explicit
    /// library path (so callers don't re-read the process-global
    /// `QOS_MUSIC_DATA`, which other parallel tests are also mutating).
    fn isolated_relay() -> (Relay, std::path::PathBuf, std::path::PathBuf) {
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("qos-music-relay-{}-{}", std::process::id(), n));
        let path = dir.join("library.automerge");
        unsafe {
            std::env::set_var("QOS_MUSIC_DATA", &path);
        }
        let relay = Relay::new();
        (relay, dir, path)
    }

    #[test]
    fn relay_starts_empty_and_serves() {
        let (relay, dir, _path) = isolated_relay();
        // A fresh headless server fabricates no catalog — real tracks arrive
        // when clients sync their library up to it.
        assert_eq!(
            relay.track_count(),
            0,
            "a fresh relay serves an empty library"
        );
        // Without the p2p feature the transport is the loopback (not live) but
        // still advertises a ticket — the seam is wired.
        assert!(relay.ticket().is_some());
        if !sync::is_p2p_available() {
            assert!(!relay.is_live());
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn relay_persists_on_run_start() {
        let (mut relay, dir, path) = isolated_relay();
        // Use the test's own explicit path — never the process-global
        // `QOS_MUSIC_DATA` that a concurrently-running test may have re-pointed.
        let _ = storage::save_to(&path, &mut relay.doc);
        assert!(
            storage::load_from(&path).is_some(),
            "relay wrote a durable copy"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
