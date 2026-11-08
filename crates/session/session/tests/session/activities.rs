//! The floating layer on the desk: the bar's pill names the activity an app publishes, the card
//! opens from the pill, and a narrow desk keeps its cards in the pill. Each state is a test with
//! its own session, and every state is written as a picture beside the desk's own captures.

use super::*;
use ui::live_activity::{Control, ControlKind, Kind, Published, Summary};

/// The app whose timer runs.
const CLOCKS: &str = "so.tilo.clocks";

/// The bar's height, whole logical pixels: everything above stands in the bar, everything below is
/// the desk the cards float over.
const BAR: u32 = 32;

/// The first row under the desk.
fn desk_bottom() -> u32 {
    u32::try_from(HEIGHT).expect("a display with rows")
}

/// Where the captures are written: under the build's `target` , so a build that runs the suite
/// elsewhere hands them back with its artifacts.
fn shots_dir() -> std::path::PathBuf {
    std::env::var_os("VISUAL_ACTIVITY_OUT").map_or_else(
        || {
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../../target/activities-shots")
        },
        std::path::PathBuf::from,
    )
}

/// The timer the pill names, with the controls its card carries.
fn tea() -> Summary {
    Summary::new(Kind::Timer, "Tea", "4:32")
        .with_id("tea")
        .with_countdown(272)
        .with_progress(0.5)
        .with_control(Control::new("toggle", "Pause", ControlKind::Primary))
        .with_control(Control::new(
            "add-minute",
            "A minute more",
            ControlKind::Secondary,
        ))
        .with_control(Control::new("close", "End", ControlKind::Close))
}

/// Show `summary` on the session's board as `app` publishes it: a card over the windows when
/// `overlay` , a reading in the bar otherwise.
fn show(session: &Session, app: &str, summary: Summary, overlay: bool) {
    let mut published = Published::of(summary);
    published.overlay = overlay;
    session.live_activities().show(
        app,
        Some(serde_json::to_value(published).expect("an activity is JSON")),
    );
}

/// The screen as it stands, and its size.
fn shot(session: &mut Session) -> (Vec<u8>, u32) {
    let shot = session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("the display is shown");
    (shot.rgba, shot.width)
}

/// How many pixels differ between two frames, in the rows `from..to` .
fn changed(before: &[u8], after: &[u8], width: u32, from: u32, to: u32) -> usize {
    let width = usize::try_from(width).expect("a display with columns");
    let stride = width * 4;
    let mut count = 0;
    for row in usize::try_from(from).expect("a row of the display")
        ..usize::try_from(to).expect("a row of the display")
    {
        let at = row * stride;
        for pixel in 0..width {
            let p = at + pixel * 4;
            if before[p..p + 4] != after[p..p + 4] {
                count += 1;
            }
        }
    }
    count
}

/// A left click in the middle of the bar, where the pill stands.
fn click_the_bar(session: &mut Session, width: u32) {
    session
        .compositor()
        .input(InputEvent::PointerMotion {
            x: f64::from(width / 2),
            y: f64::from(BAR / 2),
        })
        .expect("the pointer moves");
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: 0x110,
                pressed,
            })
            .expect("the pointer presses");
    }
}

/// A session on a display `width` wide, for the desk a phone shows.
fn session_wide(width: i32) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", width, HEIGHT)],
    )
    .expect("session")
}

/// A running timer is the bar's pill, and the pill opens as its card over the desk.
#[test]
fn the_bar_names_the_running_timer_and_its_card_opens_from_the_pill() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    let dir = shots_dir();
    let mut session = session();
    settle(&mut session, 8);
    let (empty, width) = shot(&mut session);

    // A timer that keeps to the bar: its reading stands in the pill, and nothing stands over the
    // desk.
    show(&session, CLOCKS, tea(), false);
    settle(&mut session, 8);
    let (named, _) = shot(&mut session);
    assert!(
        changed(&empty, &named, width, 0, BAR) > 100,
        "the bar names the timer: {} pixels of it moved",
        changed(&empty, &named, width, 0, BAR)
    );
    assert_eq!(
        changed(&empty, &named, width, BAR, desk_bottom()),
        0,
        "nothing stands over the desk yet"
    );
    super::desk::capture_to(&mut session, &dir, "bar-pill");

    click_the_bar(&mut session, width);
    settle(&mut session, 8);
    let (opened, _) = shot(&mut session);
    assert!(
        changed(&named, &opened, width, BAR, desk_bottom()) > 1_000,
        "the card stands over the desk: {} pixels of it moved",
        changed(&named, &opened, width, BAR, desk_bottom())
    );
    super::desk::capture_to(&mut session, &dir, "card");
}

/// A phone's desk has no room beside a window for a card: every activity waits in the pill, and
/// none stands on the desk.
#[test]
fn a_narrow_desk_keeps_its_cards_in_the_pill() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::desk::held_clock();
    let dir = shots_dir();
    let mut session = session_wide(420);
    settle(&mut session, 8);
    let (empty, width) = shot(&mut session);

    show(&session, CLOCKS, tea(), true);
    settle(&mut session, 8);
    let (named, _) = shot(&mut session);
    assert!(
        changed(&empty, &named, width, 0, BAR) > 100,
        "the bar names the timer on a phone too"
    );
    assert_eq!(
        changed(&empty, &named, width, BAR, desk_bottom()),
        0,
        "no card stands on a phone's desk"
    );
    super::desk::capture_to(&mut session, &dir, "phone-pill");
}
