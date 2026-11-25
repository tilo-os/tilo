//! The floating layer on the shell: the bar's pill names the activity an app publishes, the card
//! opens from the pill, and a narrow shell keeps its cards in the pill. Each state is a test with
//! its own session, and every state is written as a picture beside the shell's own captures.

use super::*;
use ui::live_activity::{Control, ControlKind, Kind, Published, Summary};

/// The app whose timer runs.
const CLOCKS: &str = "so.tilo.clocks";

/// The bar's height, whole logical pixels: everything above stands in the bar, everything below is
/// the shell the cards float over.
const BAR: u32 = 32;

/// The first row under the shell.
fn shell_bottom() -> u32 {
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

/// A session on a display `width` wide, for the shell a phone shows.
fn session_wide(width: i32) -> Session {
    crate::common::headless_on(
        &[OutputConfig::new("HEADLESS-1", width, HEIGHT)],
        Options {
            collab: false,
            ..Options::default()
        },
    )
}

/// A running timer is the bar's pill, and the pill opens as its card over the shell.
#[test]
fn the_bar_names_the_running_timer_and_its_card_opens_from_the_pill() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::shell::held_clock();
    let dir = shots_dir();
    let mut session = session();
    settle(&mut session, 8);
    let (empty, width) = shot(&mut session);

    // A timer that keeps to the bar: its reading stands in the pill, and nothing stands over the
    // shell.
    show(&session, CLOCKS, tea(), false);
    settle(&mut session, 8);
    let (named, _) = shot(&mut session);
    super::shell::capture_to(&mut session, &dir, "bar-pill");
    assert!(
        changed(&empty, &named, width, 0, BAR) > 100,
        "the bar names the timer: {} pixels of it moved",
        changed(&empty, &named, width, 0, BAR)
    );
    assert_eq!(
        changed(&empty, &named, width, BAR, shell_bottom()),
        0,
        "nothing stands over the shell yet"
    );

    crate::common::press(&mut session, "Show as a card");
    settle(&mut session, 8);
    let (opened, _) = shot(&mut session);
    assert!(
        changed(&named, &opened, width, BAR, shell_bottom()) > 1_000,
        "the card stands over the shell: {} pixels of it moved",
        changed(&named, &opened, width, BAR, shell_bottom())
    );
    super::shell::capture_to(&mut session, &dir, "card");
}

/// A phone's shell has no room beside a window for a card: every activity waits in the pill, and
/// none stands on the shell.
#[test]
fn a_narrow_shell_keeps_its_cards_in_the_pill() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::shell::held_clock();
    let dir = shots_dir();
    let mut session = session_wide(420);
    settle(&mut session, 8);
    let (empty, width) = shot(&mut session);

    show(&session, CLOCKS, tea(), true);
    settle(&mut session, 8);
    let (named, _) = shot(&mut session);
    super::shell::capture_to(&mut session, &dir, "phone-pill");
    assert!(
        changed(&empty, &named, width, 0, BAR) > 100,
        "the bar names the timer on a phone too"
    );
    assert_eq!(
        changed(&empty, &named, width, BAR, shell_bottom()),
        0,
        "no card stands on a phone's shell"
    );
}

/// What `app` was pressed for since the session began: each press as its app reads it.
fn pressed(session: &Session, app: &str) -> Vec<serde_json::Value> {
    session
        .live_activities()
        .pressed_controls(app, 0)
        .map(|(_, pressed)| pressed)
        .unwrap_or_default()
}

/// The command each press asked of `app`, in order.
fn commands(session: &Session, app: &str) -> Vec<String> {
    pressed(session, app)
        .iter()
        .filter_map(|press| press.get("command")?.as_str().map(str::to_owned))
        .collect()
}

/// Wait until the shell shows its control `name`, and answer where it stands.
#[track_caller]
fn standing(session: &mut Session, name: &str) -> (f64, f64, f64, f64) {
    crate::common::wait_for(session, format!("the shell's {name}"), |session| {
        crate::common::on_screen(session, name)
    })
}

/// Wait until the shell no longer shows its control `name`.
#[track_caller]
fn gone(session: &mut Session, name: &str) {
    crate::common::wait_for(session, format!("the shell's {name} to go"), |session| {
        crate::common::on_screen(session, name)
            .is_none()
            .then_some(())
    });
}

/// A call, with its mute and its end.
fn call() -> Summary {
    Summary::new(Kind::Call, "Ada", "12:04")
        .with_id("call-1")
        .with_control(Control::new("mute", "Mute", ControlKind::Secondary))
        .with_control(Control::new("hang-up", "End call", ControlKind::Close))
}

/// A recording of the screen, which its own button stops.
fn recording() -> Summary {
    Summary::new(Kind::Other, "Recording the screen", "0:42")
        .with_id("rec-1")
        .with_control(Control::new("stop", "Stop recording", ControlKind::Primary))
}

/// A timer whose app asks for it to float stands as a card in the display's bottom trailing
/// corner, over the windows and clear of the bar; its own controls reach its app, and its end in
/// the card's header ends it.
#[test]
fn a_floating_timer_stands_in_the_corner_and_is_paused_and_ended_from_its_card() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    settle(&mut session, 8);
    show(&session, CLOCKS, tea(), true);
    let (x, y, w, h) = standing(&mut session, "A minute more");
    super::shell::capture_to(&mut session, &shots_dir(), "card-corner");
    assert!(
        x + w > f64::from(WIDTH) * 0.75 && y + h > f64::from(HEIGHT) * 0.75,
        "the card stands in the bottom trailing corner: {:?}",
        (x, y, w, h)
    );

    crate::common::press(&mut session, "Pause");
    crate::common::press(&mut session, "A minute more");
    crate::common::wait_for(&mut session, "the presses to reach Clocks", |session| {
        (commands(session, CLOCKS) == ["toggle", "add-minute"]).then_some(())
    });
    assert_eq!(
        pressed(&session, CLOCKS)[0]["activity_id"],
        "tea",
        "a press names the activity it was made on"
    );

    crate::common::press(&mut session, "End");
    crate::common::wait_for(&mut session, "the end to reach Clocks", |session| {
        (commands(session, CLOCKS).last().map(String::as_str) == Some("close")).then_some(())
    });
}

/// What keeps to the bar comes out of the pill as a card at a press, and goes back into it.
#[test]
fn the_pill_pulls_a_card_out_and_folds_it_back() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    settle(&mut session, 8);
    show(&session, CLOCKS, tea(), false);
    standing(&mut session, "Show as a card");
    assert!(
        crate::common::on_screen(&session, "A minute more").is_none(),
        "nothing stands over the windows yet"
    );

    crate::common::press(&mut session, "Show as a card");
    standing(&mut session, "A minute more");
    gone(&mut session, "Show as a card");
    crate::common::press(&mut session, "Back to the bar");
    gone(&mut session, "A minute more");
    standing(&mut session, "Show as a card");
}

/// A call and a recording each stand as their app's card: the call ends from the card's header,
/// the recording stops from its own button, and the mute reaches the call.
#[test]
fn a_call_is_ended_and_a_recording_stopped_from_their_cards() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    settle(&mut session, 8);
    show(&session, "so.tilo.calls", call(), true);
    show(&session, "so.tilo.capture", recording(), true);
    standing(&mut session, "Stop recording");
    standing(&mut session, "End call");

    crate::common::press(&mut session, "Mute");
    crate::common::press(&mut session, "End call");
    crate::common::press(&mut session, "Stop recording");
    crate::common::wait_for(&mut session, "the presses to reach their apps", |session| {
        (commands(session, "so.tilo.calls") == ["mute", "hang-up"]
            && commands(session, "so.tilo.capture") == ["stop"])
        .then_some(())
    });
}

/// Three cards at most stand over the windows; the person picks the corner they dock in.
#[test]
fn the_cards_dock_in_the_corner_the_person_chose_three_at_most() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    settle(&mut session, 8);
    show(&session, "so.tilo.calls", call(), true);
    show(&session, "so.tilo.capture", recording(), true);
    show(&session, CLOCKS, tea(), true);
    let mut route = Summary::new(Kind::Navigation, "Home", "200 m").with_id("route-1");
    route.subtitle = "Turn left".into();
    show(&session, "so.tilo.maps", route, true);
    crate::common::wait_for(&mut session, "three cards", |session| {
        (session.shell_controls("Back to the bar").len() == 3).then_some(())
    });

    standing(&mut session, "End call");
    let places = session.shell_controls("Card place");
    crate::common::press_where(&mut session, "a card's place menu", |_| {
        places.first().copied()
    });
    crate::common::press_control(&mut session, "Cards at the top left");
    crate::common::wait_for(&mut session, "the cards at the top left", |session| {
        let (x, y, _, _) = crate::common::on_screen(session, "End call")?;
        (x < f64::from(WIDTH) * 0.25 && y < f64::from(HEIGHT) * 0.5).then_some(())
    });
    super::shell::capture_to(&mut session, &shots_dir(), "cards-top-left");
}

/// The cards are drawn in the person's theme: the same three cards docked in the corner read dark
/// on the dark palette and light on the light one, each written as a picture.
#[test]
fn the_cards_follow_the_persons_theme() {
    let _settled = ui::motion::testing::settled();
    let _clock = super::shell::held_clock();
    let card = |presentation: fn() -> ui::application::ComponentRunnerInputs, theme: &str| {
        let mut session = Session::headless(
            Options {
                presentation,
                collab: false,
                ..Options::default()
            },
            &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        )
        .expect("session");
        settle(&mut session, 8);
        show(&session, "so.tilo.calls", call(), true);
        show(&session, "so.tilo.capture", recording(), true);
        show(&session, CLOCKS, tea(), true);
        crate::common::wait_for(&mut session, "three cards", |session| {
            (session.shell_controls("Back to the bar").len() == 3).then_some(())
        });
        let (x, y, w, h) = standing(&mut session, "End");
        settle(&mut session, 8);
        super::shell::capture_to(&mut session, &shots_dir(), &format!("cards-{theme}"));
        let shot = session
            .compositor()
            .screenshot("HEADLESS-1")
            .expect("the display is shown");
        let scale = f64::from(shot.width) / f64::from(WIDTH);
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "a control's place on the drawn frame"
        )]
        let at = |v: f64| (v * scale) as usize;
        super::shell::region_luminance(
            &shot.rgba,
            (shot.width, shot.height),
            (at(x), at(y), at(w), at(h)),
        )
    };
    let dark = card(ui::headless::fixture_runner_inputs, "dark");
    let light = card(super::shell::light_runner_inputs, "light");
    assert!(
        light > dark + 50.0,
        "a card's header follows the theme: light {light:.0}, dark {dark:.0}"
    );
}

/// A phone's bar pulls what went live out as one card, full width under the bar, and folds it
/// back.
#[test]
fn a_phone_drops_the_card_from_under_the_bar() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_wide(420);
    settle(&mut session, 8);
    show(&session, CLOCKS, tea(), true);
    let at =
        |session: &Session, name: &str| crate::common::on_screen_at(session, name, 420, HEIGHT);
    crate::common::wait_for(&mut session, "the pill", |session| at(session, "Tea, live"));
    assert!(
        at(&session, "A minute more").is_none(),
        "no card stands until asked for"
    );

    crate::common::press_at(&mut session, "Tea, live", 420, HEIGHT);
    let (x, y, w, h) = crate::common::wait_for(&mut session, "the card", |session| {
        at(session, "Back to the bar")?;
        at(session, "A minute more")?;
        at(session, "Tea card")
    });
    super::shell::capture_to(&mut session, &shots_dir(), "phone-card");
    assert!(
        y > f64::from(BAR) && y < f64::from(HEIGHT) / 2.0,
        "under the bar: {y}"
    );
    assert!(
        x < 100.0 && x + w > 300.0,
        "the card spans the phone: {:?}",
        (x, w)
    );
    assert!(
        h < f64::from(HEIGHT) / 2.0,
        "the card is as tall as what it holds, not the phone: {h}"
    );

    crate::common::press_at(&mut session, "Fold Tea back", 420, HEIGHT);
    crate::common::wait_for(&mut session, "the card to fold", |session| {
        at(session, "A minute more").is_none().then_some(())
    });
}

/// A song's card says where it is and what is left, and a press along its bar takes it there.
#[test]
fn a_songs_card_takes_it_along_its_length() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    settle(&mut session, 8);
    let song = Summary::new(Kind::Music, "Song", "")
        .with_id("track-1")
        .with_subtitle("Artist")
        .with_progress(0.25)
        .with_length(200)
        .with_control(Control::new("previous", "Previous", ControlKind::Secondary))
        .with_control(Control::new("toggle", "Pause", ControlKind::Primary).with_icon("pause"))
        .with_control(Control::new("next", "Next", ControlKind::Secondary));
    show(&session, "so.tilo.music", song, true);
    let (x, y, w, h) = standing(&mut session, "Position in Song");
    crate::common::click(&mut session, (x + w * 0.75, y + h / 2.0));
    crate::common::press(&mut session, "Next");
    crate::common::wait_for(&mut session, "the seek to reach the app", |session| {
        let presses = pressed(session, "so.tilo.music");
        let seek = presses.iter().find(|press| press["command"] == "seek")?;
        let fraction = seek["fraction"].as_f64()?;
        ((fraction - 0.75).abs() < 0.08 && presses.iter().any(|press| press["command"] == "next"))
            .then_some(())
    });
}

/// The width a phone's display has in these tests: the one the card under the bar is measured on.
const PHONE_WIDTH: i32 = 420;

/// A session on a display `width` wide, drawing with `presentation`.
fn session_with(
    width: i32,
    presentation: fn() -> ui::application::ComponentRunnerInputs,
) -> Session {
    Session::headless(
        Options {
            presentation,
            collab: false,
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", width, HEIGHT)],
    )
    .expect("session")
}

/// The time's width as the bar's own type lays it out, unconstrained: what the whole clock needs.
fn clock_needs(clock: &str) -> f64 {
    let style = ui::widget::Text::new(clock)
        .with_role(ui::tokens::TextRole::Subheading)
        .numeric()
        .text_style()
        .clone();
    let (needs, _) = ui::font_catalog::fixture_renderer().measure_style(clock, None, &style);
    f64::from(needs)
}

/// The person's clocks in `style` on this thread, as their region settings carry them, so a
/// capture shows a clock of a shape the test names: 12-hour `12:59 PM`, 24-hour `23:59`.
fn clock_style(style: ui::clock::TimeStyle) {
    ui::format::region::install_here(
        Some(ui::format::RegionFormats {
            time: style,
            ..ui::format::RegionFormats::default()
        }),
        None,
    );
}

/// Hold this thread's clock at `hour`:`minute` on the capture's own day, so every picture shows
/// one time and the widest a clock gets (`12:59 PM`, `23:59`).
fn clock_at(hour: i8, minute: i8) -> ui::time::testing::SharedWall {
    let at = ui::format::DateTime::new(2026, 6, 30, hour, minute, 0, 0)
        .ok()
        .and_then(ui::clock::instant_of)
        .expect("the capture's day at a time in the device's zone");
    let wall = ui::time::testing::SharedWall::default();
    wall.follow();
    wall.pin(
        ui::time::UNIX_EPOCH
            + ui::time::Duration::from_millis(
                u64::try_from(at.as_millisecond()).expect("a time after the epoch"),
            ),
    );
    wall
}

/// Check the bar of the frame just captured shows the whole time, its box as wide as its text
/// needs and on the screen, naming the frame in what it says of a failure.
fn whole_clock(session: &Session, name: &str, width: i32, clipped: &mut Vec<String>) {
    let clock = session
        .read("session:time")
        .and_then(|time| time["clock"].as_str().map(str::to_owned))
        .expect("the session publishes the time");
    // What the bar shows, by name and place: a failure says which part took the room.
    let bar: Vec<(String, (f64, f64, f64, f64))> = session
        .accessible_labels()
        .into_iter()
        .filter_map(|label| {
            let rect = session.shell_text_bounds(&label)?;
            (rect.1 < 48.0).then_some((label, rect))
        })
        .collect();
    println!("{name}: {bar:?}");
    let Some((x, y, w, h)) = session.shell_text_bounds(&clock) else {
        clipped.push(format!("{name}: the bar shows no time {clock:?}: {bar:?}"));
        return;
    };
    let needs = clock_needs(&clock);
    if w + 0.5 < needs {
        clipped.push(format!(
            "{name}: {clock:?} stands {w} wide in the bar where its text needs {needs}: \
             {x} {y} {h} {bar:?}"
        ));
    }
    if x < 0.0 || x + w > f64::from(width) || y < 0.0 || y + h > f64::from(HEIGHT) {
        clipped.push(format!(
            "{name}: the clock stands off screen: {:?}",
            (x, y, w, h)
        ));
    }
}

/// A phone's bar is the tight one, and a live activity takes a control of it, but the clock is a
/// glance value: it stands whole on a phone as on a desktop, in either theme, at the widest a
/// clock gets in either of the person's two clock styles. A part that must give way folds first
/// (the pill onto one control); the time is never the one squeezed.
#[test]
fn the_bar_shows_the_whole_clock_on_a_phone_and_a_desktop() {
    let _settled = ui::motion::testing::settled();
    let dir = shots_dir();
    let mut clipped = Vec::new();
    clock_style(ui::clock::TimeStyle::H12);
    let _twelve = clock_at(12, 59);
    for (size, width) in [("phone", PHONE_WIDTH), ("desktop", crate::common::WIDTH)] {
        for (theme, presentation) in [
            (
                "dark",
                ui::headless::fixture_runner_inputs
                    as fn() -> ui::application::ComponentRunnerInputs,
            ),
            (
                "light",
                super::shell::light_runner_inputs
                    as fn() -> ui::application::ComponentRunnerInputs,
            ),
        ] {
            let mut session = session_with(width, presentation);
            settle(&mut session, 8);
            // What is live stands in the bar beside the time, as it does while a timer runs:
            // first as the pill alone, then with its card waiting, as a phone's bar shows both.
            for (state, overlay) in [("pill", false), ("card", true)] {
                show(&session, CLOCKS, tea(), overlay);
                settle(&mut session, 8);
                let name = format!("clock-{size}-{theme}-{state}");
                super::shell::capture_to(&mut session, &dir, &name);
                whole_clock(&session, &name, width, &mut clipped);
            }
        }
    }
    // The 24-hour clock, at the longest it reads, on the phone where the bar is tightest.
    clock_style(ui::clock::TimeStyle::H24);
    let _late = clock_at(23, 59);
    for (theme, presentation) in [
        (
            "dark",
            ui::headless::fixture_runner_inputs
                as fn() -> ui::application::ComponentRunnerInputs,
        ),
        (
            "light",
            super::shell::light_runner_inputs as fn() -> ui::application::ComponentRunnerInputs,
        ),
    ] {
        let mut session = session_with(PHONE_WIDTH, presentation);
        settle(&mut session, 8);
        show(&session, CLOCKS, tea(), true);
        settle(&mut session, 8);
        let name = format!("clock-phone-{theme}-clock-24");
        super::shell::capture_to(&mut session, &dir, &name);
        whole_clock(&session, &name, PHONE_WIDTH, &mut clipped);
    }
    ui::format::region::install_here(None, None);
    assert!(
        clipped.is_empty(),
        "the bar's clock is clipped, though it is short fixed content a glance reads:\n{}",
        clipped.join("\n")
    );
}
