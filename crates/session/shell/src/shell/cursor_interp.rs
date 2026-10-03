//! Cursor position interpolation and fade-out.
//!
//! Remote cursor moves arrive as discrete presence samples over the peer
//! mesh at roughly 30 Hz; rendering them raw makes the cursor jump between
//! frames. For each peer we keep a rolling (previous, latest) pair and
//! cubic-ease between them at any wall-clock instant `t`. After
//! [`FADE_AFTER`] of silence the cursor fades to zero opacity over
//! [`FADE_DURATION`] and disappears — which is also what retires a peer
//! that drops off the mesh without ever announcing `PresencePayload::Left`.
//!
//! [`CursorInterpolator::apply_to`] is the compositor's entry point.
//! Colour is deliberately *not* computed here — [`PresenceCtx::color_for`]
//! owns it, so a peer keeps one colour everywhere it's drawn.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use ui::presence::PresenceCtx;

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
    /// Sample before the most recent update; `None` until we've received two.
    prev: Option<Sample>,
    latest: Sample,
    /// Human label for the cursor's chip.
    label: String,
}

/// Interpolated cursor snapshot at a given instant.
#[derive(Clone, Debug, PartialEq)]
pub struct InterpolatedCursor {
    pub user_id: String,
    pub position: [f32; 2],
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

    pub fn push_sample(&mut self, user_id: &str, label: &str, pos: [f32; 2], t: Instant) {
        let sample = Sample { t, pos };
        match self.peers.get_mut(user_id) {
            Some(state) => {
                state.prev = Some(state.latest);
                state.latest = sample;
                // Empty label means "no update"; keep the last known name.
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
                    },
                );
            }
        }
    }

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

    /// Drop peers whose fade-out has fully completed, so the map doesn't
    /// accumulate stale entries when peers vanish without a `Left`.
    pub fn gc(&mut self, t: Instant) {
        self.peers.retain(|_, state| {
            let age = t.saturating_duration_since(state.latest.t);
            age < FADE_AFTER.saturating_add(FADE_DURATION)
        });
    }

    /// Reflects state at `now` into `ctx`, then reclaims fully-faded peers
    /// — otherwise a peer that silently drops off the mesh (no `Left`
    /// event) leaves an arrow parked on screen forever. Peers this
    /// interpolator has never seen are left untouched, so it coexists with
    /// the toolkit's own socket-driven presence pump.
    /// Returns whether the window-owned cursor projection actually changed.
    /// Callers use this to avoid turning a quiet networking pump into a full
    /// scene redraw on every event-loop tick.
    #[allow(
        clippy::float_cmp,
        reason = "cursor redraw detection compares the exact projected coordinates and alpha written to PresenceCtx"
    )]
    pub fn apply_to(&mut self, ctx: &mut PresenceCtx, now: Instant) -> bool {
        let live = self.sample_at(now);
        let mut changed = false;
        for cursor in &live {
            let label = if cursor.label.is_empty() {
                None
            } else {
                Some(cursor.label.as_str())
            };
            changed |= ctx
                .remote_cursors
                .iter()
                .find(|existing| existing.user_id == cursor.user_id)
                .is_none_or(|existing| {
                    existing.position != cursor.position
                        || existing.color[3] != cursor.alpha
                        || label.is_some_and(|label| existing.label != label)
                });
            ctx.upsert_remote_cursor_faded(
                cursor.user_id.clone(),
                cursor.position,
                label,
                cursor.alpha,
            );
        }
        // Anything we track that didn't survive `sample_at` has faded out.
        let faded: Vec<String> = self
            .peers
            .keys()
            .filter(|uid| !live.iter().any(|c| &&c.user_id == uid))
            .cloned()
            .collect();
        for uid in faded {
            ctx.remove_remote_cursor(&uid);
            changed = true;
        }
        self.gc(now);
        changed
    }
}

/// Cubic ease-in-out (smoothstep-cubed) used between samples so quick
/// motion doesn't look jittery near the sample boundaries. `t` is 0..=1.
fn cubic_ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn interpolated_at(uid: &str, state: &PeerState, now: Instant) -> Option<InterpolatedCursor> {
    // Fade is relative to the *latest* sample; `now` between `prev` and
    // `latest` counts as fully-active since the peer moved recently.
    let alpha = if now >= state.latest.t {
        let age = now.saturating_duration_since(state.latest.t);
        if age <= FADE_AFTER {
            1.0
        } else {
            // `age > FADE_AFTER` is guaranteed by the branch above.
            let fade_progress = age
                .checked_sub(FADE_AFTER)
                .unwrap_or(Duration::ZERO)
                .as_secs_f32()
                / FADE_DURATION.as_secs_f32();
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
        alpha,
        label: state.label.clone(),
    })
}

#[cfg(test)]
#[allow(
    clippy::float_cmp,
    reason = "exact interpolation endpoints and clamping are the behavior under test"
)]
mod tests {
    use super::*;

    #[test]
    fn cursor_samples_interpolate_forward_and_hold_the_latest_position() {
        fn single_sample_holds_position() {
            let t0 = Instant::now();
            let mut i = CursorInterpolator::new();
            i.push_sample("alice", "Alice", [50.0, 50.0], t0);
            let cs = i.sample_at(t0);
            assert_eq!(cs.len(), 1);
            assert_eq!(cs[0].position, [50.0, 50.0]);
            assert_eq!(cs[0].alpha, 1.0);
        }

        fn interpolates_between_samples() {
            let t0 = Instant::now();
            let t100 = t0 + Duration::from_millis(100);
            let t50 = t0 + Duration::from_millis(50);
            let mut i = CursorInterpolator::new();
            i.push_sample("alice", "Alice", [0.0, 0.0], t0);
            i.push_sample("alice", "Alice", [100.0, 100.0], t100);
            let cs = i.sample_at(t50);
            assert_eq!(cs.len(), 1);
            // Cubic ease at t=0.5 is exactly 0.5 (smoothstep symmetry).
            let (x, y) = (cs[0].position[0], cs[0].position[1]);
            assert!(
                (x - 50.0).abs() < 5.0 && (y - 50.0).abs() < 5.0,
                "expected (50\u{b1}5, 50\u{b1}5), got ({x}, {y})",
            );
        }

        fn holds_latest_position_past_the_window() {
            let t0 = Instant::now();
            let t100 = t0 + Duration::from_millis(100);
            let mut i = CursorInterpolator::new();
            i.push_sample("alice", "Alice", [0.0, 0.0], t0);
            i.push_sample("alice", "Alice", [100.0, 100.0], t100);
            let cs = i.sample_at(t100 + Duration::from_millis(500)); // well past t100, before fade
            assert_eq!(cs[0].position, [100.0, 100.0]);
        }

        fn stream_of_samples_never_moves_backwards() {
            let t0 = Instant::now();
            let t100 = t0 + Duration::from_millis(100);
            let mut i = CursorInterpolator::new();
            i.push_sample("alice", "", [0.0, 0.0], t0);
            i.push_sample("alice", "", [100.0, 100.0], t100);

            let mut xs = vec![];
            for ms in [0u64, 25, 50, 75, 100] {
                let cs = i.sample_at(t0 + Duration::from_millis(ms));
                xs.push(cs[0].position[0]);
            }
            for w in xs.windows(2) {
                assert!(
                    w[1] >= w[0] - 0.01,
                    "position must not go backwards across samples: {xs:?}",
                );
            }
            assert!((xs[2] - 50.0).abs() <= 5.0);
        }

        fn label_updates_on_non_empty_push() {
            let t0 = Instant::now();
            let mut i = CursorInterpolator::new();
            i.push_sample("alice", "Alice", [0.0, 0.0], t0);
            i.push_sample("alice", "", [10.0, 10.0], t0);
            assert_eq!(i.sample_at(t0)[0].label, "Alice");
            i.push_sample("alice", "Alice S.", [10.0, 10.0], t0);
            assert_eq!(i.sample_at(t0)[0].label, "Alice S.");
        }

        single_sample_holds_position();
        interpolates_between_samples();
        holds_latest_position_past_the_window();
        stream_of_samples_never_moves_backwards();
        label_updates_on_non_empty_push();
    }

    #[test]
    fn silent_cursors_fade_and_are_collected_or_removed() {
        fn fades_out_after_inactivity() {
            let t0 = Instant::now();
            let mut i = CursorInterpolator::new();
            i.push_sample("alice", "Alice", [10.0, 10.0], t0);

            let cs = i.sample_at(t0 + FADE_AFTER); // just before fade begins
            assert!(cs[0].alpha >= 0.99);

            let cs = i.sample_at(t0 + FADE_AFTER + FADE_DURATION / 2); // mid-fade
            assert!(cs[0].alpha > 0.0 && cs[0].alpha < 1.0);

            let cs = i.sample_at(t0 + FADE_AFTER + FADE_DURATION * 2); // after fade: gone
            assert!(cs.is_empty());
        }

        fn gc_drops_faded_peers() {
            let t0 = Instant::now();
            let mut i = CursorInterpolator::new();
            i.push_sample("alice", "Alice", [10.0, 10.0], t0);
            i.push_sample("bob", "Bob", [20.0, 20.0], t0 + Duration::from_secs(1));
            assert_eq!(i.len(), 2);
            i.gc(t0 + Duration::from_secs(1) + FADE_AFTER + FADE_DURATION * 2); // well past both fade windows
            assert_eq!(i.len(), 0);
        }

        fn remove_drops_peer() {
            let t0 = Instant::now();
            let mut i = CursorInterpolator::new();
            i.push_sample("alice", "Alice", [0.0, 0.0], t0);
            i.push_sample("bob", "Bob", [0.0, 0.0], t0);
            i.remove("alice");
            assert_eq!(i.len(), 1);
            assert_eq!(i.sample_at(t0)[0].user_id, "bob");
        }

        fades_out_after_inactivity();
        gc_drops_faded_peers();
        remove_drops_peer();
    }

    // --- apply_to: the path the compositor's frame tick actually runs ---

    #[test]
    fn apply_to_smooths_owned_cursors_with_the_shared_palette_only() {
        fn unchanged_projection_does_not_request_another_frame() {
            let t0 = Instant::now();
            let mut ctx = PresenceCtx::empty();
            let mut i = CursorInterpolator::new();
            i.push_sample("alice", "Alice", [40.0, 20.0], t0);
            assert!(i.apply_to(&mut ctx, t0));
            assert!(!i.apply_to(&mut ctx, t0 + Duration::from_millis(16)));
        }

        fn apply_to_smooths_the_cursor_the_overlay_reads() {
            let t0 = Instant::now();
            let mut ctx = PresenceCtx::empty();
            let mut i = CursorInterpolator::new();
            i.push_sample("alice", "Alice", [0.0, 0.0], t0);
            i.push_sample(
                "alice",
                "Alice",
                [100.0, 100.0],
                t0 + Duration::from_millis(100),
            );

            i.apply_to(&mut ctx, t0 + Duration::from_millis(50));

            assert_eq!(ctx.remote_cursors.len(), 1);
            let c = &ctx.remote_cursors[0];
            assert_eq!(c.user_id, "alice");
            assert_eq!(c.label, "Alice");
            assert!(
                (c.position[0] - 50.0).abs() < 5.0 && (c.position[1] - 50.0).abs() < 5.0,
                "expected the eased midpoint, got {:?}",
                c.position,
            );
        }

        fn apply_to_uses_the_shared_palette_not_a_private_one() {
            let t0 = Instant::now();
            let mut ctx = PresenceCtx::empty();
            let mut interp = CursorInterpolator::new();
            interp.push_sample("alice", "Alice", [1.0, 2.0], t0);
            interp.apply_to(&mut ctx, t0);

            let (red, green, blue) = ui::presence::user_color("alice");
            let cursor = &ctx.remote_cursors[0];
            assert!((cursor.color[0] - f32::from(red) / 255.0).abs() < 1e-3);
            assert!((cursor.color[1] - f32::from(green) / 255.0).abs() < 1e-3);
            assert!((cursor.color[2] - f32::from(blue) / 255.0).abs() < 1e-3);
        }

        fn apply_to_fades_then_retires_a_silent_peer() {
            let t0 = Instant::now();
            let mut ctx = PresenceCtx::empty();
            let mut i = CursorInterpolator::new();
            i.push_sample("alice", "Alice", [10.0, 10.0], t0);

            i.apply_to(&mut ctx, t0);
            assert_eq!(
                ctx.remote_cursors[0].color[3], 1.0,
                "fresh sample is opaque"
            );

            i.apply_to(&mut ctx, t0 + FADE_AFTER + FADE_DURATION / 2); // mid-fade
            assert_eq!(ctx.remote_cursors.len(), 1);
            let a = ctx.remote_cursors[0].color[3];
            assert!(a > 0.0 && a < 1.0, "expected a partial fade, got {a}");

            i.apply_to(&mut ctx, t0 + FADE_AFTER + FADE_DURATION * 2); // past the fade window
            assert!(
                ctx.remote_cursors.is_empty(),
                "a peer that went silent must not leave a cursor behind",
            );
        }

        fn apply_to_leaves_cursors_it_does_not_own_alone() {
            let t0 = Instant::now();
            let mut ctx = PresenceCtx::empty();
            ctx.upsert_remote_cursor("bob".into(), [7.0, 7.0], Some("Bob"));

            let mut i = CursorInterpolator::new();
            i.push_sample("alice", "Alice", [10.0, 10.0], t0);
            i.apply_to(&mut ctx, t0 + FADE_AFTER + FADE_DURATION * 2);

            let ids: Vec<&str> = ctx
                .remote_cursors
                .iter()
                .map(|c| c.user_id.as_str())
                .collect();
            assert_eq!(ids, vec!["bob"]);
        }

        unchanged_projection_does_not_request_another_frame();
        apply_to_smooths_the_cursor_the_overlay_reads();
        apply_to_uses_the_shared_palette_not_a_private_one();
        apply_to_fades_then_retires_a_silent_peer();
        apply_to_leaves_cursors_it_does_not_own_alone();
    }
}
