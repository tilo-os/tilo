//! Explore by touch: what a finger means while the built-in screen reader is
//! on, as TalkBack and VoiceOver read it.
//!
//! - A finger resting or dragging on the screen explores: the item under it
//!   is spoken and the reading cursor moves there, and nothing is pressed.
//!   Lifting the finger leaves the cursor where it is.
//! - A quick tap explores the item under it too, once it is clear no second
//!   tap follows.
//! - A double tap anywhere activates the item the cursor is on.
//! - A quick one-finger swipe right or left moves to the next or previous
//!   item.
//!
//! A second finger ends the gesture: nothing more is read from the contacts
//! until every finger has lifted.
//!
//! One recognizer for every host. It is pure: the host passes each contact
//! in the window's logical pixels with its time, arms a timer for
//! [`TouchExplorer::deadline`] and calls [`TouchExplorer::poll`] when it
//! fires. A finger that has not yet moved past [`TAP_SLOP`] is not read
//! until [`EXPLORE_DELAY`] has passed, so the start of a swipe or the first
//! tap of a double tap never moves the cursor.

use std::collections::HashSet;

use vello::kurbo::Point;
use winit::event::TouchPhase;

use crate::time::{Duration, Instant};

/// How far, in logical pixels, a finger travels before it is moving rather
/// than resting.
pub const TAP_SLOP: f64 = 12.0;
/// How long a resting finger waits before it explores, so a tap or the
/// start of a swipe reads nothing.
pub const EXPLORE_DELAY: Duration = Duration::from_millis(200);
/// The longest a swipe takes from touch to lift; a slower movement explores.
pub const SWIPE_MAX: Duration = Duration::from_millis(500);
/// The shortest horizontal distance, in logical pixels, a swipe covers.
pub const SWIPE_MIN: f64 = 48.0;
/// The longest wait between a tap's lift and the second tap's touch.
pub const DOUBLE_TAP_TIMEOUT: Duration = Duration::from_millis(300);
/// How far, in logical pixels, the second tap of a double tap may land from
/// the first.
pub const DOUBLE_TAP_SLOP: f64 = 96.0;

/// What a finger asked the screen reader for.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ExploreGesture {
    /// Speak the item under this point and move the cursor there.
    Explore(Point),
    /// Activate the item the cursor is on.
    Activate,
    /// Move to the next item in reading order.
    Next,
    /// Move to the previous item in reading order.
    Previous,
}

/// What the finger on the screen is doing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Contact {
    /// Down and still within [`TAP_SLOP`], before [`EXPLORE_DELAY`].
    Resting,
    /// Moved past the slop before exploring began: a swipe, if it lifts in
    /// time.
    Swiping,
    /// Reading what is under it.
    Exploring,
}

#[derive(Clone, Copy, Debug)]
struct Finger {
    id: u64,
    start: Point,
    at: Point,
    down: Instant,
    contact: Contact,
    /// Touched soon after, and close to, a tap: a lift in place activates.
    second_tap: bool,
}

/// Reads one-finger touches as the screen reader's gestures.
#[derive(Debug, Default)]
pub struct TouchExplorer {
    finger: Option<Finger>,
    /// A tap that lifted, waiting to see whether a second one follows.
    tap: Option<(Point, Instant)>,
    /// Every contact on the screen.
    down: HashSet<u64>,
    /// A second finger touched: nothing is read until all have lifted.
    ignoring: bool,
}

impl TouchExplorer {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Read one contact update, `at` in the window's logical pixels.
    pub fn touch(
        &mut self,
        id: u64,
        phase: TouchPhase,
        at: Point,
        now: Instant,
    ) -> Vec<ExploreGesture> {
        let mut out = self.poll(now);
        match phase {
            TouchPhase::Started => self.started(id, at, now),
            TouchPhase::Moved => out.extend(self.moved(id, at, now)),
            TouchPhase::Ended => out.extend(self.ended(id, at, now)),
            TouchPhase::Cancelled => self.cancelled(id),
        }
        out
    }

    fn started(&mut self, id: u64, at: Point, now: Instant) {
        self.down.insert(id);
        if self.down.len() > 1 {
            // A second finger: no one-finger gesture is under way any more.
            self.ignoring = true;
            self.finger = None;
            self.tap = None;
            return;
        }
        if self.ignoring {
            return;
        }
        let second_tap = self.tap.take().is_some_and(|(first, lifted)| {
            now.saturating_duration_since(lifted) <= DOUBLE_TAP_TIMEOUT
                && first.distance(at) <= DOUBLE_TAP_SLOP
        });
        self.finger = Some(Finger {
            id,
            start: at,
            at,
            down: now,
            contact: Contact::Resting,
            second_tap,
        });
    }

    fn moved(&mut self, id: u64, at: Point, now: Instant) -> Option<ExploreGesture> {
        let finger = self.finger.as_mut().filter(|finger| finger.id == id)?;
        finger.at = at;
        match finger.contact {
            Contact::Resting if finger.start.distance(at) > TAP_SLOP => {
                finger.second_tap = false;
                if now.saturating_duration_since(finger.down) < EXPLORE_DELAY {
                    finger.contact = Contact::Swiping;
                    None
                } else {
                    finger.contact = Contact::Exploring;
                    Some(ExploreGesture::Explore(at))
                }
            }
            Contact::Exploring => Some(ExploreGesture::Explore(at)),
            Contact::Resting | Contact::Swiping => None,
        }
    }

    fn ended(&mut self, id: u64, at: Point, now: Instant) -> Option<ExploreGesture> {
        self.down.remove(&id);
        if self.down.is_empty() {
            self.ignoring = false;
        }
        let finger = self.finger.take_if(|finger| finger.id == id)?;
        match finger.contact {
            Contact::Resting if finger.second_tap => Some(ExploreGesture::Activate),
            Contact::Resting => {
                self.tap = Some((finger.start, now));
                None
            }
            Contact::Swiping => swipe(finger.start, at),
            Contact::Exploring => None,
        }
    }

    fn cancelled(&mut self, id: u64) {
        self.down.remove(&id);
        if self.down.is_empty() {
            self.ignoring = false;
        }
        if self.finger.is_some_and(|finger| finger.id == id) {
            self.finger = None;
        }
        self.tap = None;
    }

    /// When [`Self::poll`] next has something to say: a resting finger
    /// begins to explore, a slow swipe becomes exploring, or a lone tap is
    /// read.
    #[must_use]
    pub fn deadline(&self) -> Option<Instant> {
        let finger = self.finger.and_then(|finger| match finger.contact {
            Contact::Resting => finger.down.checked_add(EXPLORE_DELAY),
            Contact::Swiping => finger.down.checked_add(SWIPE_MAX),
            Contact::Exploring => None,
        });
        let tap = self
            .tap
            .and_then(|(_, lifted)| lifted.checked_add(DOUBLE_TAP_TIMEOUT));
        finger.into_iter().chain(tap).min()
    }

    /// The gestures that time alone completes by `now`.
    pub fn poll(&mut self, now: Instant) -> Vec<ExploreGesture> {
        let mut out = Vec::new();
        if let Some((at, lifted)) = self.tap
            && now.saturating_duration_since(lifted) > DOUBLE_TAP_TIMEOUT
        {
            self.tap = None;
            out.push(ExploreGesture::Explore(at));
        }
        if let Some(finger) = self.finger.as_mut() {
            let held = now.saturating_duration_since(finger.down);
            let explores = match finger.contact {
                Contact::Resting => held >= EXPLORE_DELAY,
                Contact::Swiping => held >= SWIPE_MAX,
                Contact::Exploring => false,
            };
            if explores {
                finger.contact = Contact::Exploring;
                finger.second_tap = false;
                out.push(ExploreGesture::Explore(finger.at));
            }
        }
        out
    }

    /// Forget every contact, as when the screen reader turns off.
    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

/// The swipe a quick movement from `start` to `end` makes, if any: mostly
/// horizontal and at least [`SWIPE_MIN`] long.
fn swipe(start: Point, end: Point) -> Option<ExploreGesture> {
    let (dx, dy) = (end.x - start.x, end.y - start.y);
    if dx.abs() < SWIPE_MIN || dx.abs() < dy.abs() * 2.0 {
        return None;
    }
    Some(if dx > 0.0 {
        ExploreGesture::Next
    } else {
        ExploreGesture::Previous
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    struct Run {
        explorer: TouchExplorer,
        start: Instant,
    }

    impl Run {
        fn new() -> Self {
            Self {
                explorer: TouchExplorer::new(),
                start: crate::time::now(),
            }
        }
        fn at(&self, t: u64) -> Instant {
            self.start + ms(t)
        }
        fn touch(
            &mut self,
            id: u64,
            phase: TouchPhase,
            x: f64,
            y: f64,
            t: u64,
        ) -> Vec<ExploreGesture> {
            let now = self.at(t);
            self.explorer.touch(id, phase, Point::new(x, y), now)
        }
        fn poll(&mut self, t: u64) -> Vec<ExploreGesture> {
            let now = self.at(t);
            self.explorer.poll(now)
        }
    }

    #[test]
    fn a_resting_finger_explores_after_the_delay_and_a_drag_follows_it() {
        let mut run = Run::new();
        assert!(run.touch(1, TouchPhase::Started, 10.0, 10.0, 0).is_empty());
        assert_eq!(run.explorer.deadline(), Some(run.at(200)));
        assert!(run.poll(199).is_empty(), "nothing before the delay");
        assert_eq!(
            run.poll(200),
            [ExploreGesture::Explore(Point::new(10.0, 10.0))]
        );
        assert_eq!(run.explorer.deadline(), None);
        // Every movement explores now, however small.
        assert_eq!(
            run.touch(1, TouchPhase::Moved, 12.0, 10.0, 220),
            [ExploreGesture::Explore(Point::new(12.0, 10.0))]
        );
        assert_eq!(
            run.touch(1, TouchPhase::Moved, 200.0, 300.0, 400),
            [ExploreGesture::Explore(Point::new(200.0, 300.0))]
        );
        // Lifting reads nothing and leaves nothing pending.
        assert!(
            run.touch(1, TouchPhase::Ended, 200.0, 300.0, 900)
                .is_empty()
        );
        assert_eq!(run.explorer.deadline(), None);
        assert!(run.poll(5_000).is_empty());
    }

    #[test]
    fn a_slow_drag_explores_once_it_passes_the_delay() {
        let mut run = Run::new();
        run.touch(1, TouchPhase::Started, 0.0, 0.0, 0);
        assert!(run.touch(1, TouchPhase::Moved, 5.0, 0.0, 100).is_empty());
        // Past the slop only after the delay: exploring, not a swipe. The
        // delay ran out before this movement, so where the finger rested is
        // read first.
        assert_eq!(
            run.touch(1, TouchPhase::Moved, 30.0, 0.0, 250),
            [
                ExploreGesture::Explore(Point::new(5.0, 0.0)),
                ExploreGesture::Explore(Point::new(30.0, 0.0))
            ]
        );
        assert!(run.touch(1, TouchPhase::Ended, 120.0, 0.0, 300).is_empty());
    }

    #[test]
    fn a_tap_explores_where_it_landed_once_no_second_tap_follows() {
        let mut run = Run::new();
        run.touch(1, TouchPhase::Started, 40.0, 50.0, 0);
        assert!(run.touch(1, TouchPhase::Ended, 41.0, 50.0, 80).is_empty());
        assert_eq!(run.explorer.deadline(), Some(run.at(380)));
        assert!(run.poll(380).is_empty());
        assert_eq!(
            run.poll(381),
            [ExploreGesture::Explore(Point::new(40.0, 50.0))]
        );
        assert!(run.poll(2_000).is_empty());
    }

    #[test]
    fn a_double_tap_activates_and_never_explores() {
        let mut run = Run::new();
        run.touch(1, TouchPhase::Started, 40.0, 50.0, 0);
        run.touch(1, TouchPhase::Ended, 40.0, 50.0, 70);
        assert!(
            run.touch(2, TouchPhase::Started, 70.0, 80.0, 250)
                .is_empty()
        );
        assert_eq!(
            run.touch(2, TouchPhase::Ended, 70.0, 80.0, 320),
            [ExploreGesture::Activate]
        );
        assert_eq!(run.explorer.deadline(), None);
        assert!(run.poll(5_000).is_empty(), "neither tap is read as a touch");
    }

    #[test]
    fn taps_too_far_apart_in_time_or_place_are_two_taps() {
        // Too late: the first tap is read, the second is a tap of its own.
        let mut run = Run::new();
        run.touch(1, TouchPhase::Started, 40.0, 50.0, 0);
        run.touch(1, TouchPhase::Ended, 40.0, 50.0, 70);
        assert_eq!(
            run.touch(2, TouchPhase::Started, 40.0, 50.0, 400),
            [ExploreGesture::Explore(Point::new(40.0, 50.0))]
        );
        assert!(run.touch(2, TouchPhase::Ended, 40.0, 50.0, 450).is_empty());

        // Too far: the second tap starts afresh.
        let mut run = Run::new();
        run.touch(1, TouchPhase::Started, 40.0, 50.0, 0);
        run.touch(1, TouchPhase::Ended, 40.0, 50.0, 70);
        run.touch(2, TouchPhase::Started, 400.0, 50.0, 200);
        assert!(run.touch(2, TouchPhase::Ended, 400.0, 50.0, 260).is_empty());
        assert_eq!(
            run.poll(600),
            [ExploreGesture::Explore(Point::new(400.0, 50.0))]
        );

        // Held: the second touch explores instead of activating.
        let mut run = Run::new();
        run.touch(1, TouchPhase::Started, 40.0, 50.0, 0);
        run.touch(1, TouchPhase::Ended, 40.0, 50.0, 70);
        run.touch(2, TouchPhase::Started, 40.0, 50.0, 200);
        assert_eq!(
            run.poll(400),
            [ExploreGesture::Explore(Point::new(40.0, 50.0))]
        );
        assert!(run.touch(2, TouchPhase::Ended, 40.0, 50.0, 500).is_empty());
    }

    #[test]
    fn a_quick_horizontal_swipe_moves_next_or_previous() {
        let mut run = Run::new();
        run.touch(1, TouchPhase::Started, 100.0, 100.0, 0);
        assert!(run.touch(1, TouchPhase::Moved, 140.0, 104.0, 60).is_empty());
        assert_eq!(run.explorer.deadline(), Some(run.at(500)));
        assert_eq!(
            run.touch(1, TouchPhase::Ended, 200.0, 110.0, 150),
            [ExploreGesture::Next]
        );

        run.touch(1, TouchPhase::Started, 300.0, 100.0, 1_000);
        run.touch(1, TouchPhase::Moved, 250.0, 100.0, 1_050);
        assert_eq!(
            run.touch(1, TouchPhase::Ended, 200.0, 95.0, 1_120),
            [ExploreGesture::Previous]
        );
        assert!(run.poll(5_000).is_empty(), "a swipe reads nothing else");
    }

    #[test]
    fn short_vertical_or_slow_movements_are_not_swipes() {
        // Too short.
        let mut run = Run::new();
        run.touch(1, TouchPhase::Started, 100.0, 100.0, 0);
        run.touch(1, TouchPhase::Moved, 120.0, 100.0, 40);
        assert!(run.touch(1, TouchPhase::Ended, 140.0, 100.0, 80).is_empty());

        // Mostly vertical.
        run.touch(1, TouchPhase::Started, 100.0, 100.0, 1_000);
        run.touch(1, TouchPhase::Moved, 120.0, 150.0, 1_040);
        assert!(
            run.touch(1, TouchPhase::Ended, 160.0, 200.0, 1_080)
                .is_empty()
        );

        // Too slow: it explores where the finger is once the swipe's time
        // is up, and the lift moves nothing.
        run.touch(1, TouchPhase::Started, 100.0, 100.0, 2_000);
        run.touch(1, TouchPhase::Moved, 130.0, 100.0, 2_050);
        assert_eq!(
            run.poll(2_500),
            [ExploreGesture::Explore(Point::new(130.0, 100.0))]
        );
        assert_eq!(
            run.touch(1, TouchPhase::Moved, 300.0, 100.0, 2_550),
            [ExploreGesture::Explore(Point::new(300.0, 100.0))]
        );
        assert!(
            run.touch(1, TouchPhase::Ended, 300.0, 100.0, 2_600)
                .is_empty()
        );
    }

    #[test]
    fn a_cancelled_touch_reads_nothing_and_forgets_a_pending_tap() {
        let mut run = Run::new();
        run.touch(1, TouchPhase::Started, 40.0, 50.0, 0);
        run.touch(1, TouchPhase::Ended, 40.0, 50.0, 70);
        run.touch(2, TouchPhase::Started, 40.0, 50.0, 150);
        assert!(
            run.touch(2, TouchPhase::Cancelled, 40.0, 50.0, 180)
                .is_empty()
        );
        assert_eq!(run.explorer.deadline(), None);
        assert!(run.poll(5_000).is_empty());

        // A swipe cancelled midway moves nothing.
        run.touch(3, TouchPhase::Started, 100.0, 100.0, 6_000);
        run.touch(3, TouchPhase::Moved, 200.0, 100.0, 6_050);
        assert!(
            run.touch(3, TouchPhase::Cancelled, 200.0, 100.0, 6_080)
                .is_empty()
        );
        assert!(run.poll(9_000).is_empty());
    }

    #[test]
    fn a_second_finger_ends_the_gesture_until_every_finger_lifts() {
        let mut run = Run::new();
        run.touch(1, TouchPhase::Started, 100.0, 100.0, 0);
        run.touch(2, TouchPhase::Started, 200.0, 100.0, 30);
        assert_eq!(run.explorer.deadline(), None);
        assert!(run.touch(1, TouchPhase::Moved, 300.0, 100.0, 60).is_empty());
        assert!(run.touch(1, TouchPhase::Ended, 300.0, 100.0, 90).is_empty());
        // The remaining finger is still part of the two-finger touch.
        assert!(
            run.touch(2, TouchPhase::Moved, 260.0, 100.0, 120)
                .is_empty()
        );
        assert!(
            run.touch(2, TouchPhase::Ended, 260.0, 100.0, 150)
                .is_empty()
        );
        assert!(run.poll(5_000).is_empty());
        // Then one finger is read again.
        run.touch(3, TouchPhase::Started, 10.0, 10.0, 6_000);
        assert_eq!(
            run.poll(6_200),
            [ExploreGesture::Explore(Point::new(10.0, 10.0))]
        );
    }
}
