//! How a window is framed: the desk's card, its own frame, or none at all
//! when it fills a display and its content is what the display shows.

use super::*;

/// A window framed square is drawn as its buffer is: the element reaches the
/// renderer untouched, which is what lets a display show that buffer itself.
#[test]
fn a_window_framed_square_is_drawn_as_its_buffer_is() {
    let mut compositor = headless(Box::new(StackPolicy));
    let _client = mapped_client(&mut compositor, "red", (200, 100), [255, 0, 0, 255]);
    let (info, _) = compositor.windows()[0].clone();
    // A window that fills the display stands on it: no corners, no shadow.
    compositor.set_window_frame(WindowFrame::square());
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let pixel = |x, y| frame.pixel(x, y).expect("on the output");
    assert_eq!(pixel(221, 191), [255, 0, 0, 255], "the corner is square");
    assert_eq!(pixel(220, 190), [255, 0, 0, 255], "its first pixel");
    assert!(
        luma(pixel(320, 292)) == luma(BACKGROUND),
        "and nothing is cast below it"
    );

    // A window can say so itself, leaving the rest of the desktop framed.
    compositor.set_window_frame(WindowFrame::default());
    compositor.set_window_frame_for(info.id, Some(WindowFrame::square()));
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        frame.pixel(221, 191),
        Some([255, 0, 0, 255]),
        "square again"
    );
    compositor.set_window_frame_for(info.id, None);
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_ne!(
        frame.pixel(221, 191),
        Some([255, 0, 0, 255]),
        "and round again"
    );
}

/// A window the compositor gave a display to fill and whose content covers
/// it is framed as its buffer is, so the display can show the buffer.
#[test]
fn a_window_that_fills_the_display_is_drawn_as_its_buffer_is() {
    let mut compositor = headless(Box::new(StackPolicy));
    // The desk's card: a window standing on the desktop has round corners.
    compositor.set_window_frame(WindowFrame::default());
    let mut client = mapped_client(&mut compositor, "video", (200, 100), [255, 0, 0, 255]);
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let corner = frame.pixel(221, 191).expect("a corner pixel");
    assert!(
        corner[0] < 90,
        "a window on the desk keeps its card: {corner:?}"
    );

    // A player takes the display: what it draws is what the display shows,
    // corners and all.
    let red = Some([255, 0, 0, 255]);
    client
        .toplevel
        .as_ref()
        .expect("toplevel")
        .set_fullscreen(None);
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        compositor.windows()[0].1.0 == 0 && compositor.windows()[0].1.1 == 0
    });
    client.show(640, 480, [255, 0, 0, 255]);
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        compositor.windows()[0].1 == (0, 0, 640, 480)
    });
    client.show(640, 480, [255, 0, 0, 255]);
    pump(&mut compositor, &mut [&mut client], |_, clients| {
        clients[0].seen.frames_done > 0
    });
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    for (x, y) in [(0, 0), (639, 0), (0, 479), (639, 479), (320, 240)] {
        assert_eq!(
            frame.pixel(x, y),
            red,
            "the display shows the buffer at ({x}, {y})"
        );
    }
}
