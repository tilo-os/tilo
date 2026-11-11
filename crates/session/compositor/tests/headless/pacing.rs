//! A display shows one frame per refresh: a change that arrives between two vertical blanks waits
//! for the blank that shows it, and what arrives before that blank joins the frame it is shown in.

use super::*;

/// Run `operation` at `instant` on the displays' clock, which the pacing reads: a frame is due at
/// the instant its grid says, not at the instant the machine took to get there.
fn at<T>(instant: Instant, operation: impl FnOnce() -> T) -> T {
    tilo_clock::testing::with_monotonic_time(instant, operation)
}

/// A display shows one frame per refresh: a change right after a frame waits for the blank that
/// shows it, and the loop sleeps until then instead of drawing again at once.
#[test]
fn a_display_shows_one_frame_per_refresh() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    let mut config = OutputConfig::new("HEADLESS-1", 640, 480);
    // A tenth of a second per blank.
    config.refresh_mhz = 10_000;
    // The grid this display keeps starts where the test holds the clock, so the blank a change is
    // shown at is the display's to say, however long the frame that found the change took to draw.
    let grid = tilo_clock::now();
    at(grid, || compositor.add_output(&config)).expect("output");
    // A frame that shows something: one that changed no pixel takes no blank, as a display's
    // backend queues no flip for it.
    let mut client = mapped_client(
        &mut compositor,
        "org.example.shown",
        (200, 100),
        [0, 255, 0, 255],
    );
    at(grid, || compositor.render()).expect("a frame");
    let shown = compositor.frames_presented();
    assert_eq!(shown, 1, "the window's first frame is on the display");

    // Marking an app private changes what the desk a picture stands on draws.
    client.show(200, 100, [0, 0, 255, 255]);
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        compositor.owes_frame()
    });
    compositor.protect_app("org.example.shared", true);
    // The blank the change is shown at is the display's next one, ahead of the frame that found it.
    let due = compositor
        .frame_deadline()
        .expect("the blank that shows it");
    assert!(
        due.saturating_duration_since(grid) <= Duration::from_millis(100),
        "the blank is the display's next one, one refresh on"
    );
    let before = due
        .checked_sub(Duration::from_millis(1))
        .expect("an instant before the blank");
    at(before, || {
        assert_eq!(
            compositor.next_wait(),
            Some(due.saturating_duration_since(before)),
            "the loop sleeps until the blank, not a whole refresh"
        );
        compositor.render().expect("a turn");
    });
    assert_eq!(
        compositor.frames_presented(),
        shown,
        "no frame before the blank"
    );
    at(due, || compositor.render()).expect("a turn");
    assert_eq!(
        compositor.frames_presented(),
        shown + 1,
        "the frame the blank shows"
    );
}

/// What arrives while a frame is held for its blank joins that frame: the wait takes the client's
/// commit in, and the frame the blank shows carries it, rather than the commit taking a turn of its
/// own first.
#[test]
fn a_held_frame_shows_what_arrived_before_its_blank() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    let mut config = OutputConfig::new("HEADLESS-1", 640, 480);
    config.refresh_mhz = 10_000;
    compositor.add_output(&config).expect("output");
    let mut client = mapped_client(
        &mut compositor,
        "org.example.shared",
        (200, 100),
        [255, 0, 0, 255],
    );
    compositor.draw_frame().expect("a frame");
    let shown = compositor.frames_presented();
    let centre = |compositor: &mut Compositor| {
        compositor
            .screenshot("HEADLESS-1")
            .expect("the frame")
            .pixel(320, 240)
    };
    assert_eq!(centre(&mut compositor), Some([255, 0, 0, 255]));

    client.show(200, 100, [0, 0, 255, 255]);
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        compositor.owes_frame()
    });
    assert_eq!(
        compositor.frames_presented(),
        shown,
        "the commit is in, and no frame drawn for it"
    );
    compositor.draw_frame().expect("a frame");
    assert_eq!(
        compositor.frames_presented(),
        shown + 1,
        "one frame for the blank, not one for the commit too"
    );
    assert_eq!(
        centre(&mut compositor),
        Some([0, 0, 255, 255]),
        "the commit is in it"
    );
}

/// What a host serves while it waits for the blank runs there, and the frame the blank shows is
/// drawn afterwards: an app someone is waiting on starts beside the frame the pass owes rather
/// than after it.
#[test]
fn the_wait_for_the_blank_serves_what_arrived_before_the_frame() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    let mut config = OutputConfig::new("HEADLESS-1", 640, 480);
    // A blank half a second out: the wait that serves arrivals is still ahead of it.
    config.refresh_mhz = 2_000;
    compositor.add_output(&config).expect("output");
    let mut client = mapped_client(
        &mut compositor,
        "org.example.shared",
        (200, 100),
        [255, 0, 0, 255],
    );
    compositor.draw_frame().expect("a frame");
    let shown = compositor.frames_presented();

    // The desk changed: the frame that shows it waits for the blank.
    client.show(200, 100, [0, 0, 255, 255]);
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        compositor.owes_frame()
    });
    let mut served = 0;
    compositor
        .await_frame_arrivals(|_| served += 1)
        .expect("the wait");
    assert!(served >= 1, "the wait served what arrived while it waited");
    assert_eq!(
        compositor.frames_presented(),
        shown,
        "serving arrivals draws no frame of its own"
    );
    compositor.draw_frame().expect("a frame");
    assert_eq!(
        compositor.frames_presented(),
        shown + 1,
        "the frame the blank shows carries what arrived"
    );
}
