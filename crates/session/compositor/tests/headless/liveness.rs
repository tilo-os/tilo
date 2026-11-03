//! An app that stops answering: its window says so within the timeout, and
//! the person can wait for it or quit it without touching the other apps.

use super::*;

/// The window id of the window `app` opened.
fn window_of(compositor: &Compositor, app: &str) -> u64 {
    compositor
        .windows()
        .into_iter()
        .find(|(info, _)| info.app_id.as_deref() == Some(app))
        .map(|(info, _)| info.id)
        .expect("the app's window")
}

/// Click the window `id`, so its app is given input: what asks it to answer.
fn click(compositor: &mut Compositor, id: u64) {
    let (_, (x, y, width, height)) = compositor
        .windows()
        .into_iter()
        .find(|(info, _)| info.id == id)
        .expect("the window");
    compositor
        .input(InputEvent::PointerMotion {
            x: f64::from(x + width / 2),
            y: f64::from(y + height / 2),
        })
        .unwrap();
    for pressed in [true, false] {
        compositor
            .input(InputEvent::PointerButton {
                button: BTN_LEFT,
                pressed,
            })
            .unwrap();
    }
}

/// `ms` milliseconds from now, when the compositor looks again.
fn later(ms: u64) -> Instant {
    Instant::now()
        .checked_add(Duration::from_millis(ms))
        .expect("in range")
}

#[test]
fn an_app_that_stops_answering_says_so_within_the_timeout() {
    let mut compositor = headless(Box::new(StackPolicy));
    let _client = mapped_client(&mut compositor, "busy", (200, 100), [255, 0, 0, 255]);
    let id = window_of(&compositor, "busy");

    click(&mut compositor, id);
    assert!(
        compositor.not_responding_windows().is_empty(),
        "a click leaves the app time to answer"
    );
    assert!(!compositor.advance_app_liveness_at(later(4_000)));
    assert!(compositor.not_responding_windows().is_empty());
    assert!(
        compositor.advance_app_liveness_at(later(6_000)),
        "the silence is past the timeout: the window says so"
    );
    assert_eq!(compositor.not_responding_windows(), [id]);
}

#[test]
fn an_app_nobody_touched_is_not_judged() {
    let mut compositor = headless(Box::new(StackPolicy));
    let _client = mapped_client(&mut compositor, "quiet", (200, 100), [0, 255, 0, 255]);

    // A window off screen could not answer from inside a blocked swap; one
    // nobody sent input to is not asked.
    assert!(!compositor.advance_app_liveness_at(later(60_000)));
    assert!(compositor.not_responding_windows().is_empty());
}

#[test]
fn wait_gives_the_app_another_full_period() {
    let mut compositor = headless(Box::new(StackPolicy));
    let _client = mapped_client(&mut compositor, "slow", (200, 100), [0, 0, 255, 255]);
    let id = window_of(&compositor, "slow");
    click(&mut compositor, id);
    assert!(compositor.advance_app_liveness_at(later(6_000)));
    assert_eq!(compositor.not_responding_windows(), [id]);

    assert!(
        compositor.wait_window(id),
        "waiting takes the card away at once"
    );
    assert!(compositor.not_responding_windows().is_empty());
    assert!(!compositor.advance_app_liveness_at(later(2_000)));
    assert!(compositor.not_responding_windows().is_empty());
    assert!(
        compositor.advance_app_liveness_at(later(6_000)),
        "still silent after another full period: it says so again"
    );
    assert_eq!(compositor.not_responding_windows(), [id]);
}

#[test]
fn force_quit_ends_only_the_app_that_hung() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut other = mapped_client(&mut compositor, "other", (200, 100), [0, 255, 0, 255]);
    let _stuck = mapped_client(&mut compositor, "stuck", (200, 100), [255, 0, 0, 255]);
    let other_window = window_of(&compositor, "other");
    let stuck_window = window_of(&compositor, "stuck");
    click(&mut compositor, stuck_window);
    assert!(compositor.advance_app_liveness_at(later(6_000)));
    assert_eq!(compositor.not_responding_windows(), [stuck_window]);

    compositor.force_quit_window(stuck_window);
    assert!(compositor.not_responding_windows().is_empty());
    pump(&mut compositor, &mut [&mut other], |compositor, _| {
        compositor
            .windows()
            .iter()
            .all(|(info, _)| info.id != stuck_window)
    });
    assert_eq!(
        window_of(&compositor, "other"),
        other_window,
        "the app that did nothing wrong keeps its window"
    );
}

#[test]
fn an_app_that_answers_again_stops_saying_not_responding() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "came-back", (200, 100), [255, 255, 0, 255]);
    let id = window_of(&compositor, "came-back");
    click(&mut compositor, id);
    assert!(compositor.advance_app_liveness_at(later(6_000)));
    assert_eq!(compositor.not_responding_windows(), [id]);

    // It read what it was sent and answered: the window is an app like any
    // other again, and nothing was clicked to say so.
    client.exchange();
    compositor
        .dispatch(Duration::from_millis(2))
        .expect("dispatch");
    assert!(!compositor.advance_app_liveness_at(Instant::now()));
    assert!(compositor.not_responding_windows().is_empty());
}
