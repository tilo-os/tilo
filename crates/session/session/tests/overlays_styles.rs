//! The tilolang shell's system overlays in every desk style: the quick settings the bar's status
//! opens, the notification centre the bell opens (and the banner a notice rises as), and the
//! launcher the dock's entry opens. Each is the shell's own view of the desk (`apps/tl/shell`),
//! drawn over the same bar and dock whatever style the person shows, and each runs the session's
//! own published actions, so a press, the agent and the REPL change the same thing.
//!
//! These fail without the change:
//!
//! - the launcher listed only the desk's doors, never the installed apps, so a person could type
//!   "no" and Enter found nothing to open;
//! - there was no way to post a notice as a script or an agent (only an app's `notify`, or a
//!   service's own notice), so `session.notify` — and the banner and centre count it feeds — could
//!   not be driven end to end.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::time::Duration;

use serde_json::{Map, Value, json};
use tilo_compositor::{InputEvent, KeyRepeat, OutputConfig};
use tilo_session::{Options, Session};
use tilo_session_core::shell::DeskReading;
use tilo_surface_host::shell_part::DESK_RESOURCE;

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
const PERSON: &str = "Pat";
/// The Escape key, evdev's.
const KEY_ESCAPE: u32 = 1;
/// The Enter key, evdev's.
const KEY_ENTER: u32 = 28;

fn options() -> Options {
    Options {
        presentation: ui::headless::fixture_runner_inputs,
        person: PERSON.to_owned(),
        shell: true,
        services: vec![Box::new(tilo_settings_service::SettingsService::new())],
        // This device's apps, as the launcher reads them: the store `common::app_store` built and
        // pinned for the run, read once here as a device's store is.
        lists: tilo_session::Lists::fixed(tilo_session::AppLists::read()),
        ..Options::default()
    }
}

fn session() -> Session {
    let mut session =
        Session::headless(options(), &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)])
            .expect("session");
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(1),
        rate: 0,
    });
    session
}

/// The desk as the session publishes it on its door: what an agent reads.
fn desk(session: &Session) -> DeskReading {
    serde_json::from_value(
        session
            .read(DESK_RESOURCE)
            .expect("the session publishes its desk"),
    )
    .expect("the desk reads as the shell's desk")
}

#[track_caller]
fn until(session: &mut Session, done: impl FnMut(&mut Session) -> bool) {
    crate::common::until(session, "the shell's answer", done);
}

/// A press on the shell's control named `name` once it stands still: what a person does.
fn press(session: &mut Session, name: &str) {
    crate::common::press(session, name);
}

/// The person's appearance mode, as the settings' own section publishes it.
fn appearance_mode(session: &Session) -> Value {
    session
        .read("settings:settings.appearance")
        .map(|section| section["mode"].clone())
        .unwrap_or_default()
}

/// Show the surface `surface` over the desk through the desk's own `show-surface`, as an agent
/// does, and run frames until it is up.
fn show(session: &mut Session, surface: &str) {
    session
        .invoke(
            DESK_RESOURCE,
            "show-surface",
            &Map::from_iter([("surface".to_owned(), Value::String(surface.to_owned()))]),
        )
        .unwrap();
    until(session, |session| desk(session).here == surface);
}

/// Close the surface `surface` through the desk's own `close-surface`.
fn close(session: &mut Session, surface: &str) {
    session
        .invoke(
            DESK_RESOURCE,
            "close-surface",
            &Map::from_iter([("surface".to_owned(), Value::String(surface.to_owned()))]),
        )
        .unwrap();
    until(session, |session| desk(session).here == "none");
}

/// Press `keycode` and let it up again, a frame apart, as the compositor sends it.
fn tap(session: &mut Session, keycode: u32) {
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::Key { keycode, pressed })
            .unwrap();
        session.step(Duration::from_millis(1)).unwrap();
    }
}

/// Type lower-case letters on the keyboard, one key at a time.
fn type_text(session: &mut Session, text: &str) {
    const ROWS: [(&str, u32); 3] = [("qwertyuiop", 16), ("asdfghjkl", 30), ("zxcvbnm", 44)];
    for letter in text.chars() {
        let keycode = ROWS
            .iter()
            .find_map(|(row, first)| {
                row.chars()
                    .position(|c| c == letter)
                    .map(|at| first + u32::try_from(at).unwrap())
            })
            .unwrap_or_else(|| panic!("no key types {letter:?}"));
        tap(session, keycode);
    }
}

/// Show the style `style` (`strips`, `frames`, `spaces`, `desktop`), the way a person does: the
/// three canvas presets through the bar's own switch (`show_preset`), and the desktop through the
/// chrome's `apply(windows_desktop)` preset, which draws the traditional desktop. Frames until the
/// desk has drawn it and placed its bar.
fn show_style(session: &mut Session, style: &str) {
    if style == "desktop" {
        session
            .run_shell_setting("apply", vec![Value::from("windows_desktop")])
            .unwrap_or_else(|why| panic!("the desk shows {style}: {why}"));
        until(session, |session| {
            crate::common::preset_menu(session).is_none()
        });
    } else {
        let label = match style {
            "strips" => "Strips",
            "frames" => "Frames",
            "spaces" => "Spaces",
            other => panic!("no such style {other}"),
        };
        crate::common::show_preset(session, label);
    }
    // Frames for the new style's views to run and place their controls.
    for _ in 0..30 {
        session.step(Duration::from_millis(16)).unwrap();
    }
}

/// The styles the desk's bar offers, by the name the switch lists each under. The desktop is last:
/// it is the chrome's own preset, so switching away from it mid-run would change the bar too.
fn styles() -> [(&'static str, &'static str); 4] {
    [
        ("strips", "Strips"),
        ("frames", "Frames"),
        ("spaces", "Spaces"),
        ("desktop", "Desktop"),
    ]
}

// ---- Quick settings ------------------------------------------------------------------------

/// In every desk style, the bar's status opens the quick settings, and the Dark mode tile runs
/// the appearance settings' own published action: the mode turns from what it was to the other,
/// as Settings and an agent turn it. The panel closes on Escape.
#[test]
fn quick_settings_opens_and_toggles_dark_mode_in_every_style() {
    let _settled = ui::motion::testing::settled();
    let _home = tilo_test_env::TestHome::new().unwrap();
    crate::common::app_store();
    let mut session = session();
    until(&mut session, |session| {
        session
            .accessible_labels()
            .iter()
            .any(|label| label.starts_with("Showing "))
    });
    for (key, name) in styles() {
        show_style(&mut session, key);
        // The status opens the quick settings: the network's button, named by the link it is on.
        session
            .invoke(
                DESK_RESOURCE,
                "show-surface",
                &Map::from_iter([(
                    "surface".to_owned(),
                    Value::String("quick-settings".to_owned()),
                )]),
            )
            .unwrap();
        until(&mut session, |session| {
            desk(session).here == "quick-settings"
        });
        until(&mut session, |session| {
            session.shell_control("Dark mode").is_some()
        });
        let dark = json!("dark");
        let was_dark = appearance_mode(&session) == dark;
        press(&mut session, "Dark mode");
        until(&mut session, |session| {
            (appearance_mode(session) == dark) != was_dark
        });
        // The tile's press reached the settings, named nowhere but the settings' own action.
        assert!(
            session.shell_refusals().is_empty(),
            "{name}: the dark mode tile reached the settings: {:?}",
            session.shell_refusals()
        );
        // Escape closes it, as a surface over the desk does.
        tap(&mut session, KEY_ESCAPE);
        until(&mut session, |session| desk(session).here == "none");
    }
}

// ---- Notifications -------------------------------------------------------------------------

/// A notice posted through the session's own `notify` rises as the shell's banner (the toast) and
/// shows in the bell's count; the bell opens the centre with it listed, and its Dismiss takes it
/// off. Holds in every desk style: the banner and the centre are the desk's, drawn over whatever
/// style shows.
#[test]
fn a_notice_rises_as_a_banner_and_opens_the_centre_in_every_style() {
    let _settled = ui::motion::testing::settled();
    let _home = tilo_test_env::TestHome::new().unwrap();
    crate::common::app_store();
    let mut session = session();
    until(&mut session, |session| {
        session
            .accessible_labels()
            .iter()
            .any(|label| label.starts_with("Showing "))
    });
    for (key, style) in styles() {
        show_style(&mut session, key);
        // The session posts a notice, as an agent or the REPL does: the desk's own `notify`.
        session
            .invoke(
                DESK_RESOURCE,
                "notify",
                &Map::from_iter([
                    ("title".to_owned(), Value::String("Tea is ready".to_owned())),
                    (
                        "body".to_owned(),
                        Value::String("Steeped four minutes".to_owned()),
                    ),
                    ("from".to_owned(), Value::String("Clocks".to_owned())),
                    ("kind".to_owned(), Value::String("success".to_owned())),
                ]),
            )
            .unwrap_or_else(|error| panic!("{style}: notify: {error:?}"));
        // The banner (the toast) shows it, and the bell counts it.
        until(&mut session, |session| {
            session.shell_says("Tea is ready") && session.shell_says("Notifications, 1 unread")
        });
        // The bell opens the centre, and the notice is listed there.
        session
            .invoke(
                DESK_RESOURCE,
                "show-surface",
                &Map::from_iter([("surface".to_owned(), Value::String("centre".to_owned()))]),
            )
            .unwrap();
        until(&mut session, |session| desk(session).here == "centre");
        until(&mut session, |session| {
            session.shell_control("Tea is ready").is_some()
        });
        // Dismiss takes it off the desk, through the desk's own `dismiss-notice`.
        let id = desk(&session)
            .notices
            .iter()
            .find(|notice| notice.title == "Tea is ready")
            .map_or_else(
                || panic!("{style}: the desk names the notice"),
                |notice| notice.key.clone(),
            );
        session
            .invoke(
                DESK_RESOURCE,
                "dismiss-notice",
                &Map::from_iter([("notice".to_owned(), Value::String(id))]),
            )
            .unwrap();
        until(&mut session, |session| {
            !desk(session)
                .notices
                .iter()
                .any(|notice| notice.title == "Tea is ready")
        });
        assert!(
            session.shell_refusals().is_empty(),
            "{style}: the notice's actions reached the desk: {:?}",
            session.shell_refusals()
        );
        close(&mut session, "centre");
    }
}

// ---- The launcher --------------------------------------------------------------------------

/// The launcher lists the installed apps (Notes among them), typing narrows them, and Enter opens
/// the first: typing "no" and Enter presses the desk's own `open-app` for Notes. Holds in every
/// desk style, the launcher being the shell's one view over all of them.
#[test]
fn the_launcher_lists_apps_filters_and_opens_notes_on_enter() {
    let _settled = ui::motion::testing::settled();
    let _home = tilo_test_env::TestHome::new().unwrap();
    crate::common::app_store();
    let mut session = session();
    until(&mut session, |session| {
        session
            .accessible_labels()
            .iter()
            .any(|label| label.starts_with("Showing "))
    });
    // The apps the session reads from the store: each a `session.application` the launcher lists.
    until(&mut session, |session| {
        desk(session)
            .apps
            .iter()
            .any(|app| app.key == "so.tilo.lang.notes")
    });
    for (key, style) in styles() {
        show_style(&mut session, key);
        show(&mut session, "launcher");
        // The launcher lists Notes before anything is typed: the installed apps are the list,
        // not only the desk's own doors.
        until(&mut session, |session| {
            session.shell_control("Notes").is_some()
        });
        // Typing narrows to Notes: the only app row left named before the desk's own doors.
        let field = crate::common::LAUNCHER_SEARCH;
        press(&mut session, field);
        type_text(&mut session, "no");
        until(&mut session, |session| {
            session.shell_control("Notes").is_some()
        });
        // Enter opens the first app: the desk starts Notes, through its own `open-app`.
        tap(&mut session, KEY_ENTER);
        until(&mut session, |session| {
            desk(session)
                .windows
                .iter()
                .any(|window| window.app_key == "so.tilo.lang.notes")
        });
        assert!(
            session.shell_refusals().is_empty(),
            "{style}: the launcher's Enter reached the desk: {:?}",
            session.shell_refusals()
        );
        // The app opened as a tab of the workspace in front, so the launcher is done: a later
        // style starts from the desk at rest with Notes closed again.
        if desk(&session).here != "none" {
            close(&mut session, "launcher");
        }
    }
}
