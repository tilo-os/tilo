//! Collaborative playback transport — follow-the-host with drift correction.
//!
//! [`TransportState`] is a small, copyable snapshot of "where playback is": the
//! play/pause flag, the authoritative playhead at a wall-clock `epoch`, and the
//! rate. A **host** publishes it; **followers** read it and project it forward
//! to *now*, then nudge their local engine toward that projection.
//!
//! The projection ([`TransportState::projected_position`]) plus the drift
//! decision ([`correction`]) are pure functions of `(state, now_ms)` and carry
//! all the collaborative-playback timing logic, so they are unit-tested without
//! any decode, socket, or clock.
//!
//! Wiring the state onto an Automerge document is kept behind the [`TransportSync`] trait
//! so a purely local player never pays for automerge. [`CrdtTransport`] is the
//! automerge-backed implementation; [`LocalTransport`] is the no-op one.

use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc, ScalarValue, Value};
use tilo_automerge::ApplyIncoming;

/// Playhead drift (ms) beyond which a follower hard-seeks to the host instead
/// of letting the small gap ride. Below it the follower stays in sync by simply
/// playing on — a seek every frame would stutter worse than the sub-half-second
/// gap it would correct.
pub const DRIFT_SEEK_THRESHOLD_MS: u64 = 500;

/// A snapshot of collaborative playback state. `epoch_ms` is a wall-clock
/// timestamp in unix millis (`tilo_clock::unix_now_ms`), not a monotonic one,
/// so followers on other machines can project the playhead forward to their
/// own "now".
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TransportState {
    /// Whether the host is playing (vs. paused).
    pub playing: bool,
    /// The authoritative playhead, in ms, **as of `epoch_ms`**.
    pub position_ms: u64,
    /// Playback rate (1.0 = normal). Reserved for future speed controls; the
    /// projection already honours it.
    pub rate: f32,
    /// Wall-clock time (unix millis) at which `position_ms` was authoritative.
    pub epoch_ms: u64,
}

impl TransportState {
    /// Where the playhead should be **now** for a follower, given `now_ms` in
    /// the same wall-clock frame as `epoch_ms`. While paused the position is
    /// frozen; while playing it advances by `(now - epoch) * rate`.
    pub fn projected_position(&self, now_ms: u64) -> u64 {
        if !self.playing {
            return self.position_ms;
        }
        let elapsed = now_ms.saturating_sub(self.epoch_ms);
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss,
            reason = "elapsed and the projected advance are millisecond counts \
                      for a media playhead — far inside u64/f64's exact-integer \
                      range for any real session — and rate.max(0.0) keeps the \
                      product non-negative"
        )]
        let advance = (elapsed as f64 * f64::from(self.rate.max(0.0))) as u64;
        self.position_ms.saturating_add(advance)
    }
}

/// The drift decision for a follower whose engine is at `local_ms` when the
/// host's projected playhead is at `target_ms`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Correction {
    /// Within tolerance — let playback ride, no seek.
    InSync,
    /// Off by more than [`DRIFT_SEEK_THRESHOLD_MS`] — seek to this ms.
    Seek(u64),
}

/// Decide whether a follower at `local_ms` needs to hard-seek to catch the
/// host's projected `target_ms`. Pure; the whole drift policy lives here.
pub fn correction(local_ms: u64, target_ms: u64) -> Correction {
    let drift = local_ms.abs_diff(target_ms);
    if drift > DRIFT_SEEK_THRESHOLD_MS {
        Correction::Seek(target_ms)
    } else {
        Correction::InSync
    }
}

/// The seam between playback and a shared document. A local-only player uses
/// [`LocalTransport`] (no CRDT); a collaborative one uses [`CrdtTransport`].
pub trait TransportSync {
    /// Host side: write the current transport into the shared state.
    fn publish(&mut self, state: &TransportState);
    /// Follower side: read the latest transport, if any has been published.
    fn latest(&self) -> Option<TransportState>;
}

/// The no-op sync for a purely local player. Publishing is dropped and there is
/// never a remote state to follow, so a local player carries zero CRDT weight.
#[derive(Default)]
pub struct LocalTransport;

impl TransportSync for LocalTransport {
    fn publish(&mut self, _state: &TransportState) {}
    fn latest(&self) -> Option<TransportState> {
        None
    }
}

// Root-level keys the transport state occupies in the automerge document.
const K_PRESENT: &str = "present";
const K_PLAYING: &str = "playing";
const K_POSITION: &str = "position_ms";
const K_RATE: &str = "rate";
const K_EPOCH: &str = "epoch_ms";

/// The automerge-backed transport. Its whole document is the four transport
/// scalars, so the sync payload stays tiny — a handful of bytes per publish.
///
/// **Only the host calls [`TransportSync::publish`].** Followers merge the
/// host's incremental changes (via [`Self::merge_incremental`]) and read with
/// [`TransportSync::latest`]; they never write, so the last-writer-wins
/// registers never conflict and every follower converges on the host.
pub struct CrdtTransport {
    doc: AutoCommit,
}

impl Default for CrdtTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl CrdtTransport {
    /// A fresh, empty transport document (no state published yet).
    pub fn new() -> Self {
        Self {
            doc: AutoCommit::new(),
        }
    }

    /// Rebuild a transport from a full saved document (a late-joiner adopting a
    /// peer's snapshot).
    ///
    /// # Errors
    ///
    /// Returns the underlying automerge error if `bytes` is not a valid saved
    /// document.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, automerge::AutomergeError> {
        Ok(Self {
            doc: AutoCommit::load(bytes)?,
        })
    }

    /// The full document, for a late-joiner snapshot handoff.
    pub fn save(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// The changes since the last `save` / `save_incremental`, for the
    /// incremental collab path (host → followers). Pairs with
    /// [`Self::merge_incremental`].
    pub fn save_incremental(&mut self) -> Vec<u8> {
        self.doc.save_incremental()
    }

    /// Apply an incremental change blob received from the host.
    ///
    /// # Errors
    ///
    /// Returns the underlying automerge error if `bytes` is not a valid
    /// incremental change set.
    pub fn merge_incremental(&mut self, bytes: &[u8]) -> Result<(), automerge::AutomergeError> {
        self.doc.load_incoming(bytes).map(|_| ())
    }

    /// Merge a full saved document from a peer (the legacy / late-join
    /// snapshot path). Converges with the local state rather than replacing it.
    ///
    /// # Errors
    ///
    /// Returns the underlying automerge error if `bytes` is not a valid saved
    /// document, or if merging it into the local document fails.
    pub fn merge_snapshot(&mut self, bytes: &[u8]) -> Result<(), automerge::AutomergeError> {
        let mut other = AutoCommit::load(bytes)?;
        self.doc.merge_incoming(&mut other).map(|_| ())
    }

    fn read_u64(&self, key: &str) -> Option<u64> {
        match self.doc.get(ROOT, key) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Uint(u) => Some(*u),
                ScalarValue::Int(i) => u64::try_from((*i).max(0)).ok(),
                _ => None,
            },
            _ => None,
        }
    }

    fn read_bool(&self, key: &str) -> Option<bool> {
        match self.doc.get(ROOT, key) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::Boolean(b) => Some(*b),
                _ => None,
            },
            _ => None,
        }
    }

    #[allow(
        clippy::cast_precision_loss,
        reason = "only used to read the rate scalar, a small playback-speed \
                  multiplier far inside f64's exact-integer range even if \
                  stored as Int/Uint"
    )]
    fn read_f64(&self, key: &str) -> Option<f64> {
        match self.doc.get(ROOT, key) {
            Ok(Some((Value::Scalar(s), _))) => match s.as_ref() {
                ScalarValue::F64(f) => Some(*f),
                ScalarValue::Int(i) => Some(*i as f64),
                ScalarValue::Uint(u) => Some(*u as f64),
                _ => None,
            },
            _ => None,
        }
    }
}

impl TransportSync for CrdtTransport {
    fn publish(&mut self, state: &TransportState) {
        // Best-effort: a `put` on the root map only fails for a genuinely
        // corrupt document, which can't happen for scalars we control.
        let _ = self.doc.put(ROOT, K_PRESENT, ScalarValue::Boolean(true));
        let _ = self
            .doc
            .put(ROOT, K_PLAYING, ScalarValue::Boolean(state.playing));
        let _ = self
            .doc
            .put(ROOT, K_POSITION, ScalarValue::Uint(state.position_ms));
        let _ = self
            .doc
            .put(ROOT, K_RATE, ScalarValue::F64(f64::from(state.rate)));
        let _ = self
            .doc
            .put(ROOT, K_EPOCH, ScalarValue::Uint(state.epoch_ms));
    }

    #[allow(
        clippy::cast_possible_truncation,
        reason = "rate is always a small playback-speed multiplier (~0.25..=4.0), \
                  never near f32's range limits"
    )]
    fn latest(&self) -> Option<TransportState> {
        // `present` is set on the first publish; without it there is nothing to
        // follow yet (a fresh document, or a peer who never played).
        if self.read_bool(K_PRESENT) != Some(true) {
            return None;
        }
        Some(TransportState {
            playing: self.read_bool(K_PLAYING).unwrap_or(false),
            position_ms: self.read_u64(K_POSITION).unwrap_or(0),
            rate: self.read_f64(K_RATE).unwrap_or(1.0) as f32,
            epoch_ms: self.read_u64(K_EPOCH).unwrap_or(0),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_projects_drifts_and_syncs_through_the_crdt() {
        // Phase `projection_freezes_while_paused_and_advances_while_playing`.
        {
            let paused = TransportState {
                playing: false,
                position_ms: 4_000,
                rate: 1.0,
                epoch_ms: 10_000,
            };
            assert_eq!(
                paused.projected_position(99_999),
                4_000,
                "paused playhead is frozen"
            );

            let playing = TransportState {
                playing: true,
                position_ms: 4_000,
                rate: 1.0,
                epoch_ms: 10_000,
            };
            // 2.5s of wall-clock since epoch → +2500ms.
            assert_eq!(playing.projected_position(12_500), 6_500);
        }
        // Phase `projection_honours_rate`.
        {
            let fast = TransportState {
                playing: true,
                position_ms: 0,
                rate: 2.0,
                epoch_ms: 0,
            };
            assert_eq!(
                fast.projected_position(1_000),
                2_000,
                "2x rate doubles the advance"
            );
        }
        // Phase `small_drift_stays_in_sync_large_drift_seeks`.
        {
            // 400ms gap < 500ms threshold → ride it.
            assert_eq!(correction(10_000, 10_400), Correction::InSync);
            // 900ms gap in either direction → hard-seek to the host target.
            assert_eq!(correction(10_000, 10_900), Correction::Seek(10_900));
            assert_eq!(correction(20_000, 18_800), Correction::Seek(18_800));
        }
        // Phase `local_transport_never_yields_a_state`.
        {
            let mut t = LocalTransport;
            t.publish(&TransportState {
                playing: true,
                position_ms: 1,
                rate: 1.0,
                epoch_ms: 1,
            });
            assert!(t.latest().is_none());
        }
        // Phase `crdt_transport_round_trips_published_state`.
        {
            let mut host = CrdtTransport::new();
            assert!(host.latest().is_none(), "nothing published yet");
            let state = TransportState {
                playing: true,
                position_ms: 7_250,
                rate: 1.5,
                epoch_ms: 1_700_000,
            };
            host.publish(&state);
            let got = host.latest().expect("state present after publish");
            assert!(got.playing);
            assert_eq!(got.position_ms, 7_250);
            assert!((got.rate - 1.5).abs() < 1e-6);
            assert_eq!(got.epoch_ms, 1_700_000);
        }
        // Phase `follower_converges_on_host_via_incremental_sync`.
        {
            // Host publishes; the incremental blob carries the state to a follower
            // that never writes — exactly the collab-socket follow-the-host path.
            let mut host = CrdtTransport::new();
            let mut follower = CrdtTransport::new();

            host.publish(&TransportState {
                playing: true,
                position_ms: 3_000,
                rate: 1.0,
                epoch_ms: 500,
            });
            let delta = host.save_incremental();
            follower.merge_incremental(&delta).unwrap();

            let f = follower.latest().expect("follower adopted the host state");
            assert_eq!(f.position_ms, 3_000);
            assert!(f.playing);

            // A later host publish (a pause + new epoch) reaches the follower too.
            host.publish(&TransportState {
                playing: false,
                position_ms: 8_000,
                rate: 1.0,
                epoch_ms: 6_000,
            });
            follower
                .merge_incremental(&host.save_incremental())
                .unwrap();
            let f2 = follower.latest().unwrap();
            assert_eq!(f2.position_ms, 8_000);
            assert!(!f2.playing);
        }
    }
}
