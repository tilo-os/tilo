//! A display shows one frame per refresh: a change that arrives between two vertical blanks waits
//! for the blank that shows it, and what arrives before that blank joins the frame it is shown in.

use super::*;

/// A display shows one frame per refresh: a change right after a frame waits for the blank that
/// shows it, and the loop sleeps until then instead of drawing again at once.
#[test]
fn a_display_shows_one_frame_per_refresh() {
    let mut compositor = Compositor::headless(Box::new(StackPolicy)).expect("compositor");
    let mut config = OutputConfig::new("HEADLESS-1", 640, 480);
    config.refresh_mhz = 10_000;
    compositor.add_output(&config).expect("output");
    compositor.draw_frame().expect("a frame");
    let shown = compositor.frames_presented();

    // Marking an app private changes what the desk a picture stands on draws.
    compositor.protect_app("org.example.shared", true);
    // The blank the change is shown at is ahead of now, whether the frame just drawn took most of
    // the tenth of a second or almost none of it.
    let wait = compositor.next_wait().expect("the blank that shows it");
    assert!(
        wait > Duration::ZERO && wait <= Duration::from_millis(100),
        "the blank ahead, on the display's tenth-of-a-second grid: {wait:?}"
    );
    compositor.render().expect("a turn");
    assert_eq!(
        compositor.frames_presented(),
        shown,
        "no frame before the blank"
    );
    compositor.draw_frame().expect("a frame");
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
