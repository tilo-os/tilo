//! The banners over the shell: a notification raised shows a banner in the
//! corner of the screen, and it leaves when its time is up.

use super::*;

use super::shell::held_clock;
use tilo_session_core::desktop::notifications::Notification;
use tilo_session_core::desktop::overlays::TOPBAR_HEIGHT_PX;
use tilo_session_core::desktop::state::banners::CORNER_INSET;

/// A banner's card is a banner's width, not the display's: the body inside the panel's inset,
/// within a pixel of it.
const BANNER_CARD: f64 = 360.0 - 2.0 * 8.0;

/// A shell over two displays side by side, the first the one in use.
fn two_displays() -> Session {
    let mut session = crate::common::headless_on(
        &[
            OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT),
            OutputConfig::new("HEADLESS-2", WIDTH, HEIGHT).at(WIDTH, 0),
        ],
        Options {
            collab: false,
            ..Options::default()
        },
    );
    settle(&mut session, 8);
    session
}

/// A notice no app sent: what a person is told without an application behind it.
fn notice() -> Notification {
    Notification::new("Backup finished", "12 GB safe").with_app_name("Settings")
}

/// The frame the shell shows now, as RGBA pixels.
fn screen(session: &mut Session) -> image::RgbaImage {
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a picture of the shell");
    image::RgbaImage::from_raw(shot.width, shot.height, shot.rgba).expect("a whole frame")
}

/// How many pixels of `frame` differ from `shell` inside `rect`, whole pixels
/// as `(x0, y0, x1, y1)`.
fn changed_pixels(
    frame: &image::RgbaImage,
    shell: &image::RgbaImage,
    rect: (i32, i32, i32, i32),
) -> usize {
    let mut changed = 0;
    for y in rect.1..rect.3 {
        for x in rect.0..rect.2 {
            if pixel_at(frame, x, y) != pixel_at(shell, x, y) {
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
    let shell = screen(&mut session);

    // The app's notification waits four seconds on screen for the person.
    let runtime = desktop.runtime();
    let (_app, _id) = runtime.block_on(super::chrome::notify(
        bus.address(),
        "Build finished",
        "All 524 tests passed",
        4_000,
    ));
    pump(&mut session, &mut [], |session, _| {
        session.shell_text_bounds("Build finished").is_some()
    });

    // It is on the screen, in the corner, as wide as a banner and off the
    // display's edge by the margin every transient keeps.
    let (x, y, width, height) = session
        .shell_text_bounds("Build finished")
        .expect("the banner stands on the shell");
    let stack = ui::kurbo::Rect::new(x, y, x + width, y + height);
    assert_eq!(
        session.notifications().log().active().len(),
        1,
        "the notification is one banner"
    );
    assert!(
        stack.x1 <= f64::from(WIDTH) - CORNER_INSET + 0.5,
        "the banner keeps the corner margin: {stack:?}"
    );
    assert!(
        stack.x0 > f64::from(WIDTH) / 2.0,
        "the banner is in the top-right corner: {stack:?}"
    );
    assert!(
        stack.y0 >= f64::from(tilo_session_core::desktop::overlays::TOPBAR_HEIGHT_PX),
        "the banner starts under the bar: {stack:?}"
    );
    let raised = screen(&mut session);
    let rect = pixels(stack);
    let area = usize::try_from((rect.2 - rect.0) * (rect.3 - rect.1)).unwrap_or(0);
    let changed = changed_pixels(&raised, &shell, rect);
    assert!(
        changed > area / 5,
        "the banner is painted on the shell: {changed} of {area} pixels changed"
    );
    // What the banner offers answers the person like any window's buttons
    // do: each is a control of the shell, on the banner.
    assert!(
        session.controls().into_iter().any(|control| {
            let (x, y, w, h) = control.rect;
            stack.contains((x + w / 2.0, y + h / 2.0))
        }),
        "the banner's buttons are the shell's controls: {:?}",
        session.controls()
    );

    // Four seconds on screen are its whole life: the clock runs on and the
    // banner leaves, leaving the shell as it was.
    let mut at = ui::time::now();
    let mut left = false;
    for _ in 0..80 {
        at += Duration::from_millis(100);
        ui::time::testing::with_monotonic_time(at, || {
            session.step(Duration::from_millis(1)).expect("step");
        });
        if session.shell_text_bounds("Build finished").is_none() {
            left = true;
            break;
        }
    }
    assert!(left, "the banner leaves when its time is up");
    // One frame on, so what the screen shows is the shell the banner left.
    ui::time::testing::with_monotonic_time(at, || {
        session.step(Duration::from_millis(1)).expect("step");
    });
    let gone = changed_pixels(&screen(&mut session), &shell, rect);
    assert!(
        gone == 0,
        "nothing of the banner is left on the shell: {gone} pixels still differ"
    );
}

/// A banner is the card the display in use shows, in its corner: the display the person works on
/// carries it at a banner's width, the shell's transient margin in from the edge, under the bar;
/// the other display shows none of it.
#[test]
fn a_banner_is_the_corner_card_of_the_display_in_use() {
    let _settled = ui::motion::testing::settled();
    let mut session = two_displays();
    session.notices().post(notice());
    crate::common::until(&mut session, "the banner's words", |session| {
        session.shell_text_bounds("Backup finished").is_some()
    });

    // One display draws it: a part lays out its own screen, so a banner on both would come back
    // twice.
    let bounds = session.shell_text_bounds_all("Backup finished");
    assert_eq!(
        bounds.len(),
        1,
        "only the display in use rises the banner: {bounds:?}"
    );
    let (x, y, width, height) = bounds[0];
    assert!(
        x > f64::from(WIDTH) / 2.0,
        "the banner stands in the corner: {bounds:?}"
    );
    assert!(
        x + width <= f64::from(WIDTH) - CORNER_INSET + 0.5,
        "it keeps the corner margin every transient keeps: {bounds:?}"
    );
    assert!(
        y >= f64::from(TOPBAR_HEIGHT_PX),
        "it starts under the bar: {bounds:?}"
    );
    assert!(
        (width - BANNER_CARD).abs() <= 1.5,
        "the card is a banner's width, not the display's: {bounds:?}"
    );
    assert!(height > 40.0, "the banner is more than a line: {bounds:?}");
}

/// A banner's × takes the card away and keeps the notice: it waits in the notification centre,
/// where the person reads it again, as the Rust shell's banners did.
#[test]
fn a_banners_dismiss_takes_the_card_away_and_keeps_the_notice() {
    let _settled = ui::motion::testing::settled();
    let mut session = crate::common::headless(Options {
        collab: false,
        ..Options::default()
    });
    settle(&mut session, 8);
    let id = session.notices().post(notice());
    crate::common::until(&mut session, "the banner's ×", |session| {
        session.shell_control("Dismiss").is_some()
    });
    assert_eq!(
        session.notifications().log().active().len(),
        1,
        "the notice stands as one banner"
    );

    session
        .press_action("Dismiss", ui::window_targets::TargetAction::Activate)
        .expect("the banner's × is a control a person presses");
    crate::common::until(&mut session, "the card leaves", |session| {
        session.shell_text_bounds("Backup finished").is_none()
    });
    let log = session.notifications();
    let log = log.log();
    assert!(
        log.active().is_empty(),
        "the card is gone from the stack: {:?}",
        log.active()
    );
    let kept = log
        .history_entry(id)
        .unwrap_or_else(|| panic!("the notice waits in the centre: {:?}", log.history()));
    assert_eq!(kept.title, "Backup finished", "the same notice, kept");
}
