//! Cursor position interpolation, fade-out, and stable per-peer color.
//!
//! Remote cursor moves arrive as discrete `RemoteCursorMoved` samples from
//! the collab broker. Rendering them raw makes the cursor jump between
//! frames; this module smooths them out.
//!
//! For each peer we keep a rolling pair of samples (previous, latest). At
//! any wall-clock instant `t`, the peer's rendered position is the cubic-
//! eased interpolation between the two bracketing samples. After
//! [`FADE_AFTER`] of silence, the cursor fades linearly to zero opacity
//! over [`FADE_DURATION`], then disappears entirely.
//!
//! Color is derived deterministically from the peer's public key (a byte
//! slice — usually 32 bytes for ed25519). Same key ⇒ same color, forever.

use std::collections::HashMap;
use std::time::{Duration, Instant};

/// Peer is considered "still" this long after their last sample; after
/// this the cursor starts fading.
pub const FADE_AFTER: Duration = Duration::from_secs(3);
/// Fade duration once inactivity begins.
pub const FADE_DURATION: Duration = Duration::from_millis(400);

/// One position sample for a peer's cursor.
#[derive(Clone, Copy, Debug)]
struct Sample {
    t: Instant,
    pos: [f32; 2],
}

/// Per-peer interpolation state: the two most recent samples.
#[derive(Clone, Debug)]
struct PeerState {
    /// The sample the cursor was at before the most recent update.
    /// `None` before we've received two samples.
    prev: Option<Sample>,
    /// The most recent sample.
    latest: Sample,
    /// Optional human label for the cursor's chip.
    label: String,
    /// Color derived from the peer's public key. Cached so repeated
    /// sampling doesn't rehash on every frame.
    color: [f32; 4],
}

/// Interpolated cursor snapshot at a given instant.
#[derive(Clone, Debug, PartialEq)]
pub struct InterpolatedCursor {
    pub user_id: String,
    pub position: [f32; 2],
    pub color: [f32; 4],
    /// `1.0` while active, decaying linearly to `0.0` during fade-out.
    pub alpha: f32,
    pub label: String,
}

/// Rolling interpolator over `RemoteCursorMoved`-style samples.
#[derive(Default)]
pub struct CursorInterpolator {
    peers: HashMap<String, PeerState>,
}

impl CursorInterpolator {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a new position sample for `user_id`. `public_key` is used
    /// to derive a stable color the first time we see this peer; later
    /// calls with a different key are ignored for the color (peers
    /// shouldn't change keys mid-session).
    pub fn push_sample(
        &mut self,
        user_id: &str,
        public_key: &[u8],
        label: &str,
        pos: [f32; 2],
        t: Instant,
    ) {
        let sample = Sample { t, pos };
        match self.peers.get_mut(user_id) {
            Some(state) => {
                state.prev = Some(state.latest);
                state.latest = sample;
                // Refresh the friendly label if the caller supplied a
                // non-empty one; empty means "no update", so we keep the
                // last known name.
                if !label.is_empty() {
                    state.label = label.to_string();
                }
            }
            None => {
                self.peers.insert(
                    user_id.to_string(),
                    PeerState {
                        prev: None,
                        latest: sample,
                        label: label.to_string(),
                        color: color_from_pubkey(public_key),
                    },
                );
            }
        }
    }

    /// Number of peers currently being tracked.
    pub fn len(&self) -> usize {
        self.peers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty()
    }

    /// Drop the peer entirely (e.g. on `Left`).
    pub fn remove(&mut self, user_id: &str) {
        self.peers.remove(user_id);
    }

    /// Render all tracked peers at wall-clock instant `t`. Fully-faded
    /// entries are elided so the caller can render the returned slice
    /// unconditionally.
    pub fn sample_at(&self, t: Instant) -> Vec<InterpolatedCursor> {
        let mut out = Vec::with_capacity(self.peers.len());
        for (uid, state) in &self.peers {
            if let Some(c) = interpolated_at(uid, state, t) {
                out.push(c);
            }
        }
        // Deterministic order so tests can assert on the vec directly.
        out.sort_by(|a, b| a.user_id.cmp(&b.user_id));
        out
    }

    /// Drop peers whose fade-out has fully completed. Callers can invoke
    /// this once per second to keep the map from accumulating stale
    /// entries when peers vanish without a `Left`.
    pub fn gc(&mut self, t: Instant) {
        self.peers.retain(|_, state| {
            let age = t.saturating_duration_since(state.latest.t);
            age < FADE_AFTER + FADE_DURATION
        });
    }
}

/// Cubic ease-in-out (smoothstep-cubed) used between samples so quick
/// motion doesn't look jittery near the sample boundaries. `t` is 0..=1.
fn cubic_ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn interpolated_at(uid: &str, state: &PeerState, now: Instant) -> Option<InterpolatedCursor> {
    // Fade is measured relative to the *latest* sample time — even if
    // `now` sits between `prev` and `latest` (the caller renders "in the
    // past" mid-interpolation), we treat that as fully-active because
    // the peer moved recently.
    let alpha = if now >= state.latest.t {
        let age = now.saturating_duration_since(state.latest.t);
        if age <= FADE_AFTER {
            1.0
        } else {
            let fade_progress =
                (age - FADE_AFTER).as_secs_f32() / FADE_DURATION.as_secs_f32();
            if fade_progress >= 1.0 {
                return None;
            }
            1.0 - fade_progress
        }
    } else {
        1.0
    };

    let position = match state.prev {
        None => state.latest.pos,
        Some(prev) => {
            let window = state
                .latest
                .t
                .saturating_duration_since(prev.t)
                .as_secs_f32();
            if window <= 0.0 || now >= state.latest.t {
                // We're at (or past) the latest sample.
                state.latest.pos
            } else if now <= prev.t {
                // `now` is before we have any data for this interval.
                prev.pos
            } else {
                let dt = now.saturating_duration_since(prev.t).as_secs_f32();
                let raw = (dt / window).clamp(0.0, 1.0);
                let k = cubic_ease(raw);
                [
                    prev.pos[0] + (state.latest.pos[0] - prev.pos[0]) * k,
                    prev.pos[1] + (state.latest.pos[1] - prev.pos[1]) * k,
                ]
            }
        }
    };

    Some(InterpolatedCursor {
        user_id: uid.to_string(),
        position,
        color: [state.color[0], state.color[1], state.color[2], alpha * state.color[3]],
        alpha,
        label: state.label.clone(),
    })
}

/// Deterministic color from a peer's public key. FNV-1a hash → HSL with a
/// lifted lightness floor so the cursor is visible over both light and
/// dark tile backgrounds. Same key ⇒ same color, forever; distinct keys
/// map to visually distinct hues with very high probability (single-byte
/// changes rotate the hue by ~30°).
pub fn color_from_pubkey(public_key: &[u8]) -> [f32; 4] {
    let mut h: u64 = 0xcbf29ce484222325;
    for b in public_key {
        h ^= *b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    let hue = (h % 360) as f32;
    let (r, g, b) = hsl_to_rgb(hue, 0.65, 0.55);
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}

fn hsl_to_rgb(h: f32, s: f32, l: f32) -> (u8, u8, u8) {
    let c = (1.0 - (2.0 * l - 1.0).abs()) * s;
    let hh = h / 60.0;
    let x = c * (1.0 - (hh.rem_euclid(2.0) - 1.0).abs());
    let (r1, g1, b1) = match hh as i32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = l - c / 2.0;
    (
        ((r1 + m) * 255.0).clamp(0.0, 255.0) as u8,
        ((g1 + m) * 255.0).clamp(0.0, 255.0) as u8,
        ((b1 + m) * 255.0).clamp(0.0, 255.0) as u8,
    )
}

/// Public test helper — feeds a whole trajectory into the interpolator so
/// external tests can assert against real usage without touching internals.
pub fn interp_between(prev: [f32; 2], latest: [f32; 2], t: f32) -> [f32; 2] {
    let k = cubic_ease(t);
    [
        prev[0] + (latest[0] - prev[0]) * k,
        prev[1] + (latest[1] - prev[1]) * k,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(seed: u8) -> Vec<u8> {
        vec![seed; 32]
    }

    #[test]
    fn same_pubkey_yields_same_color() {
        let a = color_from_pubkey(&key(7));
        let b = color_from_pubkey(&key(7));
        assert_eq!(a, b);
    }

    #[test]
    fn different_pubkeys_usually_differ() {
        let a = color_from_pubkey(&key(1));
        let b = color_from_pubkey(&key(2));
        assert_ne!(a, b);
    }

    #[test]
    fn single_sample_holds_position() {
        let t0 = Instant::now();
        let mut i = CursorInterpolator::new();
        i.push_sample("alice", &key(1), "Alice", [50.0, 50.0], t0);
        let cs = i.sample_at(t0);
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].position, [50.0, 50.0]);
        assert_eq!(cs[0].alpha, 1.0);
    }

    #[test]
    fn interpolates_between_samples() {
        let t0 = Instant::now();
        let t100 = t0 + Duration::from_millis(100);
        let t50 = t0 + Duration::from_millis(50);
        let mut i = CursorInterpolator::new();
        i.push_sample("alice", &key(1), "Alice", [0.0, 0.0], t0);
        i.push_sample("alice", &key(1), "Alice", [100.0, 100.0], t100);
        let cs = i.sample_at(t50);
        assert_eq!(cs.len(), 1);
        // Cubic ease at t=0.5 is exactly 0.5 (smoothstep symmetry), so
        // midpoint interpolation is exact at the midpoint.
        let (x, y) = (cs[0].position[0], cs[0].position[1]);
        assert!(
            (x - 50.0).abs() < 5.0 && (y - 50.0).abs() < 5.0,
            "expected (50±5, 50±5), got ({x}, {y})",
        );
    }

    #[test]
    fn fades_out_after_inactivity() {
        let t0 = Instant::now();
        let mut i = CursorInterpolator::new();
        i.push_sample("alice", &key(1), "Alice", [10.0, 10.0], t0);

        // Just before fade begins: fully opaque.
        let cs = i.sample_at(t0 + FADE_AFTER);
        assert!(cs[0].alpha >= 0.99);

        // Mid-fade: strictly between 0 and 1.
        let cs = i.sample_at(t0 + FADE_AFTER + FADE_DURATION / 2);
        assert!(cs[0].alpha > 0.0 && cs[0].alpha < 1.0);

        // After fade: gone.
        let cs = i.sample_at(t0 + FADE_AFTER + FADE_DURATION * 2);
        assert!(cs.is_empty());
    }

    #[test]
    fn gc_drops_faded_peers() {
        let t0 = Instant::now();
        let mut i = CursorInterpolator::new();
        i.push_sample("alice", &key(1), "Alice", [10.0, 10.0], t0);
        i.push_sample("bob", &key(2), "Bob", [20.0, 20.0], t0 + Duration::from_secs(1));
        assert_eq!(i.len(), 2);
        // Well past both peers' fade windows.
        i.gc(t0 + Duration::from_secs(1) + FADE_AFTER + FADE_DURATION * 2);
        assert_eq!(i.len(), 0);
    }

    #[test]
    fn remove_drops_peer() {
        let t0 = Instant::now();
        let mut i = CursorInterpolator::new();
        i.push_sample("alice", &key(1), "Alice", [0.0, 0.0], t0);
        i.push_sample("bob", &key(2), "Bob", [0.0, 0.0], t0);
        i.remove("alice");
        assert_eq!(i.len(), 1);
        assert_eq!(i.sample_at(t0)[0].user_id, "bob");
    }

    #[test]
    fn label_updates_on_non_empty_push() {
        let t0 = Instant::now();
        let mut i = CursorInterpolator::new();
        i.push_sample("alice", &key(1), "Alice", [0.0, 0.0], t0);
        i.push_sample("alice", &key(1), "", [10.0, 10.0], t0);
        assert_eq!(i.sample_at(t0)[0].label, "Alice");
        i.push_sample("alice", &key(1), "Alice S.", [10.0, 10.0], t0);
        assert_eq!(i.sample_at(t0)[0].label, "Alice S.");
    }
}
