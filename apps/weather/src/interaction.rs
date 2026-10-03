//! Direct-manipulation state for the saved-places list: drag-to-reorder,
//! swipe-to-delete, and the right-click context menu.
//!
//! The weather app is a single full-bleed widget that hit-tests its own
//! geometry (it has no child-widget tree), so these gestures are tracked
//! locally here rather than through the toolkit's cross-widget drag/drop
//! path. Each type is a small, self-contained state machine the app's
//! event handler advances and its `draw` pass reads.
//!
//! ## The "swipe ⇔ context-menu" parity principle
//! Every destructive/structural gesture is reachable two ways that stay in
//! lockstep: a swipe-left **and** a context-menu item both resolve to the
//! same [`EntryAction`]. New per-entry actions are added once to
//! [`EntryAction`] and surfaced in both places, so the two never drift.

use vello::kurbo::Point;

/// An action that can be performed on one saved place. The single source of
/// truth shared by the swipe gesture and the context menu — add a variant
/// here and wire it into both so they can never diverge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryAction {
    /// Remove the place (swipe-left, context menu, or the trash drop zone).
    Delete,
    /// Jump this place to the top of the list.
    MoveToTop,
    /// Re-fetch this place's forecast now.
    Refresh,
}

impl EntryAction {
    /// Human-facing label for the context menu.
    pub fn label(self) -> &'static str {
        match self {
            EntryAction::Delete => "Delete",
            EntryAction::MoveToTop => "Move to top",
            EntryAction::Refresh => "Refresh",
        }
    }

    /// Whether the action is destructive (rendered in the danger colour).
    pub fn is_destructive(self) -> bool {
        matches!(self, EntryAction::Delete)
    }

    /// The actions offered in the context menu, in display order. The
    /// **first** entry is also the action bound to swipe-left — keeping the
    /// gesture and the menu's primary item in sync by construction.
    pub fn menu_for_entry() -> [EntryAction; 3] {
        [
            EntryAction::Delete,
            EntryAction::MoveToTop,
            EntryAction::Refresh,
        ]
    }

    /// The action a left-swipe resolves to. Defined in terms of the menu so
    /// the parity is enforced in code, not just by convention.
    pub fn swipe_left() -> EntryAction {
        Self::menu_for_entry()[0]
    }
}

/// Pixels of pointer travel before a press on a chip is promoted to an
/// actual drag (so a plain click never jitters into a reorder).
pub const DRAG_THRESHOLD: f64 = 6.0;

/// Leftward travel (px) past which a horizontal gesture counts as a
/// commit-on-release swipe-to-delete.
pub const SWIPE_COMMIT_DX: f64 = 96.0;

/// A press on a saved-place chip that has not yet been classified. Tracks
/// the origin so we can tell a click from a vertical reorder-drag from a
/// horizontal delete-swipe once the pointer moves.
#[derive(Clone, Copy, Debug)]
pub struct PointerPress {
    /// Index of the chip the press landed on.
    pub index: usize,
    /// Where the press started (logical coords).
    pub origin: Point,
    /// Latest pointer position.
    pub current: Point,
    /// Has the press crossed [`DRAG_THRESHOLD`] yet?
    pub moved: bool,
    /// Once classified, whether this is a vertical reorder (`true`) or a
    /// horizontal swipe (`false`). `None` until the dominant axis is known.
    pub vertical: Option<bool>,
}

impl PointerPress {
    pub fn new(index: usize, at: Point) -> Self {
        Self {
            index,
            origin: at,
            current: at,
            moved: false,
            vertical: None,
        }
    }

    /// Update with the latest pointer position; classify the axis on the
    /// first move that clears the threshold.
    pub fn update(&mut self, at: Point) {
        self.current = at;
        let dx = at.x - self.origin.x;
        let dy = at.y - self.origin.y;
        if !self.moved && (dx * dx + dy * dy).sqrt() >= DRAG_THRESHOLD {
            self.moved = true;
            self.vertical = Some(dy.abs() >= dx.abs());
        }
    }

    /// Leftward swipe distance (positive = dragged left), 0 if dragging right.
    pub fn swipe_dx(&self) -> f64 {
        (self.origin.x - self.current.x).max(0.0)
    }

    /// True once this gesture is a horizontal swipe.
    pub fn is_swipe(&self) -> bool {
        self.moved && self.vertical == Some(false)
    }

    /// True once this gesture is a vertical reorder drag.
    pub fn is_reorder(&self) -> bool {
        self.moved && self.vertical == Some(true)
    }
}

/// An open per-entry context menu anchored at the cursor.
#[derive(Clone, Debug)]
pub struct ContextMenu {
    /// The saved-place index the menu acts on.
    pub index: usize,
    /// Top-left anchor (logical coords).
    pub anchor: Point,
    /// The actions shown, top to bottom.
    pub items: Vec<EntryAction>,
}

impl ContextMenu {
    pub fn for_entry(index: usize, anchor: Point) -> Self {
        Self {
            index,
            anchor,
            items: EntryAction::menu_for_entry().to_vec(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;

    #[test]
    fn gesture_classification_and_primary_actions_agree() {
        // Case `swipe_and_menu_primary_action_match`.
        {
            // The parity invariant: swipe-left and the menu's first item are the
            // same action. If someone reorders the menu, this guards the drift.
            assert_eq!(EntryAction::swipe_left(), EntryAction::menu_for_entry()[0]);
            assert_eq!(EntryAction::swipe_left(), EntryAction::Delete);
        }
        // Case `press_classifies_vertical_vs_horizontal`.
        {
            let mut p = PointerPress::new(0, Point::new(100.0, 100.0));
            assert!(!p.moved);
            // Mostly-horizontal drag → swipe.
            p.update(Point::new(80.0, 102.0));
            assert!(p.is_swipe());
            assert!(!p.is_reorder());

            let mut q = PointerPress::new(1, Point::new(100.0, 100.0));
            // Mostly-vertical drag → reorder.
            q.update(Point::new(101.0, 130.0));
            assert!(q.is_reorder());
            assert!(!q.is_swipe());
        }
        // Case `swipe_dx_is_leftward_only`.
        {
            let mut p = PointerPress::new(0, Point::new(100.0, 50.0));
            p.update(Point::new(40.0, 50.0)); // dragged left 60px
            assert!((p.swipe_dx() - 60.0).abs() < 1e-6);
            let mut q = PointerPress::new(0, Point::new(100.0, 50.0));
            q.update(Point::new(140.0, 50.0)); // dragged right → no swipe distance
            assert_eq!(q.swipe_dx(), 0.0);
        }
    }
}
