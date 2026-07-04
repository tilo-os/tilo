//! Pointer-driven selection gestures — the press → move → release state machine
//! behind click-to-place-caret, drag-to-select, double-click-word,
//! triple-click-line, and shift-extend. App-agnostic: it tracks the gesture and
//! reports *intents* (extend? what granularity?); the app maps points to its own
//! caret/selection model. Lives in `ui` so every app gets consistent selection
//! behaviour instead of re-deriving the timing/anchor rules.

use std::time::Instant;
use vello::kurbo::Point;

/// Selection granularity, from the click count.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Granularity {
    #[default]
    Char,
    Word,
    Line,
}

/// What a press should do to the selection.
#[derive(Clone, Copy, Debug)]
pub struct PressIntent {
    /// Shift held — keep the existing selection anchor and move only the caret.
    pub extend: bool,
    /// Char / Word / Line, from single / double / triple click.
    pub granularity: Granularity,
}

/// Two presses count as a multi-click within this gap and distance.
const MULTI_CLICK_MS: u128 = 450;
const MULTI_CLICK_DIST: f64 = 4.0;

/// Tracks a text-selection drag gesture across press → move → release, including
/// multi-click (word/line) and shift-extend.
#[derive(Default)]
pub struct DragSelect {
    dragging: bool,
    granularity: Granularity,
    last_click: Option<Instant>,
    last_pos: Point,
    click_count: u8,
}

impl DragSelect {
    pub fn new() -> Self {
        Self::default()
    }

    /// Begin a gesture at `pos`. Returns whether to extend the existing selection
    /// (shift held) and the granularity from the click count. A shift-press is
    /// never a multi-click — it always extends at char granularity.
    pub fn press(&mut self, pos: Point, shift: bool) -> PressIntent {
        let now = Instant::now();
        let multi = !shift
            && self.last_click.map(|t| now.duration_since(t).as_millis() < MULTI_CLICK_MS).unwrap_or(false)
            && self.last_pos.distance(pos) < MULTI_CLICK_DIST;
        self.click_count = if multi { (self.click_count + 1).min(3) } else { 1 };
        self.last_click = Some(now);
        self.last_pos = pos;
        self.dragging = true;
        self.granularity = match self.click_count {
            2 => Granularity::Word,
            n if n >= 3 => Granularity::Line,
            _ => Granularity::Char,
        };
        PressIntent { extend: shift, granularity: self.granularity }
    }

    /// While dragging, the granularity to extend the selection to the current
    /// pointer with (the app keeps the anchor and moves the caret). `None` when
    /// no drag is in progress.
    pub fn drag(&self) -> Option<Granularity> {
        self.dragging.then_some(self.granularity)
    }

    /// End the gesture. Returns whether a drag was actually in progress.
    pub fn release(&mut self) -> bool {
        let was = self.dragging;
        self.dragging = false;
        was
    }

    pub fn is_dragging(&self) -> bool {
        self.dragging
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn click_count_escalates_to_word_then_line() {
        let mut g = DragSelect::new();
        assert_eq!(g.press(Point::ZERO, false).granularity, Granularity::Char);
        assert_eq!(g.press(Point::ZERO, false).granularity, Granularity::Word);
        assert_eq!(g.press(Point::ZERO, false).granularity, Granularity::Line);
        // a far-away press resets to single
        assert_eq!(g.press(Point::new(500.0, 500.0), false).granularity, Granularity::Char);
    }

    #[test]
    fn shift_press_extends_and_is_never_a_multi_click() {
        let mut g = DragSelect::new();
        g.press(Point::ZERO, false);
        let p = g.press(Point::ZERO, true);
        assert!(p.extend, "shift extends");
        assert_eq!(p.granularity, Granularity::Char, "shift press is not a double-click");
    }

    #[test]
    fn drag_reflects_press_and_release() {
        let mut g = DragSelect::new();
        assert!(g.drag().is_none());
        g.press(Point::ZERO, false);
        assert_eq!(g.drag(), Some(Granularity::Char));
        assert!(g.release());
        assert!(g.drag().is_none());
        assert!(!g.release(), "second release reports no drag");
    }
}
