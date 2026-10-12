//! Direct-manipulation state for the saved-places list: drag-to-reorder,
//! swipe-to-delete, and the per-place menu.
//!
//! The city list is a painted pane that hit-tests its own rows, so the press
//! classification lives here; the reorder itself is the toolkit's
//! `DragController` and the menu the toolkit's `ContextMenu`.
//!
//! ## The "swipe ⇔ menu" parity principle
//! Every destructive/structural gesture is reachable two ways that stay in
//! lockstep: a swipe-left **and** a menu row both resolve to the same
//! [`EntryAction`]. New per-entry actions are added once to [`EntryAction`]
//! and surfaced in both places, so the two never drift.

use ui::icons::Icon;
use ui::tokens::Metrics;
use ui::widget::REORDER_THRESHOLD_PX;
use vello::kurbo::Point;

/// An action that can be performed on one saved place. The single source of
/// truth shared by the swipe gesture and the menu — add a variant here and
/// wire it into both so they can never diverge.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryAction {
    /// Re-fetch this place's forecast now.
    Refresh,
    /// Jump this place to the top of the list.
    MoveToTop,
    /// Remove the place (swipe-left or the menu).
    Delete,
}

impl EntryAction {
    /// The menu row's label: what happens.
    pub fn label(self) -> &'static str {
        match self {
            EntryAction::Refresh => "Refresh forecast",
            EntryAction::MoveToTop => "Move to top",
            EntryAction::Delete => "Remove place",
        }
    }

    /// The menu row's icon.
    pub fn icon(self) -> Icon {
        match self {
            EntryAction::Refresh => Icon::Refresh,
            EntryAction::MoveToTop => Icon::ArrowUp,
            EntryAction::Delete => Icon::Trash2,
        }
    }

    /// Whether the action is destructive (the menu letters it in danger
    /// ink, after a separator).
    pub fn is_destructive(self) -> bool {
        matches!(self, EntryAction::Delete)
    }

    /// The actions offered in the menu, in display order: the destructive
    /// one last.
    pub fn menu_for_entry() -> [EntryAction; 3] {
        [
            EntryAction::Refresh,
            EntryAction::MoveToTop,
            EntryAction::Delete,
        ]
    }

    /// The action a left-swipe resolves to: the menu's destructive row, so
    /// the parity is enforced in code, not just by convention.
    pub fn swipe_left() -> EntryAction {
        Self::menu_for_entry()
            .into_iter()
            .find(|action| action.is_destructive())
            .unwrap_or(EntryAction::Delete)
    }
}

/// Leftward travel past which a horizontal gesture commits the swipe on
/// release: three pointer targets, far enough that a sloppy click never
/// deletes.
pub fn swipe_commit_dx(metrics: &Metrics) -> f64 {
    f64::from(metrics.min_pointer_target) * 3.0
}

/// A press on a saved-place row that has not yet been classified. Tracks
/// the origin so we can tell a click from a vertical reorder-drag from a
/// horizontal delete-swipe once the pointer moves.
#[derive(Clone, Copy, Debug)]
pub struct PointerPress {
    /// Index of the row the press landed on.
    pub index: usize,
    /// Where the press started (logical coords).
    pub origin: Point,
    /// Latest pointer position.
    pub current: Point,
    /// Has the press crossed the toolkit's reorder threshold yet?
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
        if !self.moved && (dx * dx + dy * dy).sqrt() >= REORDER_THRESHOLD_PX {
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

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;

    #[test]
    fn gesture_classification_and_primary_actions_agree() {
        // Case `swipe_and_menu_destructive_action_match`.
        {
            // The parity invariant: swipe-left is the menu's destructive row.
            assert!(EntryAction::menu_for_entry().contains(&EntryAction::swipe_left()));
            assert!(EntryAction::swipe_left().is_destructive());
            assert_eq!(
                EntryAction::menu_for_entry().last(),
                Some(&EntryAction::Delete),
                "the destructive row comes last"
            );
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
