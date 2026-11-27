//! A program's question standing with the banners: a program asks whether its app may reach the
//! internet, the session keeps the call as the person's asked record, and the shell draws that
//! question as a card in the corner of the display, over what the person works on.
//!
//! The card is one panel: its own ground, the question's words inside it and the answers under
//! them. It stands at a banner's width, never shrunk to just its padding, so the words are read
//! and the yes is drawn where a person presses it. The test walks the whole path in a light look
//! and a dark one: the question is asked on the program's own channel, the session raises it
//! through the device's dispatch, the record is drawn by the shell's own views, and pressing the
//! yes answers the call the program waits on.
//!
//! `ASK_BANNER_DISPLAY` (as `WIDTHxHEIGHT`, default 1280x800) lays the whole test out on another
//! display, so the same run proves the banner at a phone's width; `TL_SHELL_SHOTS` keeps the
//! frames it draws.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session outlives every call"
)]

use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_compositor::{OutputConfig, Screenshot};
use tilo_service::{AskReceiver, AskSender, Question};
use tilo_session::{Options, Session};

/// One display's refresh: how often the session asks its clients for their next frame.
const REFRESH: Duration = Duration::from_millis(16);
/// The band the bar takes at the top of the shell, above the room a banner stands in.
const BAR: i32 = 44;
/// The person's settings, which publish the appearance section a look is chosen through.
const APPEARANCE: &str = "settings/appearance";
/// The words the question shows, as its title names what the app reaches.
const ASKED: &str = "wants to use the internet";
/// The question's own line under its title.
const DETAIL: &str = "It may reach any site.";
/// The yes a plain permission question offers.
const YES: &str = "Allow";
/// The way out beside it.
const NO: &str = "Don't allow";

/// The display the test runs on: `ASK_BANNER_DISPLAY`'s `WIDTHxHEIGHT`, else a laptop's.
fn display() -> (i32, i32) {
    static SIZE: std::sync::OnceLock<(i32, i32)> = std::sync::OnceLock::new();
    *SIZE.get_or_init(|| {
        std::env::var("ASK_BANNER_DISPLAY")
            .ok()
            .and_then(|text| {
                let (width, height) = text.split_once('x')?;
                Some((width.parse().ok()?, height.parse().ok()?))
            })
            .unwrap_or((1280, 800))
    })
}

/// A headless session at the test's display, its shell the whole screen, with the person's
/// settings beside it and the channel their programs ask their questions on: the programs' side of
/// it and the `asked` records the session reads are the same channel.
fn session() -> (Session, AskSender, AskReceiver) {
    let (width, height) = display();
    let (asks, questions) = tilo_service::asks::channel();
    let session = crate::common::on_its_daemon(
        &[OutputConfig::new("HEADLESS-1", width, height)],
        Options {
            services: vec![Box::new(settings_sections::service::SettingsService::new())],
            asks: Some((asks.clone(), questions.clone())),
            collab: false,
            ..Options::default()
        },
    );
    (session, asks, questions)
}

/// The question an app asks before it first reaches the internet, as the session asks it of its
/// programs: the kind the app's launch asks in, the app it is asked for, and the sites it declares
/// (`*` is any).
fn network_question(handle: &str) -> Question {
    Question {
        handle: handle.to_owned(),
        kind: "app.network".to_owned(),
        app_id: Some("so.tilo.tube".to_owned()),
        label: "YouTube".to_owned(),
        details: json!({ "hosts": ["*"], "calls": false }),
    }
}

/// Run frames until the shell shows the control `name`.
#[track_caller]
fn until_control(session: &mut Session, name: &str) {
    crate::common::wait_for(session, format!("the control {name:?}"), |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        crate::common::on_screen_at(session, name, display().0, display().1).map(|_| ())
    });
}

/// Run frames until the question `handle` has reached the session that raises it: the program's
/// side of the channel says so, whatever the shell has drawn yet.
#[track_caller]
fn until_raised(session: &mut Session, questions: &AskReceiver, handle: &str) {
    crate::common::wait_for(
        session,
        format!("the question {handle:?} to be asked"),
        |session| {
            session.settle_services();
            session.step(REFRESH).expect("step");
            questions.question(handle).is_some().then_some(())
        },
    );
}

/// Run frames until the shell shows no control named `name`.
#[track_caller]
fn until_gone(session: &mut Session, name: &str) {
    crate::common::wait_for(session, format!("{name} to go"), |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        crate::common::on_screen_at(session, name, display().0, display().1)
            .is_none()
            .then_some(())
    });
}

/// Choose the appearance `theme` in `mode` as Settings does, and run frames until the shell reads
/// the choice: what the next frame is drawn in.
#[track_caller]
fn choose(session: &mut Session, theme: &str, mode: &str) {
    // The settings service publishes its sections on a pass of its own.
    crate::common::wait_for(session, "the appearance section", |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        session.read(APPEARANCE)
    });
    // The frame the shell draws before the switch: the look it shows now.
    let before = frame(session);
    let key = if mode == "light" {
        "light_theme"
    } else {
        "dark_theme"
    };
    for (field, value) in [(key, theme), ("mode", mode)] {
        let arguments: Map<String, Value> = json!({ "value": value })
            .as_object()
            .expect("an object")
            .clone();
        session
            .invoke(
                APPEARANCE,
                &settings_store::workspace::verb(field),
                &arguments,
            )
            .unwrap_or_else(|error| panic!("set {field}: {error:?}"));
    }
    let held = |session: &Session| {
        let read = session
            .read(APPEARANCE)
            .unwrap_or_else(|| panic!("the session publishes the appearance section"));
        read.get(key) == Some(&json!(theme)) && read.get("mode") == Some(&json!(mode))
    };
    crate::common::wait_for(session, format!("the {mode} look"), |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        held(session).then_some(())
    });
    // The shell draws the look on the frames after it reads it, and a narrower display lays its
    // page out again: run frames until the look stands below the bar and then stops changing, so
    // a capture taken now is the look at rest and not one on its way.
    let mut last = frame(session);
    let mut drawn = !same_below_bar(&before, &last);
    for _ in 0..240 {
        session.settle_services();
        for _ in 0..2 {
            session.step(REFRESH).expect("step");
        }
        let next = frame(session);
        if !drawn {
            drawn = !same_below_bar(&before, &next);
            last = next;
            continue;
        }
        if same_below_bar(&last, &next) {
            return;
        }
        last = next;
    }
    assert!(drawn, "the shell draws the {mode} look");
}

/// The frame the session draws now, without keeping it.
fn frame(session: &mut Session) -> Screenshot {
    session
        .compositor()
        .screenshot("HEADLESS-1")
        .expect("a frame of the display")
}

/// Whether two frames stand the same under the bar: the bar's clock may tick between them, and
/// nothing a look changes stands there.
fn same_below_bar(a: &Screenshot, b: &Screenshot) -> bool {
    let (start, width) = (BAR.max(0), a.width);
    let row = usize::try_from(width).unwrap_or(0) * 4;
    let from = usize::try_from(start).unwrap_or(0) * row;
    a.rgba.get(from..) == b.rgba.get(from..)
}

/// Whether `inner` stands inside `outer`, a pixel of rounding either way.
fn inside(outer: (f64, f64, f64, f64), inner: (f64, f64, f64, f64)) -> bool {
    let (ox, oy, ow, oh) = outer;
    let (ix, iy, iw, ih) = inner;
    ix >= ox - 1.0 && iy >= oy - 1.0 && ix + iw <= ox + ow + 1.0 && iy + ih <= oy + oh + 1.0
}

/// The pixels of `rect` as (x, y, colour), on the display the shot is of.
fn pixels(
    shot: &Screenshot,
    rect: (f64, f64, f64, f64),
) -> impl Iterator<Item = (u32, u32, [u8; 4])> + '_ {
    let (width, _) = display();
    let scale = f64::from(shot.width) / f64::from(width);
    let x0 = (rect.0 * scale).round().max(0.0) as u32;
    let y0 = (rect.1 * scale).round().max(0.0) as u32;
    let x1 = (((rect.0 + rect.2) * scale).round() as u32).min(shot.width);
    let y1 = (((rect.1 + rect.3) * scale).round() as u32).min(shot.height);
    (y0..y1)
        .flat_map(move |y| (x0..x1).filter_map(move |x| shot.pixel(x, y).map(|rgba| (x, y, rgba))))
}

/// How strongly what is drawn in `rect` stands out from what holds it: the spread of the pixels'
/// brightness. Words drawn on a ground read as ink over it, and a panel that was laid out with no
/// room for its words paints nothing there.
fn contrast(shot: &Screenshot, rect: (f64, f64, f64, f64)) -> u32 {
    let luma = |rgba: [u8; 4]| {
        (0.2126 * f64::from(rgba[0]) + 0.7152 * f64::from(rgba[1]) + 0.0722 * f64::from(rgba[2]))
            .round() as u32
    };
    let mut low = u32::MAX;
    let mut high = 0;
    for (_, _, rgba) in pixels(shot, rect) {
        let luma = luma(rgba);
        low = low.min(luma);
        high = high.max(luma);
    }
    assert!(low <= high, "pixels inside {rect:?}");
    high - low
}

/// The panel the question stands in, as the pixels show it: the bounds of what the question
/// brought to the top of the display, its frame and shadow with it, and how much it drew. The
/// search stops under the tallest a banner may stand, so a control that changes elsewhere on the
/// display (the bell's badge above the bar, the launcher at its foot) is not taken for the panel.
fn panel(before: &Screenshot, after: &Screenshot) -> ((f64, f64, f64, f64), usize) {
    /// The tallest a question banner stands: two questions at a banner's width and its shadow.
    const BANNER_ROOM: f64 = 520.0;
    let (width, _) = display();
    let scale = f64::from(after.width) / f64::from(width);
    let top = (f64::from(BAR) * scale).round() as u32;
    let bottom = top
        .saturating_add((BANNER_ROOM * scale).round() as u32)
        .min(after.height);
    let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
    let mut count = 0;
    for y in top..bottom {
        for x in 0..after.width {
            if before.pixel(x, y) != after.pixel(x, y) {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
                count += 1;
            }
        }
    }
    assert!(
        count > 0,
        "the question draws nothing in the corner of the display"
    );
    (
        (
            f64::from(x0) / scale,
            f64::from(y0) / scale,
            f64::from(x1 + 1 - x0) / scale,
            f64::from(y1 + 1 - y0) / scale,
        ),
        count,
    )
}

/// What a person gets of the question now: its words, its detail and its yes, all inside one
/// panel that stands at a banner's width against the edge of the display, with the words painted.
fn the_question_stands_as_one_panel(
    session: &mut Session,
    look: &str,
    before: &Screenshot,
) -> (f64, f64, f64, f64) {
    let (width, height) = display();
    let title = session
        .shell_text_bounds(ASKED)
        .unwrap_or_else(|| panic!("the question's words are laid out ({look})"));
    assert!(
        title.2 > 80.0,
        "the question's words have the room to be read ({look}): {title:?}"
    );
    let detail = session
        .shell_text_bounds(DETAIL)
        .unwrap_or_else(|| panic!("the question's detail is laid out ({look})"));
    assert!(
        detail.2 > 80.0,
        "the question's detail has the room to be read ({look}): {detail:?}"
    );
    let yes = crate::common::on_screen_at(session, YES, width, height)
        .unwrap_or_else(|| panic!("the question's {YES:?} stands on screen ({look})"));
    assert!(
        yes.2 > 20.0 && yes.3 > 20.0,
        "the question's {YES:?} has a shape to press ({look}): {yes:?}"
    );

    let after = crate::common::shot(session, &format!("ask-{look}"));
    let (panel, drawn) = panel(before, &after);
    assert!(
        panel.2 > 300.0 && panel.3 > 100.0,
        "the question stands as a banner's card, not a strip of its own padding ({look}): \
         {panel:?}, {drawn} pixels"
    );
    assert!(
        panel.0 + panel.2 <= f64::from(width) && panel.0 >= 0.0,
        "the question stands inside the display ({look}): {panel:?}"
    );
    for (what, rect) in [("question", title), ("detail", detail), ("yes", yes)] {
        assert!(
            inside(panel, rect),
            "the {what} stands inside the question's panel ({look}): {rect:?} in {panel:?}"
        );
    }
    assert!(
        contrast(&after, title) > 48,
        "the question's words are painted inside its panel ({look}): {title:?}"
    );
    assert!(
        contrast(&after, yes) > 48,
        "the question's {YES:?} is painted inside its panel ({look}): {yes:?}"
    );
    panel
}

#[test]
fn a_programs_question_stands_as_a_panel_with_its_words_and_its_yes() {
    let _home = tilo_test_env::TestHome::new().expect("a home of the test's own");
    let _settled = tilo_ui_base::motion::testing::settled();
    let (width, height) = display();
    let (mut session, asks, raised) = session();
    // The shell is up, with the start cards a person sees on an empty shell.
    crate::common::wait_for(&mut session, "the shell", |session| {
        session.settle_services();
        session.step(REFRESH).expect("step");
        crate::common::on_screen_at(session, "Open an app", width, height).map(|_| ())
    });
    // The workspace the shell shows: where an app's question is asked, as its launch asks it.
    let workspace = session
        .handle()
        .read(|tree| {
            tree.active_workspace()
                .map(|workspace| workspace.id().to_string())
        })
        .expect("a workspace is in front");

    // Nothing of the shell's own teaching stands over the question in these captures: the
    // person's switch for the control hints is off, as it is for anyone who turned it off.
    session
        .invoke(
            "settings/control-hints",
            "show-control-hints",
            &Map::from_iter([("enabled".to_owned(), json!(false))]),
        )
        .expect("the control hints are turned off");

    // In the light look, the app asks before it reaches the internet: the session keeps the call
    // as the person's asked record, and the shell draws it where a person is looking.
    choose(&mut session, "paper-light", "light");
    let before = crate::common::shot(&mut session, "ask-before-light");
    let handle = "app.network:light:so.tilo.tube";
    let mut first = asks.in_workspace(&workspace).ask(network_question(handle));
    until_raised(&mut session, &raised, handle);
    until_control(&mut session, YES);
    the_question_stands_as_one_panel(&mut session, "light", &before);

    // The person's yes answers the call the program waits on, and the question goes.
    crate::common::press_at(&mut session, YES, width, height);
    until_gone(&mut session, YES);
    let answered = first
        .until(Some(tilo_clock::now() + Duration::from_secs(30)))
        .expect("the program is answered");
    assert!(
        answered.is_some(),
        "the person's yes answers the program's question"
    );

    // The same in the dark look: the card is as legible there, and its way out declines.
    choose(&mut session, "tilo-dark", "dark");
    let before = crate::common::shot(&mut session, "ask-before-dark");
    let handle = "app.network:dark:so.tilo.tube";
    let mut second = asks.in_workspace(&workspace).ask(network_question(handle));
    until_raised(&mut session, &raised, handle);
    until_control(&mut session, YES);
    the_question_stands_as_one_panel(&mut session, "dark", &before);
    crate::common::press_at(&mut session, NO, width, height);
    until_gone(&mut session, NO);
    let declined = second
        .until(Some(tilo_clock::now() + Duration::from_secs(30)))
        .expect("the program is told");
    assert!(
        declined.is_none(),
        "the person's no answers the program's question with no"
    );
}
