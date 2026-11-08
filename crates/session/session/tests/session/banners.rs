//! The banners over the desk: a notification raised shows a banner in the
//! corner of the screen, and it leaves when its time is up.

use super::*;

use crate::desk::held_clock;
use tilo_desktop::notifications::CORNER_INSET;

/// The frame the desk shows now, as RGBA pixels.
fn screen(session: &mut Session) -> image::RgbaImage {
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a picture of the desk");
    image::RgbaImage::from_raw(shot.width, shot.height, shot.rgba).expect("a whole frame")
}

/// How many pixels of `frame` differ from `desk` inside `rect`, whole pixels
/// as `(x0, y0, x1, y1)`.
fn changed_pixels(
    frame: &image::RgbaImage,
    desk: &image::RgbaImage,
    rect: (i32, i32, i32, i32),
) -> usize {
    let mut changed = 0;
    for y in rect.1..rect.3 {
        for x in rect.0..rect.2 {
            if pixel_at(frame, x, y) != pixel_at(desk, x, y) {
                changed += 1;
            }
        }
    }
    changed
}

/// The frame `rect` covers, whole pixels.
fn pixels(rect: ui::kurbo::Rect) -> (i32, i32, i32, i32) {
    #[allow(
        clippy::cast_possible_truncation,
        reason = "a banner on a screen of a few thousand pixels"
    )]
    (
        rect.x0 as i32,
        rect.y0 as i32,
        rect.x1.ceil() as i32,
        rect.y1.ceil() as i32,
    )
}

/// An app's notification, raised over the freedesktop interface as the
/// desktop's daemon takes it, shows a banner in the display's top-right
/// corner, on the screen and not only in what the desktop says it shows;
/// once its four seconds on screen run out, it leaves again.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one test follows one notification from raised to gone"
)]
fn a_raised_notification_shows_a_banner_that_expires() {
    let _settled = ui::motion::testing::settled();
    let _clock = held_clock();
    let bus = PrivateBus::start();
    let wake = tilo_service::LoopWake::default();
    let (side, link) = tilo_session::portal_link::channel(wake.handle());
    let desktop = Rc::new(
        DesktopServices::start(
            &Buses {
                session: Bus::Address(bus.address().to_owned()),
                system: Bus::None,
            },
            &wake.handle(),
            std::sync::Arc::new(tilo_portal::NoCapture),
            &side,
            None,
        )
        .expect("desktop services"),
    );
    let portal = desktop
        .portal()
        .map(|serving| link.with_captures(serving.input_captures().clone()));
    let mut session = crate::common::headless(Options {
        services: desktop.services(),
        wake,
        desktop: Some(Rc::clone(&desktop)),
        portal,
        collab: false,
        ..Options::default()
    });
    settle(&mut session, 8);
    let desk = screen(&mut session);

    // The app's notification waits four seconds on screen for the person.
    let runtime = desktop.runtime();
    let (_app, _id) = runtime.block_on(super::chrome::notify(
        bus.address(),
        "Build finished",
        "All 524 tests passed",
        4_000,
    ));
    pump(&mut session, &mut [], |session, _| {
        !session.banners().shown().stack.is_zero_area()
    });

    // It is on the screen, in the corner, as wide as a banner and off the
    // display's edge by the margin every transient keeps.
    let shown = session.banners().shown();
    let stack = shown.stack;
    assert_eq!(shown.banners.len(), 1, "the notification is one banner");
    assert!(
        stack.x1 <= f64::from(WIDTH) - CORNER_INSET + 0.5,
        "the banner keeps the corner margin: {stack:?}"
    );
    assert!(
        stack.x0 > f64::from(WIDTH) / 2.0,
        "the banner is in the top-right corner: {stack:?}"
    );
    assert!(
        stack.y0 >= f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX),
        "the banner starts under the bar: {stack:?}"
    );
    let raised = screen(&mut session);
    let rect = pixels(stack);
    let area = usize::try_from((rect.2 - rect.0) * (rect.3 - rect.1)).unwrap_or(0);
    let changed = changed_pixels(&raised, &desk, rect);
    assert!(
        changed > area / 5,
        "the banner is painted on the desk: {changed} of {area} pixels changed"
    );
    // What the banner offers answers the person like any window's buttons
    // do: each is a control of the desk, standing on the banner.
    assert!(
        session
            .controls()
            .into_iter()
            .any(|(_, (x, y, w, h))| stack.contains((x + w / 2.0, y + h / 2.0))),
        "the banner's buttons are the desk's controls: {:?}",
        session.controls()
    );

    // Four seconds on screen are its whole life: the clock runs on and the
    // banner leaves, leaving the desk as it was.
    let mut at = ui::time::now();
    let mut left = false;
    for _ in 0..80 {
        at += Duration::from_millis(100);
        ui::time::testing::with_monotonic_time(at, || {
            session.step(Duration::from_millis(1)).expect("step");
        });
        if session.banners().shown().stack.is_zero_area() {
            left = true;
            break;
        }
    }
    assert!(left, "the banner leaves when its time is up");
    let gone = changed_pixels(&screen(&mut session), &desk, rect);
    assert!(
        gone == 0,
        "nothing of the banner is left on the desk: {gone} pixels still differ"
    );
}
