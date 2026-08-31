//! Pointer-driven selection gestures — the press → move → release state machine
//! behind click-to-place-caret, drag-to-select, double-click-word,
//! triple-click-line, and shift-extend. App-agnostic: it tracks the gesture and
//! reports *intents* (extend? what granularity?); the app maps points to its own
//! caret/selection model. Lives in `ui` so every app gets consistent selection
//! behaviour instead of re-deriving the timing/anchor rules.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};
use vello::kurbo::Point;

// Navigation gestures (back / forward): app-agnostic recognisers for the
// two ways a user asks to go back or forward, recognised once in the host
// `App` and delivered to the whole widget tree via `Widget::on_nav_gesture`.
//
//   * two-finger horizontal swipe on a touchpad (precise/pixel scroll)
//   * edge-pull swipe from a screen edge (the phone/tablet back gesture)

/// The direction a recognised navigation gesture asks for.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NavDirection {
    Back,
    Forward,
}

/// Phase of a continuous gesture, mapped from the windowing layer's
/// touch/scroll phases so this module stays winit-free and unit-testable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GesturePhase {
    Begin,
    Update,
    End,
}

/// Accumulated horizontal travel (physical px) that commits a two-finger
/// swipe to a navigation.
const TWO_FINGER_SWIPE_PX: f64 = 90.0;
/// A swipe must be this many times more horizontal than vertical to count
/// — so ordinary diagonal/vertical scrolling never navigates.
const SWIPE_HORIZONTAL_RATIO: f64 = 1.6;
/// A pause longer than this between scroll samples starts a fresh swipe,
/// so a missing `Begin`/`End` phase (some Wayland setups omit them) can't
/// let stale travel accumulate across unrelated gestures.
const SWIPE_IDLE_RESET_MS: u128 = 200;
/// A touch must start within this many px of a screen edge to arm an
/// edge-pull.
const EDGE_ZONE_PX: f64 = 24.0;
/// Inward travel (physical px) from the edge that commits an edge-pull.
const EDGE_PULL_PX: f64 = 56.0;

/// Default sign mapping: a positive horizontal scroll delta (as winit
/// reports it) is treated as a *back* swipe — matching the Firefox/Safari
/// touchpad convention. Flip this one constant if it feels inverted for a
/// given natural-scroll setting.
const POSITIVE_DX_IS_BACK: bool = true;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Edge {
    Left,
    Right,
}

/// One armed edge-pull touch, keyed by finger id.
struct EdgeTouch {
    start_x: f64,
    edge: Edge,
    fired: bool,
}

/// Recognises back/forward navigation gestures from touchpad swipes and
/// touchscreen edge pulls. One instance lives on the `App`; feed it every
/// scroll and touch event and act on the `Some(NavDirection)` it returns.
#[derive(Default)]
pub struct NavGestureRecognizer {
    swipe_x: f64,
    swipe_y: f64,
    swipe_latched: bool,
    last_scroll: Option<Instant>,
    touches: HashMap<u64, EdgeTouch>,
}

impl NavGestureRecognizer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a scroll sample. `precise` is `true` for pixel-precise
    /// touchpad scroll and `false` for a notched mouse wheel — only the
    /// former can be a two-finger swipe, so a horizontal wheel tilt never
    /// navigates. Returns a direction exactly once per gesture, when the
    /// accumulated horizontal travel crosses the threshold while staying
    /// dominantly horizontal.
    pub fn on_scroll(
        &mut self,
        dx: f64,
        dy: f64,
        phase: GesturePhase,
        precise: bool,
    ) -> Option<NavDirection> {
        if !precise {
            return None;
        }
        let now = Instant::now();
        let idle = self
            .last_scroll
            .is_none_or(|t| now.duration_since(t).as_millis() > SWIPE_IDLE_RESET_MS);
        self.last_scroll = Some(now);
        if phase == GesturePhase::Begin || idle {
            self.reset_swipe();
        }
        if phase == GesturePhase::End {
            self.reset_swipe();
            return None;
        }
        self.swipe_x += dx;
        self.swipe_y += dy;
        if self.swipe_latched {
            return None;
        }
        if self.swipe_x.abs() >= TWO_FINGER_SWIPE_PX
            && self.swipe_x.abs() >= SWIPE_HORIZONTAL_RATIO * self.swipe_y.abs()
        {
            self.swipe_latched = true;
            let rightward = self.swipe_x > 0.0;
            return Some(if rightward == POSITIVE_DX_IS_BACK {
                NavDirection::Back
            } else {
                NavDirection::Forward
            });
        }
        None
    }

    /// Feed a touch sample (physical coordinates + the window's physical
    /// width, for edge detection). A finger that starts within the edge
    /// zone and is dragged inward past the threshold navigates once: left
    /// edge → Back, right edge → Forward (the phone/tablet convention).
    pub fn on_touch(
        &mut self,
        id: u64,
        x: f64,
        _y: f64,
        phase: GesturePhase,
        window_width: f64,
    ) -> Option<NavDirection> {
        match phase {
            GesturePhase::Begin => {
                let edge = if x <= EDGE_ZONE_PX {
                    Some(Edge::Left)
                } else if x >= window_width - EDGE_ZONE_PX {
                    Some(Edge::Right)
                } else {
                    None
                };
                if let Some(edge) = edge {
                    self.touches.insert(
                        id,
                        EdgeTouch {
                            start_x: x,
                            edge,
                            fired: false,
                        },
                    );
                }
                None
            }
            GesturePhase::Update => {
                let t = self.touches.get_mut(&id)?;
                if t.fired {
                    return None;
                }
                let dx = x - t.start_x;
                let dir = match t.edge {
                    Edge::Left if dx >= EDGE_PULL_PX => Some(NavDirection::Back),
                    Edge::Right if dx <= -EDGE_PULL_PX => Some(NavDirection::Forward),
                    _ => None,
                };
                if dir.is_some() {
                    t.fired = true;
                }
                dir
            }
            GesturePhase::End => {
                self.touches.remove(&id);
                None
            }
        }
    }

    fn reset_swipe(&mut self) {
        self.swipe_x = 0.0;
        self.swipe_y = 0.0;
        self.swipe_latched = false;
    }
}

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

/// How far apart two presses may be and still count as one multi-click.
///
/// A pointer preference the session owns, not a per-widget constant. Five
/// widgets each carried their own copy of 450–500 ms, so the value could not be
/// changed for the accessibility setting it is, and a test could only race it:
/// a slow frame between two presses is indistinguishable from a slow user.
static MULTI_CLICK_MS: AtomicU64 = AtomicU64::new(450);

/// How far the pointer may travel between them.
const MULTI_CLICK_DIST: f64 = 4.0;

/// The current multi-click window.
pub fn multi_click_interval() -> Duration {
    Duration::from_millis(MULTI_CLICK_MS.load(Ordering::Relaxed))
}

/// Set the multi-click window for this session.
pub fn set_multi_click_interval(d: Duration) {
    // Clamped to `u64::MAX` just above, so this conversion never fails;
    // `try_from` spells that out instead of an unchecked `as`.
    let millis = u64::try_from(d.as_millis().min(u128::from(u64::MAX))).unwrap_or(u64::MAX);
    MULTI_CLICK_MS.store(millis, Ordering::Relaxed);
}

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
            && self
                .last_click
                .is_some_and(|t| now.duration_since(t) < multi_click_interval())
            && self.last_pos.distance(pos) < MULTI_CLICK_DIST;
        self.click_count = if multi {
            self.click_count.saturating_add(1).min(3)
        } else {
            1
        };
        self.last_click = Some(now);
        self.last_pos = pos;
        self.dragging = true;
        self.granularity = match self.click_count {
            2 => Granularity::Word,
            n if n >= 3 => Granularity::Line,
            _ => Granularity::Char,
        };
        PressIntent {
            extend: shift,
            granularity: self.granularity,
        }
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
        assert_eq!(
            g.press(Point::new(500.0, 500.0), false).granularity,
            Granularity::Char
        );
    }

    #[test]
    fn shift_press_extends_and_is_never_a_multi_click() {
        let mut g = DragSelect::new();
        g.press(Point::ZERO, false);
        let p = g.press(Point::ZERO, true);
        assert!(p.extend, "shift extends");
        assert_eq!(
            p.granularity,
            Granularity::Char,
            "shift press is not a double-click"
        );
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

    // ── navigation gestures ────────────────────────────────────────────

    /// Drive a two-finger swipe as a sequence of precise scroll samples
    /// within one gesture (single `Begin`), returning the first direction
    /// the recogniser commits to.
    fn swipe(
        recognizer: &mut NavGestureRecognizer,
        total_dx: f64,
        total_dy: f64,
    ) -> Option<NavDirection> {
        let steps = 6;
        let (mut fired, mut phase) = (None, GesturePhase::Begin);
        for i in 0..steps {
            let d = recognizer.on_scroll(
                total_dx / f64::from(steps),
                total_dy / f64::from(steps),
                phase,
                true,
            );
            fired = fired.or(d);
            phase = GesturePhase::Update;
            let _ = i;
        }
        fired
    }

    #[test]
    fn two_finger_swipe_navigates_by_direction() {
        let mut g = NavGestureRecognizer::new();
        assert_eq!(swipe(&mut g, 200.0, 5.0), Some(NavDirection::Back));
        let mut g2 = NavGestureRecognizer::new();
        assert_eq!(swipe(&mut g2, -200.0, 5.0), Some(NavDirection::Forward));
    }

    #[test]
    fn mostly_vertical_scroll_never_navigates() {
        let mut g = NavGestureRecognizer::new();
        assert_eq!(swipe(&mut g, 200.0, 400.0), None);
    }

    #[test]
    fn small_swipe_below_threshold_does_not_fire() {
        let mut g = NavGestureRecognizer::new();
        assert_eq!(swipe(&mut g, 40.0, 2.0), None);
    }

    #[test]
    fn swipe_fires_once_per_gesture() {
        let mut g = NavGestureRecognizer::new();
        assert_eq!(
            g.on_scroll(120.0, 0.0, GesturePhase::Begin, true),
            Some(NavDirection::Back)
        );
        // Continued travel in the same gesture must not re-fire.
        assert_eq!(g.on_scroll(120.0, 0.0, GesturePhase::Update, true), None);
    }

    #[test]
    fn notched_wheel_is_ignored() {
        let mut g = NavGestureRecognizer::new();
        assert_eq!(g.on_scroll(500.0, 0.0, GesturePhase::Update, false), None);
    }

    #[test]
    fn left_edge_pull_goes_back_right_edge_goes_forward() {
        let w = 1000.0;
        let mut g = NavGestureRecognizer::new();
        assert_eq!(g.on_touch(1, 10.0, 300.0, GesturePhase::Begin, w), None);
        assert_eq!(
            g.on_touch(1, 80.0, 305.0, GesturePhase::Update, w),
            Some(NavDirection::Back)
        );
        assert_eq!(g.on_touch(1, 900.0, 305.0, GesturePhase::End, w), None);

        assert_eq!(g.on_touch(2, 995.0, 300.0, GesturePhase::Begin, w), None);
        assert_eq!(
            g.on_touch(2, 900.0, 305.0, GesturePhase::Update, w),
            Some(NavDirection::Forward)
        );
    }

    #[test]
    fn touch_starting_mid_screen_never_navigates() {
        let mut g = NavGestureRecognizer::new();
        g.on_touch(1, 500.0, 300.0, GesturePhase::Begin, 1000.0);
        assert_eq!(
            g.on_touch(1, 800.0, 300.0, GesturePhase::Update, 1000.0),
            None
        );
    }

    #[test]
    fn edge_pull_fires_once() {
        let w = 1000.0;
        let mut g = NavGestureRecognizer::new();
        g.on_touch(1, 8.0, 300.0, GesturePhase::Begin, w);
        assert_eq!(
            g.on_touch(1, 80.0, 300.0, GesturePhase::Update, w),
            Some(NavDirection::Back)
        );
        assert_eq!(
            g.on_touch(1, 160.0, 300.0, GesturePhase::Update, w),
            None,
            "must not re-fire"
        );
    }
}
