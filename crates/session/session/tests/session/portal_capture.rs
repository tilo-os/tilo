//! The screenshots and colour picks the portal asks for: the session's own
//! frames, whole displays or the window with focus, and the pixel under the
//! pointer.

use std::path::PathBuf;
use std::sync::Arc;

use tilo_portal::{Capture as _, CaptureError, CaptureTarget, NO_COLOUR_UNDER_POINTER};
use tilo_session::capture::ScreenCapture;

use super::*;

/// A session answering what the portal asks for, with `outputs` on: its
/// pictures and picks are taken here, its screenshots kept under
/// `directory`.
fn serving(directory: &std::path::Path, outputs: &[OutputConfig]) -> (Session, Arc<ScreenCapture>) {
    let wake = tilo_service::LoopWake::default();
    let (capture, captures) =
        tilo_session::capture::channel_to(directory.to_owned(), wake.handle());
    let session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            wake,
            captures: Some(captures),
            collab: false,
            ..Options::default()
        },
        outputs,
    )
    .expect("session");
    (session, Arc::new(capture))
}

/// Step `session` until what it is asked from another thread answers; the
/// answer.
#[track_caller]
fn answered<T: Send + 'static>(session: &mut Session, asking: std::thread::JoinHandle<T>) -> T {
    let started = std::time::Instant::now();
    while !asking.is_finished() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the session never answered"
        );
        session.step(Duration::from_millis(5)).expect("step");
    }
    asking.join().expect("the asking thread")
}

/// The colour under the pointer, asked from another runtime as an app does.
fn pick(
    capture: &Arc<ScreenCapture>,
    session: &mut Session,
) -> Result<(f64, f64, f64), CaptureError> {
    let capture = Arc::clone(capture);
    answered(
        session,
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("runtime")
                .block_on(capture.color_at_pointer())
        }),
    )
}

/// A picture of `target`, asked from another runtime as an app does; the
/// file it is in.
fn shown_picture(
    capture: &Arc<ScreenCapture>,
    session: &mut Session,
    target: CaptureTarget,
) -> image::RgbaImage {
    let capture = Arc::clone(capture);
    let path: PathBuf = answered(
        session,
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("runtime")
                .block_on(capture.screenshot(target))
        }),
    )
    .expect("a screenshot");
    image::open(&path).expect("a PNG").to_rgba8()
}

/// Move the pointer to `at`, as the person does.
fn point_at(session: &mut Session, at: (f64, f64)) {
    session
        .compositor()
        .input(InputEvent::PointerMotion { x: at.0, y: at.1 })
        .expect("pointer");
    session.step(Duration::from_millis(1)).expect("step");
}

/// The window of `app` and its rectangle `(x, y, w, h)`.
fn window_rect(session: &mut Session, app: &str) -> (i32, i32, i32, i32) {
    window(session.compositor(), app)
        .expect("the window is mapped")
        .1
}

/// The middle of the window of `app`, where the person points to take its
/// colour.
fn middle_of(rect: (i32, i32, i32, i32)) -> (i32, i32) {
    let (x, y, width, height) = rect;
    (x + width / 2, y + height / 2)
}

/// How a pick answers a pixel's colour: red, green and blue, each from 0 to
/// 1.
fn channels(pixel: [u8; 4]) -> (f64, f64, f64) {
    (
        f64::from(pixel[0]) / 255.0,
        f64::from(pixel[1]) / 255.0,
        f64::from(pixel[2]) / 255.0,
    )
}

/// Make the display at `index`, in reading order, the one the person works
/// on, through the desk's own action for it: a window opened then opens
/// there.
fn focus_display(session: &mut Session, index: usize) {
    fn collect(resources: &[tilo_actions::ResourceSpec], displays: &mut Vec<String>) {
        for resource in resources {
            if resource.kind == tilo_desktop::desktop_actions::DISPLAY_KIND {
                displays.push(resource.id.as_str().to_owned());
            }
            collect(&resource.children, displays);
        }
    }
    let mut displays = Vec::new();
    collect(&session.resources(), &mut displays);
    invoke(session, &displays[index], "focus");
}

/// What the client of `mapped` draws.
const DRAWN: [u8; 4] = [200, 40, 40, 255];

/// A screenshot the portal asks for is the session's frames: the loop is
/// woken and answers with a PNG of every display, side by side.
#[test]
fn the_portals_screenshot_is_a_picture_of_every_display() {
    let _settled = ui::motion::testing::settled();
    let directory = tempfile::tempdir().unwrap();
    let external = OutputConfig::new("HDMI-A-1", WIDTH, HEIGHT).at(WIDTH, 0);
    let (mut session, capture) = serving(
        directory.path(),
        &[OutputConfig::new("eDP-1", WIDTH, HEIGHT).at(0, 0), external],
    );
    session.step(Duration::from_millis(1)).unwrap();
    let picture = shown_picture(&capture, &mut session, CaptureTarget::Screen);
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
    let _settled = ui::motion::testing::settled();
    let directory = tempfile::tempdir().unwrap();
    let (mut session, capture) = serving(
        directory.path(),
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    );
    let _client = mapped(&mut session, "org.example.Terminal");
    // The session serves the studio's selector, so what an app may ask for
    // is every target the desktop can take, the window and the area among
    // them. Neither of those two needs the selector here.
    assert_eq!(
        capture.targets(),
        CaptureTarget::Screen.bit()
            | CaptureTarget::Window.bit()
            | CaptureTarget::Area.bit()
            | CaptureTarget::ActiveWindow.bit()
    );
    let picture = shown_picture(&capture, &mut session, CaptureTarget::ActiveWindow);
    let (width, height) = picture.dimensions();
    assert!(
        width < u32::try_from(WIDTH).unwrap() && height < u32::try_from(HEIGHT).unwrap(),
        "the window, not the display: {width}x{height}"
    );
    let (middle_x, middle_y) = middle_of((
        0,
        0,
        i32::try_from(width).unwrap(),
        i32::try_from(height).unwrap(),
    ));
    assert_eq!(
        pixel_at(&picture, middle_x, middle_y),
        DRAWN,
        "the colour its client drew"
    );
}

/// A colour pick is the pixel under the pointer when the app asks: the
/// session reads the same picture a screenshot is of, so the two agree,
/// private windows included.
#[test]
fn the_portals_colour_pick_is_the_pixel_under_the_pointer() {
    let _settled = ui::motion::testing::settled();
    let directory = tempfile::tempdir().unwrap();
    let (mut session, capture) = serving(
        directory.path(),
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    );
    let _client = mapped(&mut session, "org.example.Notes");
    let (at_x, at_y) = middle_of(window_rect(&mut session, "org.example.Notes"));
    point_at(&mut session, (f64::from(at_x), f64::from(at_y)));

    assert_eq!(
        pick(&capture, &mut session),
        Ok(channels(DRAWN)),
        "the colour under the pointer, at once and with no sheet"
    );

    // A private window is picked as a screenshot of the screen shows it:
    // the placeholder, not what the window draws.
    session.compositor().protect_app("org.example.Notes", true);
    let private = pick(&capture, &mut session).expect("a colour");
    assert_ne!(private, channels(DRAWN), "a private window keeps its own");
    let shown = shown_picture(&capture, &mut session, CaptureTarget::Screen);
    assert_eq!(
        private,
        channels(pixel_at(&shown, at_x, at_y)),
        "the pick is the pixel the screenshot has there"
    );
}

/// The pixel comes from the display the pointer is over: with two displays
/// side by side, a pick on the second is the second one's colour, not what
/// the first shows at the same place on it.
#[test]
fn the_portals_colour_pick_is_the_display_under_the_pointer() {
    let _settled = ui::motion::testing::settled();
    let directory = tempfile::tempdir().unwrap();
    let external = OutputConfig::new("HDMI-A-1", WIDTH, HEIGHT).at(WIDTH, 0);
    let (mut session, capture) = serving(
        directory.path(),
        &[OutputConfig::new("eDP-1", WIDTH, HEIGHT).at(0, 0), external],
    );
    session.step(Duration::from_millis(1)).unwrap();
    focus_display(&mut session, 1);
    let _client = mapped(&mut session, "org.example.Right");
    let (at_x, at_y) = middle_of(window_rect(&mut session, "org.example.Right"));
    assert!(at_x >= WIDTH, "the window opened on the second display");
    point_at(&mut session, (f64::from(at_x), f64::from(at_y)));

    assert_eq!(
        pick(&capture, &mut session),
        Ok(channels(DRAWN)),
        "the second display's pixel"
    );
    let shown = shown_picture(&capture, &mut session, CaptureTarget::Screen);
    assert_eq!(
        pick(&capture, &mut session),
        Ok(channels(pixel_at(&shown, at_x, at_y))),
        "where a screenshot of both displays has the pointer"
    );
    assert_ne!(
        pick(&capture, &mut session),
        Ok(channels(pixel_at(&shown, at_x - WIDTH, at_y))),
        "not what the first display shows at that place on it"
    );
}

/// With no pointer over a display there is nothing to pick, and the app is
/// told to move it there rather than that the person cancelled.
#[test]
fn the_portals_colour_pick_with_no_display_under_the_pointer_says_where_to_move_it() {
    let _settled = ui::motion::testing::settled();
    let directory = tempfile::tempdir().unwrap();
    let (mut session, capture) = serving(
        directory.path(),
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    );
    session
        .compositor()
        .remove_output("HEADLESS-1")
        .expect("unplugged");
    session.step(Duration::from_millis(1)).unwrap();
    assert_eq!(
        pick(&capture, &mut session),
        Err(CaptureError::Failed(NO_COLOUR_UNDER_POINTER.to_owned()))
    );
}
