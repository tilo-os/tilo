//! Direct manipulation of the saved-place list: drag to reorder, swipe to delete,
//! and the per-place menu. Every structural gesture reaches one [`EntryAction`]
//! from two ways, so a new action is added once and shows in both.

use ui::icons::Icon;
use ui::tokens::Metrics;
use ui::widget::REORDER_THRESHOLD_PX;
use vello::kurbo::Point;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryAction {
    Refresh,
    MoveToTop,
    Delete,
}

impl EntryAction {
    pub fn label(self) -> &'static str {
        match self {
            EntryAction::Refresh => "Refresh forecast",
            EntryAction::MoveToTop => "Move to top",
            EntryAction::Delete => "Remove place",
        }
    }

    pub fn icon(self) -> Icon {
        match self {
            EntryAction::Refresh => Icon::Refresh,
            EntryAction::MoveToTop => Icon::ArrowUp,
            EntryAction::Delete => Icon::Trash2,
        }
    }

    /// Whether the action destroys something; the menu puts it last, in danger ink.
    pub fn is_destructive(self) -> bool {
        matches!(self, EntryAction::Delete)
    }

    pub fn menu_for_entry() -> [EntryAction; 3] {
        [
            EntryAction::Refresh,
            EntryAction::MoveToTop,
            EntryAction::Delete,
        ]
    }

    /// The action a left-swipe runs: the menu's destructive row, so the two cannot
    /// drift.
    pub fn swipe_left() -> EntryAction {
        Self::menu_for_entry()
            .into_iter()
            .find(|action| action.is_destructive())
            .unwrap_or(EntryAction::Delete)
    }
}

/// Leftward travel past which a swipe commits: three pointer targets, so a sloppy
/// click never deletes.
pub fn swipe_commit_dx(metrics: &Metrics) -> f64 {
    f64::from(metrics.min_pointer_target) * 3.0
}

#[derive(Clone, Copy, Debug)]
pub struct PointerPress {
    pub index: usize,
    pub origin: Point,
    pub current: Point,
    pub moved: bool,
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

    pub fn update(&mut self, at: Point) {
        self.current = at;
        let dx = at.x - self.origin.x;
        let dy = at.y - self.origin.y;
        if !self.moved && (dx * dx + dy * dy).sqrt() >= REORDER_THRESHOLD_PX {
            self.moved = true;
            self.vertical = Some(dy.abs() >= dx.abs());
        }
    }

    pub fn swipe_dx(&self) -> f64 {
        (self.origin.x - self.current.x).max(0.0)
    }

    pub fn is_swipe(&self) -> bool {
        self.moved && self.vertical == Some(false)
    }

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
        {
            let mut p = PointerPress::new(0, Point::new(100.0, 100.0));
            assert!(!p.moved);
            p.update(Point::new(80.0, 102.0));
            assert!(p.is_swipe());
            assert!(!p.is_reorder());

            let mut q = PointerPress::new(1, Point::new(100.0, 100.0));
            q.update(Point::new(101.0, 130.0));
            assert!(q.is_reorder());
            assert!(!q.is_swipe());
        }
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
