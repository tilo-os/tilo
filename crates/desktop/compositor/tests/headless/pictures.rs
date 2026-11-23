//! Windows pictured by a surface: live in a rect of their own, with their corners and lift,
//! and as a placeholder where the window is private.

use super::*;

/// A surface that pictures one window.
struct Pictures(WindowPicture);

impl ShellSurface for Pictures {
    fn layers(&self) -> Vec<SurfaceLayer> {
        Vec::new()
    }

    fn accepts_pointer(&self, _x: f64, _y: f64) -> bool {
        false
    }

    fn pointer_motion(&mut self, _x: f64, _y: f64) {}

    fn pointer_button(&mut self, _button: u32, _pressed: bool) {}

    fn window_pictures(&self) -> Vec<WindowPicture> {
        vec![self.0]
    }
}

#[test]
fn a_surface_surface_pictures_a_window_live_in_a_rect_of_its_own() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(&mut compositor, "red", (200, 100), [255, 0, 0, 255]);
    let (info, _) = compositor.windows()[0].clone();
    compositor.add_surface(Rc::new(RefCell::new(Pictures(WindowPicture {
        window: info.id,
        rect: (10.0, 10.0, 100.0, 100.0),
        opacity: 1.0,
        clip: None,
    }))));
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    // 200 x 100 fits 100 x 100 as 100 x 50, centred: rows 35..85.
    assert_eq!(frame.pixel(12, 40), Some([255, 0, 0, 255]));
    assert_eq!(frame.pixel(108, 80), Some([255, 0, 0, 255]));
    assert_eq!(frame.pixel(60, 20), Some(BACKGROUND), "above the picture");
    assert_eq!(frame.pixel(60, 95), Some(BACKGROUND), "below it");
    assert_eq!(
        frame.pixel(320, 240),
        Some([255, 0, 0, 255]),
        "the window itself"
    );

    // The picture is live: the window's next buffer shows in it.
    client.show(200, 100, [0, 255, 0, 255]);
    pump(&mut compositor, &mut [&mut client], |_, _| true);
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(frame.pixel(60, 60), Some([0, 255, 0, 255]));
}

#[test]
fn a_picture_of_a_window_carries_its_corners_and_its_lift() {
    let mut compositor = headless(Box::new(StackPolicy));
    let _client = mapped_client(&mut compositor, "red", (200, 100), [255, 0, 0, 255]);
    let (info, _) = compositor.windows()[0].clone();
    compositor.add_surface(Rc::new(RefCell::new(Pictures(WindowPicture {
        window: info.id,
        rect: (10.0, 10.0, 100.0, 100.0),
        opacity: 1.0,
        clip: None,
    }))));
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    let pixel = |x, y| frame.pixel(x, y).expect("on the output");
    // Half size in a 100 x 100 rect: 100 x 50 at (10, 35), corners cut by 7.
    assert_eq!(pixel(60, 60), [255, 0, 0, 255], "the picture");
    assert_ne!(pixel(11, 36), [255, 0, 0, 255], "its corner is cut");
    assert_eq!(pixel(30, 60), [255, 0, 0, 255], "past the cut");
    assert!(
        luma(pixel(60, 88)) < luma(BACKGROUND),
        "and it casts the window's lift below: {:?}",
        pixel(60, 88)
    );

    // The picture is a view of its own: the window's effect moves the window, not the picture.
    compositor.set_window_effect(
        info.id,
        Some(WindowEffect {
            offset: (-200.0, 0.0),
            ..WindowEffect::default()
        }),
    );
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        frame.pixel(60, 60),
        Some([255, 0, 0, 255]),
        "the picture stays"
    );
    assert_eq!(
        frame.pixel(320, 240),
        Some(BACKGROUND),
        "the window moved away"
    );
}

/// The pixels of a picture's rect (fits 200 x 100 into 100 x 100: 100 x 50 at 10, 35) and of the
/// window itself (centred: 220, 190 to 420, 290).
const IN_PICTURE: [(u32, u32); 4] = [(15, 40), (60, 60), (105, 40), (60, 80)];
const IN_WINDOW: (u32, u32) = (320, 240);

/// A window the session keeps private is a placeholder wherever a surface pictures it, on
/// the person's own display too: the shell a picture stands on is what a screenshot, a recording and
/// a share show.
#[test]
fn a_private_window_is_pictured_as_the_placeholder_its_captures_show() {
    let mut compositor = headless(Box::new(StackPolicy));
    let _client = mapped_client(
        &mut compositor,
        "org.example.shared",
        (200, 100),
        [255, 0, 0, 255],
    );
    let (info, _) = compositor.windows()[0].clone();
    compositor.add_surface(Rc::new(RefCell::new(Pictures(WindowPicture {
        window: info.id,
        rect: (10.0, 10.0, 100.0, 100.0),
        opacity: 1.0,
        clip: None,
    }))));
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        frame.pixel(60, 60),
        Some([255, 0, 0, 255]),
        "nobody has marked it private yet: the picture is live"
    );

    compositor.protect_app("org.example.shared", true);
    compositor.draw_frame().expect("a frame");
    let display = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        display.pixel(IN_WINDOW.0, IN_WINDOW.1),
        Some([255, 0, 0, 255]),
        "the person's own display still shows the window"
    );
    let placeholder = display
        .pixel(IN_PICTURE[0].0, IN_PICTURE[0].1)
        .expect("on the output");
    assert_ne!(
        placeholder,
        [255, 0, 0, 255],
        "the picture carries none of the window"
    );
    for (x, y) in IN_PICTURE {
        assert_eq!(
            display.pixel(x, y),
            Some(placeholder),
            "the whole picture is one flat placeholder at ({x}, {y})"
        );
    }

    // A copy of the shell shows the window and its picture as that same placeholder: no secret
    // leaves with it.
    let copy = compositor
        .read_back_region("HEADLESS-1", (0, 0, 640, 480))
        .expect("a capture of the output");
    assert_eq!(copy.pixel(IN_WINDOW.0, IN_WINDOW.1), Some(placeholder));
    for (x, y) in IN_PICTURE {
        assert_eq!(
            copy.pixel(x, y),
            Some(placeholder),
            "the picture at ({x}, {y})"
        );
    }
    let red = |pixel: [u8; 4]| pixel[0] > 128 && pixel[1] < 64 && pixel[2] < 64;
    for y in 0..copy.height {
        for x in 0..copy.width {
            assert!(
                !red(copy.pixel(x, y).expect("on the capture")),
                "no secret of the window at ({x}, {y})"
            );
        }
    }

    // Shown again once the session no longer marks it private.
    compositor.protect_app("org.example.shared", false);
    compositor.draw_frame().expect("a frame");
    let frame = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(frame.pixel(60, 60), Some([255, 0, 0, 255]), "pictured live");
}

/// An app known to hold secrets is private without asking, and its picture is the placeholder its
/// captures are.
#[test]
fn a_picture_of_an_app_that_holds_secrets_is_its_placeholder() {
    let mut compositor = headless(Box::new(StackPolicy));
    let _client = mapped_client(
        &mut compositor,
        "so.tilo.passwords",
        (200, 100),
        [255, 0, 0, 255],
    );
    let (info, _) = compositor.windows()[0].clone();
    compositor.add_surface(Rc::new(RefCell::new(Pictures(WindowPicture {
        window: info.id,
        rect: (10.0, 10.0, 100.0, 100.0),
        opacity: 1.0,
        clip: None,
    }))));
    compositor.draw_frame().expect("a frame");
    let display = compositor.screenshot("HEADLESS-1").unwrap();
    let placeholder = display
        .pixel(IN_PICTURE[0].0, IN_PICTURE[0].1)
        .expect("on the output");
    assert_ne!(placeholder, [255, 0, 0, 255]);
    for (x, y) in IN_PICTURE {
        assert_eq!(display.pixel(x, y), Some(placeholder), "at ({x}, {y})");
    }
    let copy = compositor
        .read_back_region("HEADLESS-1", (0, 0, 640, 480))
        .expect("a capture of the output");
    for (x, y) in IN_PICTURE {
        assert_eq!(
            copy.pixel(x, y),
            Some(placeholder),
            "captured at ({x}, {y})"
        );
    }
    assert_eq!(copy.pixel(IN_WINDOW.0, IN_WINDOW.1), Some(placeholder));
}

/// A private window that closed still stands on the shell while the desktop
/// animates it away: a copy of the shell shows its placeholder until then,
/// never the frame it drew last.
#[test]
fn a_private_window_that_closed_is_a_placeholder_in_copies() {
    let mut compositor = headless(Box::new(StackPolicy));
    let mut client = mapped_client(
        &mut compositor,
        "org.example.shared",
        (200, 100),
        [255, 0, 0, 255],
    );
    compositor.protect_app("org.example.shared", true);
    // The frame it drew last is kept for its close.
    compositor.draw_frame().expect("a frame");
    client.toplevel.as_ref().unwrap().destroy();
    client.surface.as_ref().unwrap().destroy();
    pump(&mut compositor, &mut [&mut client], |compositor, _| {
        compositor.windows().is_empty()
    });
    assert_eq!(
        compositor.closing_windows().len(),
        1,
        "its last frame is kept"
    );
    compositor.draw_frame().expect("a frame");
    let display = compositor.screenshot("HEADLESS-1").unwrap();
    assert_eq!(
        display.pixel(IN_WINDOW.0, IN_WINDOW.1),
        Some([255, 0, 0, 255]),
        "the person's own display shows it closing"
    );
    let copy = compositor
        .read_back_region("HEADLESS-1", (0, 0, 640, 480))
        .expect("a capture of the output");
    let placeholder = copy
        .pixel(IN_WINDOW.0, IN_WINDOW.1)
        .expect("on the capture");
    assert_ne!(
        placeholder,
        [255, 0, 0, 255],
        "none of the frame it drew last"
    );
    assert_ne!(
        placeholder, BACKGROUND,
        "not what it stands on either: a capture says something is withheld"
    );

    // Once the desktop lets it go, nothing of it is on the shell at all.
    let info = compositor.closing_windows()[0].0.id;
    compositor.release_snapshot(info);
    compositor.draw_frame().expect("a frame");
    let copy = compositor
        .read_back_region("HEADLESS-1", (0, 0, 640, 480))
        .expect("a capture of the output");
    assert_eq!(copy.pixel(IN_WINDOW.0, IN_WINDOW.1), Some(BACKGROUND));
}
