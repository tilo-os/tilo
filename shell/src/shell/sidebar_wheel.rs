//! `SidebarWheel` — left-edge wheel of strip cards for the active workspace.
//!
//! Wheeling past the first/last strip accumulates pull in
//! [`SidebarWheel::overscroll`]; releasing past [`OVERSCROLL_THRESHOLD`]
//! commits a freshly-minted workspace (prepended for an upward pull,
//! appended for downward) and activates it, otherwise the accumulator snaps back to zero.
//!
//! State lives on the borrowed [`WorkspaceTree`]: `tree.active_strip` maps
//! each workspace to its active strip; the wheel advances the index by
//! `dir.signum()` clamped to the strip list's ends and writes it back.

use crate::shell::state::{Strip, StripId, Workspace, WorkspaceId, WorkspaceTree};

/// Pixel-pull at which a release commits a new workspace. Deliberately
/// generous — 240px is roughly one full mouse-wheel flick — so the commit
/// reads as intentional rather than an accidental flick past the edge.
pub const OVERSCROLL_THRESHOLD: f32 = 240.0;

/// Below this, a few px of bounce doesn't flash the "New session" hint.
const OVERSCROLL_HINT_DEADZONE: f32 = 32.0;

/// Authoritative workspace/strip state lives on the borrowed
/// [`WorkspaceTree`]; this owns only presentational scroll state and
/// the transient overscroll accumulator.
pub struct SidebarWheel {
    pub(crate) scroll_offset: f32,
    /// Drives the hint and the commit-threshold check in
    /// [`SidebarWheel::release_overscroll`].
    pub(crate) overscroll: f32,
    /// While true the wheel asks for pointer capture so stray motion
    /// outside the sidebar keeps feeding it.
    pub(crate) gesture_active: bool,
    /// `+1` bottom, `-1` top, `0` idle — lets `drag_overscroll` ignore
    /// motion that reverses mid-gesture.
    pub(crate) overscroll_dir: i32,
}

impl SidebarWheel {
    pub fn new() -> Self {
        Self {
            scroll_offset: 0.0,
            overscroll: 0.0,
            gesture_active: false,
            overscroll_dir: 0,
        }
    }

    /// A hard stop, not a wrap-around — the overscroll-create gesture
    /// layers on top of it.
    pub fn on_wheel(&mut self, dir: i32, tree: &mut WorkspaceTree) {
        if dir == 0 {
            return;
        }
        let step: i32 = if dir > 0 { 1 } else { -1 };
        let active_ws = tree.active;
        let Some(ws) = tree.workspaces.iter().find(|w| w.id == active_ws) else {
            return;
        };
        if ws.strips.is_empty() {
            return;
        }
        // Defaults to 0 for a freshly-created workspace.
        let current_idx = match tree.active_strip.get(&active_ws).copied() {
            Some(id) => ws.strips.iter().position(|s| s.id == id).unwrap_or(0),
            None => 0,
        };
        // `step` is always +-1 and `current_idx` a strip-list index (never
        // near i32::MAX), so this can't overflow; still spelled with
        // checked/saturating arithmetic rather than asserting that.
        let last_idx = ws.strips.len().saturating_sub(1);
        let next_idx = if step < 0 {
            current_idx.saturating_sub(1)
        } else {
            current_idx.saturating_add(1).min(last_idx)
        };
        let Some(next_id) = ws.strips.get(next_idx).map(|s| s.id) else {
            return;
        };
        tree.active_strip.insert(active_ws, next_id);
    }

    /// Always `0.0` until the widget-side animation is wired up.
    pub fn scroll_offset(&self) -> f32 {
        self.scroll_offset
    }

    /// `dir`: `> 0` bottom, `< 0` top, `0` no-op.
    pub fn begin_overscroll(&mut self, dir: i32) {
        if dir == 0 {
            return;
        }
        self.gesture_active = true;
        self.overscroll_dir = dir.signum();
        self.overscroll = 0.0;
    }

    /// Reverse motion (against `begin_overscroll`'s direction) is
    /// dropped so a flicky wheel can't both commit and snap back.
    pub fn drag_overscroll(&mut self, dir: i32, px: f32) {
        if !self.gesture_active {
            return;
        }
        if dir.signum() != self.overscroll_dir {
            return;
        }
        self.overscroll += px.abs();
    }

    pub fn overscroll_hint_visible(&self) -> bool {
        self.overscroll > OVERSCROLL_HINT_DEADZONE
    }

    /// Drives the hint card's alpha.
    pub fn overscroll_progress(&self) -> f32 {
        (self.overscroll / OVERSCROLL_THRESHOLD).clamp(0.0, 1.0)
    }

    pub fn wants_pointer_capture(&self) -> bool {
        self.gesture_active
    }

    /// If the pull crossed [`OVERSCROLL_THRESHOLD`], a new workspace is
    /// inserted and activated; otherwise the accumulator resets to 0.
    pub fn release_overscroll(&mut self, tree: &mut WorkspaceTree) {
        let committing = self.gesture_active && self.overscroll_progress() >= 1.0;
        let dir = self.overscroll_dir;
        // Cleared before mutating the tree, so the wheel is always left idle.
        self.gesture_active = false;
        self.overscroll = 0.0;
        self.overscroll_dir = 0;
        if committing {
            Self::commit_new_workspace(tree, dir);
        }
    }

    /// Creates and activates a fresh workspace without the overscroll
    /// gesture. Takes `&self` — no wheel-local state is touched, so
    /// it's invokable from any idle callsite.
    pub fn click_management_new_workspace(&self, tree: &mut WorkspaceTree) -> WorkspaceId {
        Self::commit_new_workspace(tree, 1)
    }

    /// `dir < 0` prepends, else appends — growing in the gesture's direction.
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
