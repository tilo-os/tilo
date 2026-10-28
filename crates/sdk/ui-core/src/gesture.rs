//! Pointer-driven selection gestures — the press → move → release state machine
//! behind click-to-place-caret, drag-to-select, double-click-word,
//! triple-click-line, and shift-extend. App-agnostic: it tracks the gesture and
//! reports *intents* (extend? what granularity?); the app maps points to its own
//! caret/selection model. Lives in `ui` so every app gets consistent selection
//! behaviour instead of re-deriving the timing/anchor rules.

use crate::time::{Duration, Instant};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
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

/// Which [`NavDirection`]s a widget subtree would consume right now.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct NavAvailability {
    pub back: bool,
    pub forward: bool,
}

impl NavAvailability {
    #[must_use]
    pub fn or(self, other: Self) -> Self {
        Self {
            back: self.back || other.back,
            forward: self.forward || other.forward,
        }
    }
}

/// Phase of a continuous gesture, mapped from the windowing layer's
/// touch/scroll phases so this module stays winit-free and unit-testable.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GesturePhase {
    Begin,
    Update,
    End,
    /// The input source lost/canceled this contact. Unlike [`Self::End`],
    /// cancellation must never commit a terminal tap, swipe, or pinch.
    Cancel,
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

/// Device-independent output of the direct-touch gesture arena. Widgets use
/// this instead of interpreting raw contact streams, so a canvas, document or
/// game surface gets the same slop, pinch and swipe arbitration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TouchGestureEvent {
    Tap {
        position: Point,
    },
    Pan {
        fingers: u8,
        delta: (f64, f64),
    },
    Pinch {
        fingers: u8,
        center: Point,
        scale: f64,
    },
    Swipe {
        fingers: u8,
        direction: TouchSwipeDirection,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TouchSwipeDirection {
    Left,
    Right,
    Up,
    Down,
}

#[derive(Clone, Copy, Debug)]
struct Contact {
    start: Point,
    current: Point,
}

/// Resolves taps, pans, multi-finger swipes and pinches from raw direct-touch
/// contacts. It reports continuous pan/pinch updates and at most one terminal
/// tap or swipe. Long-press timing remains a host concern because it needs a
/// frame/timer wake even when no touch event arrives.
#[derive(Default)]
pub struct TouchGestureArena {
    contacts: HashMap<u64, Contact>,
    start_center: Option<Point>,
    previous_center: Option<Point>,
    start_span: Option<f64>,
    previous_scale: f64,
    max_fingers: u8,
    moved: bool,
}

impl TouchGestureArena {
    /// Travel a finger may make and still be a tap.
    pub const TAP_SLOP: f64 = 14.0;
    const SWIPE_DISTANCE: f64 = 56.0;
    const AXIS_DOMINANCE: f64 = 1.4;
    const PINCH_SLOP: f64 = 0.04;

    pub fn new() -> Self {
        Self::default()
    }

    /// Once a third contact joins, the whole stream remains shell-owned
    /// through every release; ownership must not flicker back to an app when
    /// the contact count falls from three to two.
    pub fn shell_owns_current(&self) -> bool {
        self.max_fingers >= 3 && !self.contacts.is_empty()
    }

    /// Drop every direct-touch contact without producing a terminal gesture.
    /// Used when the compositor loses the input source or transfers the whole
    /// seat to the lock screen.
    pub fn cancel_all(&mut self) {
        self.contacts.clear();
        self.reset();
    }

    pub fn on_touch(
        &mut self,
        id: u64,
        position: Point,
        phase: GesturePhase,
    ) -> Vec<TouchGestureEvent> {
        match phase {
            GesturePhase::Begin => {
                self.contacts.insert(
                    id,
                    Contact {
                        start: position,
                        current: position,
                    },
                );
                self.max_fingers = self.max_fingers.max(self.finger_count());
                let center = self.center();
                self.start_center = center;
                self.previous_center = center;
                self.start_span = self.span();
                self.previous_scale = 1.0;
                Vec::new()
            }
            GesturePhase::Update => {
                let Some(contact) = self.contacts.get_mut(&id) else {
                    return Vec::new();
                };
                contact.current = position;
                let Some(center) = self.center() else {
                    return Vec::new();
                };
                let previous = self.previous_center.replace(center).unwrap_or(center);
                let delta = (center.x - previous.x, center.y - previous.y);
                if self
                    .contacts
                    .values()
                    .any(|contact| contact.current.distance(contact.start) > Self::TAP_SLOP)
                {
                    self.moved = true;
                }
                if let (Some(start_span), Some(span)) = (self.start_span, self.span())
                    && start_span > 0.0
                {
                    let scale = span / start_span;
                    let translation = self
                        .start_center
                        .map_or(0.0, |start| start.distance(center));
                    let radial_motion = (span - start_span).abs();
                    if ((scale - 1.0).abs() >= Self::PINCH_SLOP
                        && radial_motion >= translation * 0.7)
                        || (self.previous_scale - 1.0).abs() >= Self::PINCH_SLOP
                    {
                        self.previous_scale = scale;
                        return vec![TouchGestureEvent::Pinch {
                            fingers: self.finger_count(),
                            center,
                            scale,
                        }];
                    }
                }
                if delta.0 != 0.0 || delta.1 != 0.0 {
                    vec![TouchGestureEvent::Pan {
                        fingers: self.finger_count(),
                        delta,
                    }]
                } else {
                    Vec::new()
                }
            }
            GesturePhase::End => {
                if let Some(contact) = self.contacts.get_mut(&id) {
                    contact.current = position;
                }
                let terminal = if self.contacts.len() == 1 {
                    self.terminal_event(position)
                } else {
                    None
                };
                self.contacts.remove(&id);
                if self.contacts.is_empty() {
                    self.reset();
                }
                terminal.into_iter().collect()
            }
            GesturePhase::Cancel => {
                self.contacts.remove(&id);
                if self.contacts.is_empty() {
                    self.reset();
                }
                Vec::new()
            }
        }
    }

    fn finger_count(&self) -> u8 {
        u8::try_from(self.contacts.len()).unwrap_or(u8::MAX)
    }

    /// Contacts currently down.
    pub fn contact_count(&self) -> usize {
        self.contacts.len()
    }

    /// Centroid of the contacts currently down.
    pub fn center(&self) -> Option<Point> {
        if self.contacts.is_empty() {
            return None;
        }
        let (x, y) = self.contacts.values().fold((0.0, 0.0), |sum, contact| {
            (sum.0 + contact.current.x, sum.1 + contact.current.y)
        });
        let count = f64::from(self.finger_count());
        Some(Point::new(x / count, y / count))
    }

    fn span(&self) -> Option<f64> {
        let center = self.center()?;
        (self.contacts.len() >= 2).then(|| {
            self.contacts
                .values()
                .map(|contact| contact.current.distance(center))
                .sum::<f64>()
                / f64::from(self.finger_count())
        })
    }

    fn terminal_event(&self, position: Point) -> Option<TouchGestureEvent> {
        let start = self.start_center?;
        let end = self.center().unwrap_or(position);
        let dx = end.x - start.x;
        let dy = end.y - start.y;
        if !self.moved && dx.hypot(dy) <= Self::TAP_SLOP {
            return Some(TouchGestureEvent::Tap { position });
        }
        let (ax, ay) = (dx.abs(), dy.abs());
        let direction = if ax >= Self::SWIPE_DISTANCE && ax >= ay * Self::AXIS_DOMINANCE {
            if dx < 0.0 {
                TouchSwipeDirection::Left
            } else {
                TouchSwipeDirection::Right
            }
        } else if ay >= Self::SWIPE_DISTANCE && ay >= ax * Self::AXIS_DOMINANCE {
            if dy < 0.0 {
                TouchSwipeDirection::Up
            } else {
                TouchSwipeDirection::Down
            }
        } else {
            return None;
        };
        Some(TouchGestureEvent::Swipe {
            fingers: self.max_fingers.max(1),
            direction,
        })
    }

    fn reset(&mut self) {
        self.start_center = None;
        self.previous_center = None;
        self.start_span = None;
        self.previous_scale = 1.0;
        self.max_fingers = 0;
        self.moved = false;
    }
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
        let now = crate::time::now();
        let idle = self
            .last_scroll
            .is_none_or(|t| now.duration_since(t).as_millis() > SWIPE_IDLE_RESET_MS);
        self.last_scroll = Some(now);
        if phase == GesturePhase::Begin || idle {
            self.reset_swipe();
        }
        if matches!(phase, GesturePhase::End | GesturePhase::Cancel) {
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
            GesturePhase::End | GesturePhase::Cancel => {
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

std::thread_local! {
    // A capture may need two presses to survive slow rendering, without
    // changing another capture's timing or the session preference.
    static CAPTURE_MULTI_CLICK_INTERVAL: std::cell::Cell<Option<Duration>> = const {
        std::cell::Cell::new(None)
    };
}

/// How far the pointer may travel between them.
const MULTI_CLICK_DIST: f64 = 4.0;

/// The current multi-click window.
pub fn multi_click_interval() -> Duration {
    CAPTURE_MULTI_CLICK_INTERVAL
        .get()
        .unwrap_or_else(|| Duration::from_millis(MULTI_CLICK_MS.load(Ordering::Relaxed)))
}

/// Override capture timing synchronously on this thread, restoring it even
/// when a frame panics. Ordinary hosts keep the session preference.
pub(crate) fn with_capture_multi_click_interval<T>(
    interval: Duration,
    capture: impl FnOnce() -> T,
) -> T {
    struct Restore(Option<Duration>);
    impl Drop for Restore {
        fn drop(&mut self) {
            CAPTURE_MULTI_CLICK_INTERVAL.set(self.0);
        }
    }
    let _restore = Restore(CAPTURE_MULTI_CLICK_INTERVAL.replace(Some(interval)));
    capture()
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
        let now = crate::time::now();
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
    fn parallel_captures_keep_their_own_multi_click_gestures_and_session_preference() {
        let session_interval = multi_click_interval();
        let ready = std::sync::Barrier::new(3);
        let done = std::sync::Barrier::new(3);
        std::thread::scope(|scope| {
            let capture = |interval| {
                with_capture_multi_click_interval(interval, || {
                    ready.wait();
                    let mut gesture = DragSelect {
                        last_click: Some(crate::time::now()),
                        last_pos: Point::ZERO,
                        click_count: 1,
                        ..DragSelect::default()
                    };
                    let granularity = gesture.press(Point::ZERO, false).granularity;
                    done.wait();
                    granularity
                })
            };
            let long = scope.spawn(move || capture(Duration::from_secs(3600)));
            let short = scope.spawn(move || capture(Duration::ZERO));
            ready.wait();
            let observed_session_interval = multi_click_interval();
            done.wait();
            assert_eq!(long.join().unwrap(), Granularity::Word);
            assert_eq!(short.join().unwrap(), Granularity::Char);
            assert_eq!(observed_session_interval, session_interval);
        });
        assert_eq!(multi_click_interval(), session_interval);
    }

    #[test]
    fn capture_multi_click_timing_restores_nested_and_panicking_frames() {
        let session_interval = multi_click_interval();
        with_capture_multi_click_interval(Duration::from_secs(3600), || {
            let result = std::panic::catch_unwind(|| {
                with_capture_multi_click_interval(Duration::ZERO, || {
                    assert_eq!(multi_click_interval(), Duration::ZERO);
                    panic!("capture frame failed");
                });
            });
            assert!(result.is_err());
            assert_eq!(multi_click_interval(), Duration::from_secs(3600));
        });
        assert_eq!(multi_click_interval(), session_interval);
    }

    #[test]
    fn drag_select_escalates_clicks_extends_with_shift_and_tracks_the_drag() {
        // Click count escalates to word then line; a far-away press resets.
        let mut g = DragSelect::new();
        assert_eq!(g.press(Point::ZERO, false).granularity, Granularity::Char);
        assert_eq!(g.press(Point::ZERO, false).granularity, Granularity::Word);
        assert_eq!(g.press(Point::ZERO, false).granularity, Granularity::Line);
        assert_eq!(
            g.press(Point::new(500.0, 500.0), false).granularity,
            Granularity::Char
        );

        // A shift press extends and is never a multi-click.
        let mut g = DragSelect::new();
        g.press(Point::ZERO, false);
        let p = g.press(Point::ZERO, true);
        assert!(p.extend, "shift extends");
        assert_eq!(
            p.granularity,
            Granularity::Char,
            "shift press is not a double-click"
        );

        let mut g = DragSelect::new();
        assert!(g.drag().is_none());
        g.press(Point::ZERO, false);
        assert_eq!(g.drag(), Some(Granularity::Char));
        assert!(g.release());
        assert!(g.drag().is_none());
        assert!(!g.release(), "second release reports no drag");
    }

    #[test]
    fn touch_arena_arbitrates_taps_pinches_swipes_and_shell_streams() {
        // A direct-touch tap is delayed until contact ends.
        let mut arena = TouchGestureArena::new();
        assert!(
            arena
                .on_touch(1, Point::new(20.0, 30.0), GesturePhase::Begin)
                .is_empty()
        );
        assert_eq!(
            arena.on_touch(1, Point::new(21.0, 30.0), GesturePhase::End),
            [TouchGestureEvent::Tap {
                position: Point::new(21.0, 30.0)
            }]
        );

        let mut arena = TouchGestureArena::new();
        arena.on_touch(1, Point::new(40.0, 50.0), GesturePhase::Begin);
        arena.on_touch(2, Point::new(60.0, 50.0), GesturePhase::Begin);
        let events = arena.on_touch(2, Point::new(80.0, 50.0), GesturePhase::Update);
        assert!(matches!(
            events.as_slice(),
            [TouchGestureEvent::Pinch { scale, .. }] if *scale > 1.5
        ));

        let mut arena = TouchGestureArena::new();
        arena.on_touch(1, Point::new(100.0, 80.0), GesturePhase::Begin);
        arena.on_touch(2, Point::new(100.0, 120.0), GesturePhase::Begin);
        arena.on_touch(1, Point::new(20.0, 80.0), GesturePhase::Update);
        arena.on_touch(2, Point::new(20.0, 120.0), GesturePhase::Update);
        assert!(
            arena
                .on_touch(1, Point::new(20.0, 80.0), GesturePhase::End)
                .is_empty()
        );
        assert_eq!(
            arena.on_touch(2, Point::new(20.0, 120.0), GesturePhase::End),
            [TouchGestureEvent::Swipe {
                fingers: 2,
                direction: TouchSwipeDirection::Left
            }]
        );

        // A three-finger stream stays shell-owned while contacts lift.
        let mut arena = TouchGestureArena::new();
        for (id, x) in [(1, 20.0), (2, 40.0), (3, 60.0)] {
            arena.on_touch(id, Point::new(x, 20.0), GesturePhase::Begin);
        }
        assert!(arena.shell_owns_current());
        for (id, x, still_owned) in [(1, 20.0, true), (2, 40.0, true), (3, 60.0, false)] {
            arena.on_touch(id, Point::new(x, 20.0), GesturePhase::End);
            assert_eq!(
                arena.shell_owns_current(),
                still_owned,
                "after lifting {id}"
            );
        }

        // Cancelling a three-finger stream never emits a terminal shell swipe.
        let mut arena = TouchGestureArena::new();
        for (id, x) in [(1, 100.0), (2, 140.0), (3, 180.0)] {
            arena.on_touch(id, Point::new(x, 80.0), GesturePhase::Begin);
        }
        for (id, x) in [(1, 20.0), (2, 60.0), (3, 100.0)] {
            arena.on_touch(id, Point::new(x, 80.0), GesturePhase::Update);
        }
        for (id, x) in [(1, 20.0), (2, 60.0), (3, 100.0)] {
            assert!(
                arena
                    .on_touch(id, Point::new(x, 80.0), GesturePhase::Cancel)
                    .is_empty(),
                "cancellation is disposal, never gesture completion"
            );
        }
        assert!(!arena.shell_owns_current());
    }

    // ── navigation gestures ────────────────────────────────────────────

    /// Drive a two-finger swipe as a sequence of precise scroll samples
    /// within one gesture (single `Begin`), returning the first direction
    /// the recogniser commits to.
    #[allow(
        clippy::similar_names,
        reason = "total_dx/total_dy is the standard delta-x/delta-y naming convention; \
                  distinguishing them further would reduce clarity"
    )]
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
    fn trackpad_swipe_navigates_by_direction_once_above_threshold() {
        for (case, dx, dy, want) in [
            (
                "rightward swipe goes back",
                200.0,
                5.0,
                Some(NavDirection::Back),
            ),
            (
                "leftward swipe goes forward",
                -200.0,
                5.0,
                Some(NavDirection::Forward),
            ),
            ("mostly vertical scroll never navigates", 200.0, 400.0, None),
            ("small swipe below threshold does not fire", 40.0, 2.0, None),
        ] {
            let mut g = NavGestureRecognizer::new();
            assert_eq!(swipe(&mut g, dx, dy), want, "case {case}");
        }

        // A swipe fires once per gesture: continued travel must not re-fire.
        let mut g = NavGestureRecognizer::new();
        assert_eq!(
            g.on_scroll(120.0, 0.0, GesturePhase::Begin, true),
            Some(NavDirection::Back)
        );
        assert_eq!(g.on_scroll(120.0, 0.0, GesturePhase::Update, true), None);

        let mut g = NavGestureRecognizer::new();
        assert_eq!(g.on_scroll(500.0, 0.0, GesturePhase::Update, false), None);
    }

    #[test]
    fn edge_pull_navigates_once_and_only_from_a_screen_edge() {
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

        // A touch starting mid-screen never navigates.
        let mut g = NavGestureRecognizer::new();
        g.on_touch(1, 500.0, 300.0, GesturePhase::Begin, 1000.0);
        assert_eq!(
            g.on_touch(1, 800.0, 300.0, GesturePhase::Update, 1000.0),
            None
        );

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
