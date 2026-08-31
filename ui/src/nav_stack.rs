//! A reusable, app-agnostic navigation stack with a back action and an
//! edge-swipe back-gesture hook.
//!
//! Apps that model UI as a series of views need three things the toolkit can
//! own once: a history with push/pop ([`NavStack<T>`]), a single back intent
//! ([`NavAction`]) that gesture/key/button all resolve to, and a gesture
//! entry point ([`EdgeSwipeBack`]) so an edge-swipe drives the same action.
//!
//! The stack stores cloneable descriptors, never widgets — pure state,
//! unit-tested without a render pass.

use vello::kurbo::Point;

/// The one navigation intent a back-gesture / Escape / Alt-Left / a Back button
/// all resolve to. Kept as an enum (rather than a bare "go back") so future
/// intents (forward, home, jump-to-root) slot in without changing call sites.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavAction {
    Back,
}

/// A generic navigation history. `T` names a view (an app's route/screen
/// descriptor); the stack owns the ordered history with the current view last.
///
/// `root` is split out from `rest` (rather than one `Vec` with a
/// never-empty invariant) so "the stack is never empty" is a fact of the
/// type, not a runtime check [`NavStack::current`] would otherwise have to
/// assert.
#[derive(Clone, Debug)]
pub struct NavStack<T> {
    root: T,
    rest: Vec<T>,
}

impl<T: Clone + PartialEq> NavStack<T> {
    /// A stack rooted at `root` (the first, un-poppable view).
    pub fn new(root: T) -> Self {
        Self {
            root,
            rest: Vec::new(),
        }
    }

    /// The view currently on screen (the top of the stack).
    pub fn current(&self) -> &T {
        self.rest.last().unwrap_or(&self.root)
    }

    /// Push `view` as the new current view. A push equal to the current view is
    /// a no-op (navigating to where you already are does not grow history), so
    /// repeated identical navigations don't stack up and Back stays meaningful.
    pub fn push(&mut self, view: T) {
        if self.current() != &view {
            self.rest.push(view);
        }
    }

    /// Replace the current view in place without growing history. Use when a
    /// view mutates into a variant of itself (e.g. a query refining) that a
    /// Back should skip over rather than replay.
    pub fn replace_current(&mut self, view: T) {
        if let Some(last) = self.rest.last_mut() {
            *last = view;
        } else {
            self.root = view;
        }
    }

    pub fn can_go_back(&self) -> bool {
        !self.rest.is_empty()
    }

    /// Pop the current view and return a reference to the view now on screen,
    /// or `None` when already at the root (nothing to pop).
    pub fn back(&mut self) -> Option<&T> {
        if self.rest.is_empty() {
            None
        } else {
            self.rest.pop();
            Some(self.current())
        }
    }

    /// Apply a [`NavAction`], returning the view now on screen if it changed.
    pub fn apply(&mut self, action: NavAction) -> Option<&T> {
        match action {
            NavAction::Back => self.back(),
        }
    }

    /// Number of views in the history (>= 1).
    pub fn depth(&self) -> usize {
        // `root` plus whatever `rest` holds; bounded by however many views
        // an app ever pushed, nowhere near `usize::MAX`.
        self.rest.len().saturating_add(1)
    }

    pub fn reset(&mut self, root: T) {
        self.rest.clear();
        self.root = root;
    }
}

/// Arms when a press starts within `edge_width` of the left edge, fires
/// [`NavAction::Back`] once the pointer travels `trigger_distance` px right
/// in a mostly-horizontal drag. No velocity/animation, but a complete
/// pointer-event contract a richer recognizer can later drop into.
#[derive(Clone, Debug)]
pub struct EdgeSwipeBack {
    edge_width: f64,
    trigger_distance: f64,
    /// The armed press origin (within the edge zone), or `None` when idle.
    origin: Option<Point>,
    /// Latched once a swipe fires, so a single drag emits Back exactly once.
    fired: bool,
}

impl EdgeSwipeBack {
    /// A recognizer for a left-edge back swipe. `edge_width` is how far from the
    /// left edge a press must start to arm; `trigger_distance` is the rightward
    /// travel that fires Back.
    pub fn new(edge_width: f64, trigger_distance: f64) -> Self {
        Self {
            edge_width: edge_width.max(0.0),
            trigger_distance: trigger_distance.max(1.0),
            origin: None,
            fired: false,
        }
    }

    /// A sensible default: arm within 24px of the left edge, fire after 80px of
    /// rightward travel.
    pub fn default_back() -> Self {
        Self::new(24.0, 80.0)
    }

    /// A pointer press landed at `p` (logical coords, relative to the surface's
    /// left edge). Arms the recognizer when the press is inside the edge zone.
    pub fn pointer_down(&mut self, p: Point) {
        self.fired = false;
        self.origin = (p.x <= self.edge_width).then_some(p);
    }

    /// The pointer moved to `p`. Returns [`NavAction::Back`] exactly once, on the
    /// frame the horizontal travel first exceeds the trigger distance while the
    /// gesture stays predominantly horizontal. `None` otherwise.
    pub fn pointer_move(&mut self, p: Point) -> Option<NavAction> {
        if self.fired {
            return None;
        }
        let origin = self.origin?;
        let dx = p.x - origin.x;
        let dy = (p.y - origin.y).abs();
        if dx >= self.trigger_distance && dx > dy {
            self.fired = true;
            return Some(NavAction::Back);
        }
        None
    }

    /// The pointer was released / capture ended — disarm for the next gesture.
    pub fn pointer_up(&mut self) {
        self.origin = None;
        self.fired = false;
    }

    /// Whether a swipe is currently armed (a press started in the edge zone and
    /// has not yet fired or been released).
    pub fn is_armed(&self) -> bool {
        self.origin.is_some() && !self.fired
    }
}

impl Default for EdgeSwipeBack {
    fn default() -> Self {
        Self::default_back()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_is_current_and_not_poppable() {
        let mut s = NavStack::new("home");
        assert_eq!(*s.current(), "home");
        assert!(!s.can_go_back());
        assert_eq!(s.back(), None, "root does not pop");
        assert_eq!(*s.current(), "home");
        assert_eq!(s.depth(), 1);
    }

    #[test]
    fn push_then_back_returns_to_previous() {
        let mut s = NavStack::new("home");
        s.push("results");
        s.push("watch");
        assert_eq!(*s.current(), "watch");
        assert!(s.can_go_back());
        assert_eq!(s.back(), Some(&"results"), "back pops watch → results");
        assert_eq!(*s.current(), "results");
        assert_eq!(s.back(), Some(&"home"), "back again → home");
        assert!(!s.can_go_back());
        assert_eq!(s.back(), None, "cannot pop past root");
    }

    #[test]
    fn duplicate_push_is_a_noop() {
        let mut s = NavStack::new("home");
        s.push("home");
        assert_eq!(
            s.depth(),
            1,
            "pushing the current view does not grow history"
        );
        s.push("results");
        s.push("results");
        assert_eq!(
            s.depth(),
            2,
            "a repeated push of the same new view is ignored"
        );
    }

    #[test]
    fn replace_current_does_not_grow_history() {
        let mut s = NavStack::new("home");
        s.push("search:a");
        s.replace_current("search:ab");
        assert_eq!(*s.current(), "search:ab");
        assert_eq!(s.depth(), 2);
        assert_eq!(s.back(), Some(&"home"), "back skips over the replaced view");
    }

    #[test]
    fn apply_back_action_pops() {
        let mut s = NavStack::new(1);
        s.push(2);
        assert_eq!(s.apply(NavAction::Back), Some(&1));
        assert_eq!(s.apply(NavAction::Back), None);
    }

    #[test]
    fn edge_swipe_fires_back_once_on_rightward_drag_from_edge() {
        let mut g = EdgeSwipeBack::new(24.0, 80.0);
        g.pointer_down(Point::new(8.0, 400.0));
        assert!(g.is_armed());
        assert_eq!(
            g.pointer_move(Point::new(40.0, 402.0)),
            None,
            "short drag: not yet"
        );
        assert_eq!(
            g.pointer_move(Point::new(100.0, 404.0)),
            Some(NavAction::Back),
            "crossing the threshold fires Back",
        );
        assert_eq!(
            g.pointer_move(Point::new(160.0, 404.0)),
            None,
            "Back fires exactly once per gesture",
        );
        g.pointer_up();
        assert!(!g.is_armed());
    }

    #[test]
    fn edge_swipe_ignores_presses_away_from_edge() {
        let mut g = EdgeSwipeBack::new(24.0, 80.0);
        g.pointer_down(Point::new(300.0, 400.0));
        assert!(!g.is_armed());
        assert_eq!(
            g.pointer_move(Point::new(500.0, 400.0)),
            None,
            "no arm → no fire"
        );
    }

    #[test]
    fn edge_swipe_ignores_mostly_vertical_drag() {
        let mut g = EdgeSwipeBack::new(24.0, 80.0);
        g.pointer_down(Point::new(6.0, 100.0));
        // Sub-threshold horizontal with a large vertical: a scroll.
        assert_eq!(g.pointer_move(Point::new(50.0, 400.0)), None);
        // Past the horizontal threshold too, but vertical delta is bigger.
        assert_eq!(g.pointer_move(Point::new(90.0, 500.0)), None);
    }
}
