//! When a bar shows over the window that covers its display: entering full screen shows it for its
//! idle time, the top edge brings it back, a press in the band while it hides is the window's, and
//! leaving full screen docks it again. The person's own "hide the top bar" setting is the same
//! machinery without a fullscreen window.
//! Every rule is reasoned about at an instant, so these drive a synthetic timeline rather than a
//! clock.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::arithmetic_side_effects,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_desktop::chrome::TopBarPresence;
use tilo_desktop::topbar::reveal::FULLSCREEN_BAR_IDLE;
use tilo_desktop::topbar::{Finger, Reveals};
use ui::time::{Duration, Instant};
use ui::vello::kurbo::{Point, Rect};
use ui::widget::top_edge_reveal::DWELL;

const DISPLAY: Rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
const BAR_HEIGHT: f64 = 24.0;

fn ms(t: Instant, n: u64) -> Instant {
    t.checked_add(Duration::from_millis(n))
        .expect("test instants stay in range")
}

/// A desk of one display with its bar docked on the display's top edge.
fn desk() -> (Reveals, Instant) {
    let t0 = Instant::now();
    let mut reveals = Reveals::default();
    reveals.set_height(BAR_HEIGHT);
    reveals.follow(&[(None, DISPLAY, false)], t0);
    (reveals, t0)
}

/// A desk whose bar shows only when reached for, as the person's "hide the top bar" asks.
fn hidden_desk() -> (Reveals, Instant) {
    let (mut reveals, t0) = desk();
    reveals.set_hide(true);
    (reveals, t0)
}

/// A desk whose display a window just took full screen.
fn fullscreen_desk() -> (Reveals, Instant) {
    let (mut reveals, t0) = desk();
    reveals.follow(&[(None, DISPLAY, true)], t0);
    (reveals, t0)
}

/// A straight pointer path from `from` to `to` in `steps` samples over `total` milliseconds from
/// `t` ; returns the last sample's instant.
fn path(
    reveals: &mut Reveals,
    from: Point,
    to: Point,
    steps: u32,
    total: u64,
    t: Instant,
) -> Instant {
    let mut at = t;
    for i in 1..=steps {
        let f = f64::from(i) / f64::from(steps);
        at = ms(
            t,
            total
                .saturating_mul(u64::from(i))
                .checked_div(u64::from(steps))
                .unwrap_or(total),
        );
        reveals.motion(from.lerp(to, f), at);
    }
    at
}

/// Whether something of the bar is held open, which keeps it shown.
fn idle(_: Option<u32>) -> bool {
    false
}

#[test]
fn docked_unless_the_person_hides_it() {
    let (mut reveals, t0) = desk();
    path(
        &mut reveals,
        Point::new(600.0, 500.0),
        Point::new(600.0, 0.0),
        6,
        100,
        t0,
    );
    assert_eq!(reveals.presence(None, false), TopBarPresence::Docked);
    assert_eq!(reveals.deadline(), None);

    reveals.set_hide(true);
    assert_eq!(reveals.presence(None, false), TopBarPresence::Hidden);
    assert_eq!(
        reveals.presence(None, true),
        TopBarPresence::Shown,
        "a menu of the bar holds it"
    );
}

#[test]
fn a_push_to_the_top_edge_shows_it_and_leaving_hides_it() {
    let (mut reveals, t0) = hidden_desk();
    let t = path(
        &mut reveals,
        Point::new(600.0, 500.0),
        Point::new(620.0, 0.0),
        6,
        120,
        t0,
    );
    // Stopped against the edge, it shows after the rest it takes; a click there before then would
    // have been the window's.
    assert_eq!(reveals.deadline(), Some(t + DWELL));
    reveals.settle(t + DWELL, idle);
    assert_eq!(
        reveals.presence(None, false),
        TopBarPresence::Shown,
        "pushed to the edge"
    );

    // Along the bar it stays; straight down into the window it goes.
    let t = path(
        &mut reveals,
        Point::new(620.0, 0.0),
        Point::new(1200.0, 12.0),
        8,
        400,
        ms(t0, 500),
    );
    assert_eq!(reveals.presence(None, false), TopBarPresence::Shown);
    let t = path(
        &mut reveals,
        Point::new(1200.0, 12.0),
        Point::new(700.0, 400.0),
        6,
        120,
        t,
    );
    reveals.settle(ms(t, 2000), idle);
    assert_eq!(reveals.presence(None, false), TopBarPresence::Hidden);
}

/// Crossing the top edge on the way somewhere else is not reaching for the bar, however fast it
/// goes.
#[test]
fn crossing_the_edge_in_transit_does_not_show_it() {
    let (mut reveals, t0) = hidden_desk();
    let t = path(
        &mut reveals,
        Point::new(80.0, 400.0),
        Point::new(90.0, 2.0),
        4,
        60,
        t0,
    );
    let t = path(
        &mut reveals,
        Point::new(90.0, 2.0),
        Point::new(1240.0, 2.0),
        8,
        80,
        t,
    );
    let t = path(
        &mut reveals,
        Point::new(1240.0, 2.0),
        Point::new(1250.0, 400.0),
        4,
        60,
        t,
    );
    reveals.settle(ms(t, 4000), idle);
    assert_eq!(reveals.presence(None, false), TopBarPresence::Hidden);
}

#[test]
fn slowing_onto_a_window_near_the_top_is_not_reaching_for_the_bar() {
    let (mut reveals, t0) = hidden_desk();
    // A flick up that settles on a tab row 14 px down, below the band.
    let t = path(
        &mut reveals,
        Point::new(300.0, 500.0),
        Point::new(300.0, 14.0),
        6,
        120,
        t0,
    );
    reveals.settle(ms(t, 2000), idle);
    assert_eq!(reveals.presence(None, false), TopBarPresence::Hidden);

    // Resting in the band and clicking at once: the click is the window's,
    // and the rest it was waiting for is cancelled.
    let p = Point::new(310.0, 2.0);
    reveals.motion(p, ms(t, 2100));
    reveals.press(p);
    let p = Point::new(330.0, 0.0);
    let t = path(
        &mut reveals,
        Point::new(310.0, 300.0),
        p,
        4,
        60,
        ms(t, 6000),
    );
    reveals.press(p);
    assert_eq!(reveals.deadline(), None, "the rest is cancelled");
    reveals.settle(ms(t, 2000), idle);
    assert_eq!(reveals.presence(None, false), TopBarPresence::Hidden);
}

#[test]
fn a_fullscreen_window_shows_the_bar_on_entering_and_it_collapses_when_idle() {
    let (mut reveals, t0) = fullscreen_desk();
    assert_eq!(
        reveals.presence(None, false),
        TopBarPresence::Shown,
        "entering full screen shows where the bar went"
    );
    // The pointer moving over the window does not cut it short.
    reveals.motion(Point::new(600.0, 500.0), ms(t0, 100));
    reveals.motion(Point::new(640.0, 520.0), ms(t0, 140));
    assert_eq!(reveals.presence(None, false), TopBarPresence::Shown);
    let due = reveals.deadline().expect("it is due to collapse");
    assert_eq!(due, t0 + FULLSCREEN_BAR_IDLE);
    assert!(reveals.settle(due, idle));
    assert_eq!(reveals.presence(None, false), TopBarPresence::Hidden);
    assert_eq!(reveals.deadline(), None, "and nothing wakes for it");

    // The same window again is nothing new; leaving full screen forgets it.
    reveals.follow(&[(None, DISPLAY, true)], ms(t0, 5000));
    assert_eq!(reveals.presence(None, false), TopBarPresence::Hidden);
    reveals.follow(&[(None, DISPLAY, false)], ms(t0, 5000));
    assert_eq!(reveals.presence(None, false), TopBarPresence::Docked);
    assert_eq!(reveals.entered(), None);
    assert_eq!(reveals.deadline(), None);
}

#[test]
fn a_key_or_a_swipe_brings_it_back_for_the_idle_time() {
    let (mut reveals, t0) = fullscreen_desk();
    reveals.settle(t0 + FULLSCREEN_BAR_IDLE, idle);
    assert_eq!(reveals.presence(None, false), TopBarPresence::Hidden);

    let t = ms(t0, 3000);
    assert!(reveals.reveal_by_key(t), "the key shows it");
    assert_eq!(reveals.presence(None, false), TopBarPresence::Shown);
    assert_eq!(reveals.deadline(), Some(t + FULLSCREEN_BAR_IDLE));
    // Asked for again, it stays that long again.
    let again = ms(t, 1500);
    assert!(!reveals.reveal_by_key(again));
    assert!(!reveals.settle(t + FULLSCREEN_BAR_IDLE, idle));
    assert_eq!(reveals.presence(None, false), TopBarPresence::Shown);
    reveals.settle(again + FULLSCREEN_BAR_IDLE, idle);
    assert_eq!(reveals.presence(None, false), TopBarPresence::Hidden);

    // A finger coming down from the top edge shows it too, and the desk takes the contact so
    // nothing under it sees it.
    let edge = Point::new(600.0, 1.0);
    assert!(
        reveals.touch(Finger::Down, edge, ms(t0, 9000)),
        "a finger on the top edge is the desk's"
    );
    reveals.touch(Finger::Move, Point::new(600.0, 40.0), ms(t0, 9050));
    assert_eq!(reveals.presence(None, false), TopBarPresence::Shown);
    reveals.touch(Finger::Lift, Point::new(600.0, 40.0), ms(t0, 9100));
    let up = ms(t0, 9050);
    reveals.settle(up + FULLSCREEN_BAR_IDLE, idle);
    assert_eq!(
        reveals.presence(None, false),
        TopBarPresence::Hidden,
        "no finger resting on it, it goes when idle"
    );
}

#[test]
fn the_pointer_on_it_or_using_it_keeps_it() {
    let (mut reveals, t0) = fullscreen_desk();
    // In use (its calendar, say), it stays past the idle time.
    let due = t0 + FULLSCREEN_BAR_IDLE;
    reveals.settle(due, |_| true);
    assert_eq!(reveals.presence(None, false), TopBarPresence::Shown);
    assert_eq!(reveals.deadline(), Some(due + FULLSCREEN_BAR_IDLE));

    // Resting on it, it stays; leaving it for the window hides it.
    reveals.motion(Point::new(600.0, 12.0), ms(t0, 2500));
    reveals.settle(due + FULLSCREEN_BAR_IDLE, idle);
    assert_eq!(reveals.presence(None, false), TopBarPresence::Shown);
    let t = ms(t0, 7000);
    for (i, y) in [60.0, 200.0, 400.0].into_iter().enumerate() {
        let at = ms(t, 40 * (u64::try_from(i).unwrap_or(0) + 1));
        reveals.motion(Point::new(600.0, y), at);
    }
    reveals.settle(ms(t, 2000), idle);
    assert_eq!(reveals.presence(None, false), TopBarPresence::Hidden);
}
