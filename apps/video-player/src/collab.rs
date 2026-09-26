//! Collaborative playback over the shared [`media`] transport.
//!
//! One peer is the **host**; every other is a **follower**. The host folds its
//! engine's playback state into a [`media::CrdtTransport`] and broadcasts the
//! change; followers merge it and bring their engine in line with drift
//! correction ([`media::MediaEngine::apply_transport`]). The wire is the same
//! `ui::collaboration` collab socket every Tilo app uses — the CRDT bytes ride
//! `CollabRequest::document_incremental`, so a peer that joins the workspace simply
//! starts following.
//!
//! This module holds only the CRDT-drive logic (`host_step` / ingest /
//! `follow_step`), so it is fully unit-testable without a live socket; the app
//! ([`crate::app`]) owns the `CollaborationManager` and pumps bytes between it
//! and this state.

use media::{CrdtTransport, MediaEngine, TransportState, TransportSync};

/// Which side of a collaborative session this peer is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Authoritative: publishes transport, everyone follows.
    Host,
    /// Follows the host's published transport.
    Follower,
}

impl Role {
    /// Resolve the role from `$TILO_PLAYER_ROLE` (`follower` → Follower, else
    /// Host), so two instances on a shared workspace pick complementary roles.
    pub fn from_env() -> Self {
        match std::env::var("TILO_PLAYER_ROLE").ok().as_deref() {
            Some("follower" | "Follower") => Role::Follower,
            _ => Role::Host,
        }
    }
}

/// Only re-broadcast once the host's playhead has drifted this far from what a
/// follower would already project — throttles the host to a change/seek plus a
/// gentle heartbeat while it accumulates.
const REPUBLISH_DRIFT_MS: u64 = 200;

/// The CRDT-drive half of collaborative playback. Owns the transport document
/// and the host/follower policy; carries no socket of its own.
pub struct CollabPlayback {
    transport: CrdtTransport,
    role: Role,
    last_published: Option<TransportState>,
}

impl CollabPlayback {
    pub fn new(role: Role) -> Self {
        Self {
            transport: CrdtTransport::new(),
            role,
            last_published: None,
        }
    }

    pub fn role(&self) -> Role {
        self.role
    }

    /// Host side: fold the engine's current transport into the CRDT and, if it
    /// materially changed, return the incremental blob to broadcast. `None`
    /// when there is nothing new to send (or this peer is a follower).
    pub fn host_step(&mut self, engine: &MediaEngine, now_ms: u64) -> Option<Vec<u8>> {
        if self.role != Role::Host {
            return None;
        }
        let state = engine.transport_state(now_ms);
        if !self.should_publish(&state, now_ms) {
            return None;
        }
        self.transport.publish(&state);
        self.last_published = Some(state);
        Some(self.transport.save_incremental())
    }

    /// Merge an incremental transport blob received from the host.
    pub fn ingest(&mut self, bytes: &[u8]) {
        if let Err(e) = self.transport.merge_incremental(bytes) {
            tracing::debug!("collab transport: bad incremental blob dropped ({e})");
        }
    }

    /// Merge a full transport snapshot (legacy / late-join handoff).
    pub fn ingest_snapshot(&mut self, bytes: &[u8]) {
        if let Err(e) = self.transport.merge_snapshot(bytes) {
            tracing::debug!("collab transport: bad snapshot dropped ({e})");
        }
    }

    /// Follower side: bring `engine` in line with the latest host transport.
    /// Returns `true` if a corrective seek was issued. A no-op for the host.
    pub fn follow_step(&mut self, engine: &mut MediaEngine, now_ms: u64) -> bool {
        if self.role != Role::Follower {
            return false;
        }
        match self.transport.latest() {
            Some(state) => engine.apply_transport(&state, now_ms),
            None => false,
        }
    }

    /// The latest transport the document holds, if any (for diagnostics/tests).
    pub fn latest(&self) -> Option<TransportState> {
        self.transport.latest()
    }

    fn should_publish(&self, state: &TransportState, now_ms: u64) -> bool {
        match self.last_published {
            None => true,
            Some(prev) => {
                // Publish on a play/pause flip, or once the true playhead has
                // drifted from what a follower would already project.
                prev.playing != state.playing
                    || (prev.rate - state.rate).abs() > f32::EPSILON
                    || prev.projected_position(now_ms).abs_diff(state.position_ms)
                        > REPUBLISH_DRIFT_MS
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use media::Source;

    fn engine_at(pos_ms: u64, playing: bool) -> MediaEngine {
        let mut e = MediaEngine::new_synthesized();
        e.open(Source::Remote {
            url: "https://x/v.mp4".into(),
        });
        e.seek_ms(pos_ms);
        if playing {
            e.play();
        }
        e
    }

    #[test]
    fn host_publishes_changes_and_follower_applies_without_publishing() {
        // Case `host_publishes_then_follower_follows`.
        {
            let mut host = CollabPlayback::new(Role::Host);
            let mut follower = CollabPlayback::new(Role::Follower);

            // Host is playing at 30s; broadcast that.
            let host_engine = engine_at(30_000, true);
            let delta = host
                .host_step(&host_engine, 1_000)
                .expect("first host step publishes");
            follower.ingest(&delta);

            // A fresh follower engine at 0 must hard-seek to catch the host.
            let mut follower_engine = engine_at(0, false);
            let seeked = follower.follow_step(&mut follower_engine, 1_000);
            assert!(seeked, "large drift → follower seeks to the host");
            assert!(follower_engine.position_ms() >= 30_000);
            assert!(follower_engine.is_playing(), "follower adopts play state");
        }
        // Case `host_does_not_republish_an_unchanged_state`.
        {
            let mut host = CollabPlayback::new(Role::Host);
            let engine = engine_at(5_000, false); // paused, static
            assert!(
                host.host_step(&engine, 0).is_some(),
                "first publish always fires"
            );
            assert!(
                host.host_step(&engine, 10).is_none(),
                "no change → nothing to send"
            );
        }
        // Case `host_republishes_on_play_pause_flip`.
        {
            let mut host = CollabPlayback::new(Role::Host);
            let paused = engine_at(5_000, false);
            host.host_step(&paused, 0);
            let playing = engine_at(5_000, true);
            assert!(
                host.host_step(&playing, 0).is_some(),
                "play/pause flip re-broadcasts"
            );
        }
        // Case `follower_never_publishes`.
        {
            let mut follower = CollabPlayback::new(Role::Follower);
            let engine = engine_at(1_000, true);
            assert!(
                follower.host_step(&engine, 0).is_none(),
                "a follower never broadcasts"
            );
        }
        // Case `host_rate_change_is_replicated_and_applied_by_follower`.
        {
            let mut host = CollabPlayback::new(Role::Host);
            let mut follower = CollabPlayback::new(Role::Follower);
            let mut host_engine = engine_at(4_000, false);
            let initial = host.host_step(&host_engine, 100).expect("initial publish");
            follower.ingest(&initial);
            host_engine.set_rate(2.0);
            let delta = host
                .host_step(&host_engine, 100)
                .expect("rate change publishes immediately");
            follower.ingest(&delta);
            let mut follower_engine = engine_at(4_000, false);
            follower.follow_step(&mut follower_engine, 100);
            assert!((follower_engine.rate() - 2.0).abs() < f32::EPSILON);
        }
    }
}
