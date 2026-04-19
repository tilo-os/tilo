//! `SidebarWheel` — left-edge wheel of strip cards for the active workspace.
//!
//! Spec: `docs/superpowers/specs/2026-04-18-nirios-shell-design.md` §5 and
//! §10 — `WorkspaceView = Row([SidebarWheel(strips), StripView(active_strip)])`.
//!
//! T21 owns the skeleton:
//! - a static, vertical stack of strip cards (2D; the full 2.5D radial
//!   geometry is deferred per spec §10),
//! - wheel-to-switch-strip on the **active workspace** via
//!   [`SidebarWheel::on_wheel`], clamped at the strip list's ends.
//!
//! The struct reserves `scroll_offset`, `overscroll`, and `gesture_active`
//! up-front so T22's overscroll-create gesture (and T23's management-button
//! entrypoint) don't need to reshape the type mid-plan. Those later tasks
//! wire the gesture state through `begin_overscroll` / `drag_overscroll` /
//! `release_overscroll`; at this task they stay inert.
//!
//! State lives on the borrowed [`WorkspaceTree`]: `tree.active_strip` is the
//! per-workspace `HashMap<WorkspaceId, StripId>` that records which strip is
//! active on each workspace. The wheel reads that map to locate the current
//! strip in the active workspace's `strips` vector, advances the index by
//! `dir.signum()` clamped to `[0, strips.len() - 1]`, and writes the new
//! `StripId` back into the map.

use crate::shell::state::WorkspaceTree;

/// Logical state for the sidebar wheel.
///
/// At T21 the struct is effectively stateless for the wheel-to-switch path —
/// all authoritative state lives on [`WorkspaceTree`]. The three gesture
/// fields are declared here so T22 can extend behaviour without changing
/// the type's shape or the callers that construct it.
pub struct SidebarWheel {
    /// Continuous vertical offset used by the spring animation when the
    /// wheel settles onto the active strip. Purely presentational.
    pub(crate) scroll_offset: f32,
    /// Accumulated pull past the top/bottom edge during an overscroll
    /// gesture. T22 consumes this to drive the "New session" hint and the
    /// commit-threshold check.
    pub(crate) overscroll: f32,
    /// Whether an overscroll drag is in progress. T22 toggles this via
    /// `begin_overscroll` / `release_overscroll`.
    pub(crate) gesture_active: bool,
}

impl SidebarWheel {
    /// Construct a fresh, idle wheel.
    pub fn new() -> Self {
        Self {
            scroll_offset: 0.0,
            overscroll: 0.0,
            gesture_active: false,
        }
    }

    /// Advance the active strip within the active workspace by `dir.signum()`.
    ///
    /// Clamped at the strip list's ends: at the first/last strip, a further
    /// wheel in that direction is a no-op. T22 layers the overscroll-create
    /// gesture on top of this clamp, which is why the clamp here is a hard
    /// stop rather than a wrap-around.
    pub fn on_wheel(&mut self, dir: i32, tree: &mut WorkspaceTree) {
        if dir == 0 {
            return;
        }
        let step: i32 = if dir > 0 { 1 } else { -1 };
        let active_ws = tree.active;
        // Find the active workspace's strips; if absent (empty tree), nothing to do.
        let Some(ws) = tree.workspaces.iter().find(|w| w.id == active_ws) else {
            return;
        };
        if ws.strips.is_empty() {
            return;
        }
        // Current strip index in the active workspace, defaulting to 0 if
        // `active_strip` hasn't recorded one yet (e.g. freshly-created ws).
        let current_idx = match tree.active_strip.get(&active_ws).copied() {
            Some(id) => ws.strips.iter().position(|s| s.id == id).unwrap_or(0),
            None => 0,
        };
        let len = ws.strips.len() as i32;
        let next_idx = (current_idx as i32 + step).clamp(0, len - 1) as usize;
        let next_id = ws.strips[next_idx].id;
        tree.active_strip.insert(active_ws, next_id);
    }

    /// Current scroll offset (presentational). Exposed for widget rendering
    /// in later tasks; the skeleton value is always `0.0`.
    pub fn scroll_offset(&self) -> f32 {
        self.scroll_offset
    }
}

impl Default for SidebarWheel {
    fn default() -> Self {
        Self::new()
    }
}
