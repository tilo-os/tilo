//! `SidebarWheel` — left-edge wheel of strip cards for the active workspace.
//!
//! Spec: `docs/superpowers/specs/2026-04-18-nirios-shell-design.md` §5 and
//! §10 — `WorkspaceView = Row([SidebarWheel(strips), StripView(active_strip)])`.
//!
//! T21 seeded the skeleton and wheel-to-switch-strip behaviour. T22 activates
//! the gesture fields to implement **overscroll-create**: when the user
//! continues wheeling past the first or last strip of the active workspace,
//! the pull accumulates in [`SidebarWheel::overscroll`] and — if released
//! past [`OVERSCROLL_THRESHOLD`] — commits a freshly-minted workspace onto
//! the tree (prepended for an upward pull, appended for downward) and
//! activates it. Releasing below the threshold snaps the accumulator back to
//! zero.
//!
//! Responsibilities split across the three public methods:
//! - [`SidebarWheel::begin_overscroll`] marks the gesture active and records
//!   the edge direction (`+1` = bottom, `-1` = top).
//! - [`SidebarWheel::drag_overscroll`] accumulates absolute pixel delta
//!   **only when the drag direction matches the recorded edge**; opposite
//!   motion is a no-op here (the caller resumes normal wheel navigation via
//!   [`SidebarWheel::on_wheel`]).
//! - [`SidebarWheel::release_overscroll`] either commits + activates a new
//!   workspace (progress ≥ 1.0) or snaps `overscroll` back to 0.
//!
//! The "New session" hint is driven by two pure projections:
//! - [`SidebarWheel::overscroll_hint_visible`] — true once pull exceeds a
//!   small dead-zone (32px) so a few pixels of accidental overscroll don't
//!   flash the hint card.
//! - [`SidebarWheel::overscroll_progress`] — clamped `[0, 1]` ratio that
//!   drives the hint card's alpha in the widget layer.
//!
//! State lives on the borrowed [`WorkspaceTree`]: `tree.active_strip` is the
//! per-workspace `HashMap<WorkspaceId, StripId>` that records which strip is
//! active on each workspace. The wheel reads that map to locate the current
//! strip in the active workspace's `strips` vector, advances the index by
//! `dir.signum()` clamped to `[0, strips.len() - 1]`, and writes the new
//! `StripId` back into the map.

use crate::shell::state::{Strip, StripId, Workspace, WorkspaceId, WorkspaceTree};

/// Pixel-pull at which a release commits a new workspace. Below this the
/// gesture snaps back. The threshold is deliberately generous — 240px is
/// roughly one full mouse-wheel flick — so the commit reads as an intentional
/// gesture rather than an accidental flick past the edge.
pub const OVERSCROLL_THRESHOLD: f32 = 240.0;

/// Dead-zone below which the "New session" hint stays hidden. Tuned so a
/// handful of pixels of bounce-past-the-edge don't visibly flash the hint.
const OVERSCROLL_HINT_DEADZONE: f32 = 32.0;

/// Logical state for the sidebar wheel.
///
/// Authoritative workspace/strip state lives on the borrowed
/// [`WorkspaceTree`]; the wheel itself only owns presentational scroll state
/// and the transient overscroll accumulator used by the gesture.
pub struct SidebarWheel {
    /// Continuous vertical offset used by the spring animation when the
    /// wheel settles onto the active strip. Purely presentational.
    pub(crate) scroll_offset: f32,
    /// Accumulated pull past the top/bottom edge during an overscroll
    /// gesture. Drives the "New session" hint and the commit-threshold
    /// check in [`SidebarWheel::release_overscroll`].
    pub(crate) overscroll: f32,
    /// Whether an overscroll drag is in progress. Toggled on by
    /// [`SidebarWheel::begin_overscroll`] and off by
    /// [`SidebarWheel::release_overscroll`]; while true the wheel asks for
    /// pointer capture so stray motion outside the sidebar keeps feeding
    /// the gesture.
    pub(crate) gesture_active: bool,
    /// Direction of the active edge pull: `+1` for the bottom edge (wheel
    /// down past the last strip), `-1` for the top edge. Zero when no
    /// gesture is active. Stored so `drag_overscroll` can ignore motion
    /// that reverses direction mid-gesture.
    pub(crate) overscroll_dir: i32,
}

impl SidebarWheel {
    /// Construct a fresh, idle wheel.
    pub fn new() -> Self {
        Self {
            scroll_offset: 0.0,
            overscroll: 0.0,
            gesture_active: false,
            overscroll_dir: 0,
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

    /// Start an overscroll gesture against the `dir` edge.
    ///
    /// `dir > 0` = bottom edge (user pulled past the last strip), `dir < 0`
    /// = top edge. A zero direction is a no-op. The accumulator is reset
    /// so a subsequent release snaps cleanly to 0 if the drag is short.
    pub fn begin_overscroll(&mut self, dir: i32) {
        if dir == 0 {
            return;
        }
        self.gesture_active = true;
        self.overscroll_dir = dir.signum();
        self.overscroll = 0.0;
    }

    /// Accumulate `px` (absolute pixels) of pull past the edge.
    ///
    /// Only motion whose sign matches the edge direction recorded by
    /// [`SidebarWheel::begin_overscroll`] contributes — reverse motion is
    /// dropped so a flicky wheel can't both commit and snap back in the
    /// same gesture. Values are `abs()`-ed defensively; callers may pass
    /// signed deltas without harm.
    pub fn drag_overscroll(&mut self, dir: i32, px: f32) {
        if !self.gesture_active {
            return;
        }
        if dir.signum() != self.overscroll_dir {
            return;
        }
        self.overscroll += px.abs();
    }

    /// Whether the "New session" hint card should be drawn.
    ///
    /// True once the accumulator exceeds a small dead-zone so a pixel or
    /// two of bounce doesn't flash the hint.
    pub fn overscroll_hint_visible(&self) -> bool {
        self.overscroll > OVERSCROLL_HINT_DEADZONE
    }

    /// Clamped `[0, 1]` progress toward the commit threshold. Intended for
    /// the hint card's alpha (and, in a later task, its scale tween).
    pub fn overscroll_progress(&self) -> f32 {
        (self.overscroll / OVERSCROLL_THRESHOLD).min(1.0).max(0.0)
    }

    /// While a gesture is active the widget layer should hold pointer
    /// capture so motion outside the sidebar keeps feeding the drag.
    pub fn wants_pointer_capture(&self) -> bool {
        self.gesture_active
    }

    /// End the gesture. If the pull crossed [`OVERSCROLL_THRESHOLD`], a new
    /// workspace is inserted into `tree` (prepended for a top-edge pull,
    /// appended for a bottom-edge pull) and activated. Otherwise the
    /// accumulator is reset to 0 — in v1 the spring-back is a hard reset;
    /// the animated ease is wired up alongside the widget draw in a later
    /// task (spec §10).
    pub fn release_overscroll(&mut self, tree: &mut WorkspaceTree) {
        let committing = self.gesture_active && self.overscroll_progress() >= 1.0;
        let dir = self.overscroll_dir;
        // Always clear gesture state before mutating the tree — any early
        // return paths below still leave the wheel in a clean idle state.
        self.gesture_active = false;
        self.overscroll = 0.0;
        self.overscroll_dir = 0;
        if committing {
            Self::commit_new_workspace(tree, dir);
        }
    }

    /// Internal helper shared by the overscroll commit branch and T23's
    /// management-button entrypoint. Inserts a fresh, empty workspace
    /// (one seeded "Main" strip) and activates it. `dir >= 0` appends to
    /// the tail; `dir < 0` prepends to the head so an upward pull grows
    /// the list in the direction the user was gesturing.
    pub(crate) fn commit_new_workspace(tree: &mut WorkspaceTree, dir: i32) -> WorkspaceId {
        let ws_id = WorkspaceId::new();
        let strip_id = StripId::new();
        let mut ws = Workspace::new(ws_id, "Untitled");
        ws.strips.push(Strip::new(strip_id, "Main"));
        if dir < 0 {
            tree.workspaces.insert(0, ws);
        } else {
            tree.workspaces.push(ws);
        }
        tree.active_strip.insert(ws_id, strip_id);
        tree.active = ws_id;
        ws_id
    }
}

impl Default for SidebarWheel {
    fn default() -> Self {
        Self::new()
    }
}
