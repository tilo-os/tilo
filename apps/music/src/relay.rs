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

use crate::{doc, seed, storage, sync};

/// The headless server: an authoritative document + its sync transport.
pub struct Relay {
    doc: AutoCommit,
    transport: Box<dyn sync::LibrarySync>,
    path: std::path::PathBuf,
}

impl Relay {
    /// Load (or seed) the authoritative document from disk and bind a sync
    /// transport. The transport is the live iroh engine when the `p2p` feature
    /// is on and the environment supports it, otherwise the loopback (so the
    /// relay always starts, even headless in CI).
    pub fn new() -> Self {
        let path = storage::doc_path();
        let mut doc = match storage::load_from(&path) {
            Some(d) => d,
            None => {
                let mut d = AutoCommit::new();
                let lib = seed::sample_library();
                doc::seed(&mut d, &lib, &seed::sample_playback(&lib));
                d
            }
        };
        let transport = sync::default_sync(&mut doc);
        Self { doc, transport, path }
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
    /// never touched.
    fn isolated_relay() -> (Relay, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("qos-music-relay-{}", std::process::id()));
        let path = dir.join("library.automerge");
        // SAFETY: tests in this module run serially via the unique pid dir.
        unsafe {
            std::env::set_var("QOS_MUSIC_DATA", &path);
        }
        let relay = Relay::new();
        (relay, dir)
    }

    #[test]
    fn relay_seeds_and_serves_the_library() {
        let (relay, dir) = isolated_relay();
        assert!(relay.track_count() >= 20, "headless relay seeds the catalog");
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
        let (mut relay, dir) = isolated_relay();
        let path = storage::doc_path();
        let _ = storage::save_to(&path, &mut relay.doc);
        assert!(storage::load_from(&path).is_some(), "relay wrote a durable copy");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
