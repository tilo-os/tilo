//! Remote-cursor bounds helper and overlay widget stub.
//!
//! T24 lands the smallest load-bearing API: a pure predicate used when the
//! overlay decides which `PresenceCtx::remote_cursors` entries should render
//! over the active tile, plus an `RemoteCursorOverlay` struct fleshed out in
//! T25 (SimulatedFeed drives the cursors) and T26 (draw + per-tile clip).
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
