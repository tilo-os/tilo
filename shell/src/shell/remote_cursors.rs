//! Remote-cursor bounds helper, overlay widget stub, and a simulated feed.
//!
//! T24 landed the smallest load-bearing API: a pure predicate used when the
//! overlay decides which `PresenceCtx::remote_cursors` entries should render
//! over the active tile, plus an `RemoteCursorOverlay` struct fleshed out in
//! T25 (SimulatedFeed drives the cursors) and T26 (draw + per-tile clip).
//!
//! T25 adds `SimulatedFeed`, which emits a single deterministic fake remote
//! cursor so the chrome can be demoed without a real peer on the wire. It
//! writes directly into `PresenceCtx::remote_cursors`, which is the same
//! surface the real collaboration code feeds into, so the overlay widget
//! (T26) doesn't need to know whether its input is real or simulated.
//!
//! The bounds are `[x, y, w, h]` in scene-space — the same coordinate space
//! that `PresenceCtx::remote_cursors[_].position` uses, so callers can pass
//! the active tile's layout rect directly.

/// Returns true when `pos` lies inside the rectangle `bounds = [x, y, w, h]`.
///
/// The check is half-open on the right and bottom edges, matching the
/// convention used by taffy-derived `Rect`s elsewhere in the shell.
pub fn cursor_in_bounds(pos: [f32; 2], bounds: [f32; 4]) -> bool {
    let [x, y, w, h] = bounds;
    let [px, py] = pos;
    px >= x && px < x + w && py >= y && py < y + h
}

/// Overlay widget placeholder. T24 only establishes the type so T25/T26 can
/// attach simulation and draw behaviour without touching the public surface
/// again. The active-tile bounds default to a zero rect; the shell sets them
/// on each frame before delegating draw.
#[derive(Debug, Clone, Default)]
pub struct RemoteCursorOverlay {
    active_tile_bounds: [f32; 4],
}

impl RemoteCursorOverlay {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active_tile_bounds(&self) -> [f32; 4] {
        self.active_tile_bounds
    }

    pub fn set_active_tile_bounds(&mut self, bounds: [f32; 4]) {
        self.active_tile_bounds = bounds;
    }
}

/// Deterministic fake cursor source used by the shell demo/tests.
///
/// Spawns a single peer ("Ada") whose cursor traces a smooth Lissajous-ish
/// path derived from accumulated time, so scrubbing through frames or
/// replaying with the same seed always produces the same trajectory. The
/// seed only affects the peer's display color — the motion is seed-independent
/// so tests can assert position equality/inequality without knowing the seed.
#[derive(Debug, Clone)]
pub struct SimulatedFeed {
    t: std::time::Duration,
    seed: u64,
    user: String,
    color: [f32; 4],
}

impl SimulatedFeed {
    /// Creates a feed with a single peer "Ada" whose display color is derived
    /// from `seed`. The accumulated time starts at zero so the first `tick`
    /// produces a position offset from the idle origin.
    pub fn new_with_seed(seed: u64) -> Self {
        Self {
            t: std::time::Duration::ZERO,
            seed,
            user: "Ada".to_string(),
            color: color_from_seed(seed),
        }
    }

    /// Advances the feed by `dt` and rewrites `ctx.remote_cursors` with the
    /// single simulated peer's current position. Subsequent calls replace the
    /// vec contents so a single feed never leaks multiple phantom cursors.
    pub fn tick(&mut self, dt: std::time::Duration, ctx: &mut quick_ui::presence::PresenceCtx) {
        self.t += dt;
        let t_secs = self.t.as_secs_f32();
        let position = [
            200.0 + 80.0 * t_secs.sin(),
            150.0 + 60.0 * (t_secs * 0.7).cos(),
        ];
        ctx.remote_cursors.clear();
        ctx.remote_cursors.push(quick_ui::presence::RemoteCursor {
            user_id: format!("sim-{}", self.seed),
            color: self.color,
            position,
            label: self.user.clone(),
        });
    }
}

/// Maps `seed` to a stable, pleasant-looking RGBA color. We avoid pulling a
/// random generator into the hot path: a splitmix-style mix of the seed gives
/// us three independent bytes, normalised to `[0.3, 1.0]` so the cursor stays
/// visible over both light and dark tile backgrounds.
fn color_from_seed(seed: u64) -> [f32; 4] {
    let mut z = seed.wrapping_add(0x9e3779b97f4a7c15);
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d049bb133111eb);
    z ^= z >> 31;
    let r = ((z & 0xff) as f32) / 255.0;
    let g = (((z >> 8) & 0xff) as f32) / 255.0;
    let b = (((z >> 16) & 0xff) as f32) / 255.0;
    // Lift each channel into [0.3, 1.0] so no component is too dark.
    let lift = |c: f32| 0.3 + 0.7 * c;
    [lift(r), lift(g), lift(b), 1.0]
}
