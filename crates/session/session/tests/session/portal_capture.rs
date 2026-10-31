//! The screenshots the portal asks for: the session's own frames, whole
//! displays or the window with focus.

use super::*;
/// A screenshot the portal asks for is the session's frames: the loop is
/// woken and answers with a PNG of every display, side by side.
#[test]
fn the_portals_screenshot_is_a_picture_of_every_display() {
    use tilo_portal::Capture as _;

    let _settled = ui::motion::testing::settled();
    let directory = tempfile::tempdir().unwrap();
    let wake = tilo_service::LoopWake::default();
    let (capture, captures) =
        tilo_session::capture::channel_to(directory.path().to_owned(), wake.handle());
    let external = OutputConfig::new("HDMI-A-1", WIDTH, HEIGHT).at(WIDTH, 0);
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            wake,
            captures: Some(captures),
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("eDP-1", WIDTH, HEIGHT).at(0, 0), external],
    )
    .expect("session");
    session.step(Duration::from_millis(1)).unwrap();
    let asked = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(capture.screenshot(tilo_portal::CaptureTarget::Screen))
    });
    let started = std::time::Instant::now();
    while !asked.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the screenshot was never answered"
        );
        session.step(Duration::from_secs(1)).unwrap();
    }
    let path = asked.join().unwrap().expect("a screenshot");
    assert!(path.starts_with(directory.path()));
    let picture = image::open(&path).unwrap().to_rgba8();
    assert_eq!(
        picture.dimensions(),
        (
            u32::try_from(WIDTH * 2).unwrap(),
            u32::try_from(HEIGHT).unwrap()
        )
    );
}

/// A screenshot of the window with focus is that window's own frame: what
/// its client drew, not the desk around it.
#[test]
fn the_portals_screenshot_of_the_active_window_is_that_window_alone() {
    use tilo_portal::Capture as _;

    let _settled = ui::motion::testing::settled();
    let directory = tempfile::tempdir().unwrap();
    let wake = tilo_service::LoopWake::default();
    let (capture, captures) =
        tilo_session::capture::channel_to(directory.path().to_owned(), wake.handle());
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            wake,
            captures: Some(captures),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    let _client = mapped(&mut session, "org.example.Terminal");
    // The session serves the studio's selector, so what an app may ask for
    // is every target the desktop can take, the window and the area among
    // them. Neither of those two needs the selector here.
    assert_eq!(
        capture.targets(),
        tilo_portal::CaptureTarget::Screen.bit()
            | tilo_portal::CaptureTarget::Window.bit()
            | tilo_portal::CaptureTarget::Area.bit()
            | tilo_portal::CaptureTarget::ActiveWindow.bit()
    );
    let asked = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(capture.screenshot(tilo_portal::CaptureTarget::ActiveWindow))
    });
    let started = std::time::Instant::now();
    while !asked.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the screenshot was never answered"
        );
        session.step(Duration::from_secs(1)).unwrap();
    }
    let path = asked.join().unwrap().expect("a screenshot");
    let picture = image::open(&path).unwrap().to_rgba8();
    let (width, height) = picture.dimensions();
    assert!(
        width < u32::try_from(WIDTH).unwrap() && height < u32::try_from(HEIGHT).unwrap(),
        "the window, not the display: {width}x{height}"
    );
    #[allow(clippy::cast_possible_wrap, reason = "a window's pixels on a screen")]
    let (middle_x, middle_y) = ((width / 2) as i32, (height / 2) as i32);
    assert_eq!(
        pixel_at(&picture, middle_x, middle_y),
        [200, 40, 40, 255],
        "the colour its client drew"
    );
}
