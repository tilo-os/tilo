//! The tilolang shell on the native session (headless backend), its whole screen under the windows:
//! its bar shows the shell the session publishes on its API (the time, the workspace and strip in
//! front, the person, the unread notifications), switching workspace from it brings that workspace
//! to the front, and with nothing changing it does no work.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss,
    clippy::arithmetic_side_effects,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use crate::common::{
    LAUNCHER_SEARCH, OVERVIEW_SEARCH, WORKSPACE_NAME, press, press_at, press_where, type_text,
};
use std::collections::HashMap;
use std::io::BufRead as _;
use std::rc::Rc;
use std::time::Duration;
use tilo_test_env::SpawnUnderEnvLock as _;

use tilo_compositor::{InputEvent, KeyRepeat, OutputConfig};
use tilo_session::desktop_services::{Bus, Buses, DesktopServices};
use tilo_session::{Options, Service as _, Session};
use tilo_session_core::desktop::state::{DesktopNotifications, HostAction, HostActionSource};
use tilo_session_core::shell::{LayoutPersonReading, LayoutWorkspaceReading};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
/// The panel's sections stand one below another, taller than the bar's screen: a screen this tall
/// shows them all, so a test presses one below the fold where a person would after scrolling.
const TALL: i32 = 4000;
const BTN_LEFT: u32 = 0x110;
/// The right button, which points at a thing.
const BTN_RIGHT: u32 = 0x111;
const PERSON: &str = "Pat";

fn options() -> Options {
    Options {
        presentation: ui::headless::fixture_runner_inputs,
        person: PERSON.to_owned(),
        // The shell keeps the person's control hints in their settings, through the settings
        // service's actions, as the session the desktop runs always does.
        services: vec![Box::new(settings_sections::service::SettingsService::new())],
        ..Options::default()
    }
}

fn session() -> Session {
    let mut session =
        Session::headless(options(), &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)])
            .expect("session");
    // A tap here is a press and a release a frame apart, and a frame on a loaded machine can
    // outlast the repeat delay: held keys would repeat ("stuuudio"). No test here holds one.
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(1),
        rate: 0,
    });
    session
}

fn time(session: &Session) -> Time {
    serde_json::from_value(
        session
            .read("session:time")
            .expect("the session publishes the time"),
    )
    .expect("the time reads as the shell's time")
}

/// The time the session publishes.
#[derive(serde::Deserialize)]
struct Time {
    clock: String,
}

/// Whether the device's clock is synchronised, as the session publishes it beside its time: an
/// app that makes a code from a seed and the clock asks this before it trusts what it makes.
#[test]
fn the_session_publishes_whether_its_clock_is_synchronised() {
    let session = session();
    let said = session
        .read("session:clock")
        .expect("the session publishes its clock");
    assert_eq!(
        said["synchronised"],
        serde_json::json!(tilo_clock::is_synchronised()),
        "the fact is the platform's own answer, and none where it cannot say: {said}"
    );
}

fn centre(session: &Session) -> tilo_session::hosted::NotificationCentre {
    serde_json::from_value(
        session
            .read("session:notifications")
            .expect("the session publishes its notifications"),
    )
    .expect("the notifications read as the centre")
}

/// What the bar names of the person's layout: who they are, the workspace and the row in
/// front, every workspace and who else is here.
#[derive(Debug)]
struct Shell {
    person: String,
    workspace: String,
    strip: String,
    workspaces: Vec<LayoutWorkspaceReading>,
    people: Vec<LayoutPersonReading>,
}

/// The person's layout as the session publishes it to the shell.
fn shell(session: &Session) -> Shell {
    let reading = session.layout_reading();
    let front = reading
        .workspaces
        .iter()
        .find(|workspace| workspace.current);
    Shell {
        person: reading.person.clone(),
        workspace: front
            .map(|workspace| workspace.name.clone())
            .unwrap_or_default(),
        strip: reading
            .rows
            .iter()
            .find(|row| row.current && front.is_some_and(|front| front.key == row.workspace))
            .map(|row| row.name.clone())
            .unwrap_or_default(),
        workspaces: reading.workspaces.clone(),
        people: reading.people,
    }
}

/// How many notices the shell names the person has not seen: the count the shell's bell shows of
/// the notices it reads.
fn waiting(session: &Session) -> usize {
    centre(session)
        .notices
        .iter()
        .filter(|notice| !notice.seen)
        .count()
}

/// Run frames until `done` holds.
#[track_caller]
fn until(session: &mut Session, done: impl FnMut(&mut Session) -> bool) {
    crate::common::until(session, "the shell's answer", done);
}

/// Whether the bar says `words` : a control's label carries what it shows and what it opens ("Show
/// strips, now on Main", "Pat (you): your profile"), so naming the words is what the bar does.
fn says(session: &Session, words: &str) -> bool {
    session
        .accessible_labels()
        .iter()
        .any(|label| label.contains(words))
}

/// Keep the screen as `name.png` where `TL_SHELL_SHOTS` names, and its bar's band as `name-bar.png`
/// .
fn keep(session: &mut Session, name: &str) -> tilo_compositor::Screenshot {
    let shot = session.compositor().screenshot("HEADLESS-1").unwrap();
    save(&shot, name);
    shot
}

/// Write `shot` as `name.png` where `TL_SHELL_SHOTS` names, and its bar's band as `name-bar.png` .
fn save(shot: &tilo_compositor::Screenshot, name: &str) {
    if let Some(dir) = std::env::var_os("TL_SHELL_SHOTS") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        image::save_buffer(
            dir.join(format!("{name}.png")),
            &shot.rgba,
            shot.width,
            shot.height,
            image::ColorType::Rgba8,
        )
        .unwrap();
        let band = usize::try_from(shot.width).unwrap().saturating_mul(40 * 4);
        image::save_buffer(
            dir.join(format!("{name}-bar.png")),
            &shot.rgba[..band],
            shot.width,
            40,
            image::ColorType::Rgba8,
        )
        .unwrap();
    }
}

/// How many pixels of `after` differ from `before` by more than a shade: what a person sees change
/// on the screen.
fn changed(before: &tilo_compositor::Screenshot, after: &tilo_compositor::Screenshot) -> usize {
    assert_eq!(
        (before.width, before.height),
        (after.width, after.height),
        "the two frames are of one screen"
    );
    before
        .rgba
        .chunks_exact(4)
        .zip(after.rgba.chunks_exact(4))
        .filter(|(a, b)| {
            a.iter()
                .zip(b.iter())
                .any(|(a, b)| u8::abs_diff(*a, *b) > 8)
        })
        .count()
}

/// The colours the screen shows across the bar's band: the bar draws there,
/// and nothing of the shell does.
fn band_colours(shot: &tilo_compositor::Screenshot) -> usize {
    (0..shot.width)
        .step_by(2)
        .filter_map(|x| shot.pixel(x, 16))
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

tilo_test_env::scenario! {
    fn the_bar_shows_what_the_session_publishes;

    phase the_bar_shows_the_time_the_workspace_its_strip_and_the_person_the_session_publishes;
    {
        let _settled = ui::motion::testing::settled();
        let mut session = session();
        until(&mut session, |session| {
            let shell = shell(session);
            says(session, &time(session).clock)
                && says(session, &shell.workspace)
                && (shell.strip.is_empty() || says(session, &shell.strip))
                && says(session, PERSON)
        });
        let shell = shell(&session);
        assert_eq!(shell.person, PERSON);
        assert!(!shell.workspaces.is_empty(), "the shell has its workspaces");
        assert!(
            shell
                .workspaces
                .iter()
                .any(|workspace| workspace.current && workspace.name == shell.workspace),
            "the workspace in front is the one named: {shell:?}"
        );
        let clock = time(&session).clock;
        for words in [clock.as_str(), shell.workspace.as_str(), PERSON] {
            assert!(
                says(&session, words),
                "the bar says {words:?}: {:?}",
                session.accessible_labels()
            );
        }
        if !shell.strip.is_empty() {
            assert!(says(&session, &shell.strip), "the bar names the strip");
        }
        // The bar draws in its band: its controls stand on the shell there.
        let _shot = crate::common::wait_for(&mut session, "the bar in its band", |session| {
            session.step(Duration::from_millis(16)).expect("step");
            let shot = keep(session, "shell-bar-empty-shell");
            (band_colours(&shot) > 8).then_some(shot)
        });
        // Its controls stand where the bar's band is.
        let (_, y, _, h) = session.shell_control(&shell.workspace).unwrap();
        let band = bar_band(&session);
        assert!(
            y >= band.1 - 0.5 && y + h <= band.1 + band.3 + 0.5,
            "the workspace's control stands in the bar's band: {y} {h} of {band:?}"
        );
    }

    phase an_apps_notification_is_counted_on_the_bar;
    {
        let _settled = ui::motion::testing::settled();
        let bus = SessionBus::start();
        let wake = tilo_service::LoopWake::default();
        let (side, link) = tilo_session::portal_link::channel(wake.handle());
        let desktop = Rc::new(
            DesktopServices::start(
                &Buses {
                    session: Bus::Address(bus.address.clone()),
                    system: Bus::None,
                },
                &wake.handle(),
                std::sync::Arc::new(tilo_portal::NoCapture),
                &side,
                None,
            )
            .unwrap(),
        );
        let portal = desktop
            .portal()
            .map(|serving| link.with_captures(serving.input_captures().clone()));
        let mut session = Session::headless(
            Options {
                services: desktop.services(),
                wake,
                desktop: Some(Rc::clone(&desktop)),
                portal,
                ..options()
            },
            &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        )
        .expect("session");
        until(&mut session, |session| says(session, &time(session).clock));
        assert!(says(&session, "Notifications"), "nothing unread: the bell");
        desktop.runtime().block_on(async {
            let app = zbus::connection::Builder::address(bus.address.as_str())
                .unwrap()
                .build()
                .await
                .unwrap();
            let proxy = zbus::Proxy::new(
                &app,
                "org.freedesktop.Notifications",
                "/org/freedesktop/Notifications",
                "org.freedesktop.Notifications",
            )
            .await
            .unwrap();
            let hints: HashMap<&str, zbus::zvariant::Value<'_>> = HashMap::new();
            let _: u32 = proxy
                .call_method(
                    "Notify",
                    &(
                        "Mail",
                        0_u32,
                        "",
                        "New mail",
                        "From Ada",
                        Vec::<&str>::new(),
                        hints,
                        0_i32,
                    ),
                )
                .await
                .unwrap()
                .body()
                .deserialize()
                .unwrap();
        });
        // The bell stays named with what it shows, and says how many are unread.
        until(&mut session, |session| {
            waiting(session) == 1 && says(session, "Notifications, 1 unread")
        });
        // And the notice rises as the shell's banner, in place of the shell's own banners app,
        // which draws nothing under the shell: the shell names the one it has up, and the shell
        // draws that one.
        until(&mut session, |session| says(session, "New mail"));
        keep(&mut session, "shell-bar-notification");
    }

    phase the_bars_status_shows_the_sounds_published_level;
    // The bar's status is the device's own reading: the sound's level, from the same `sound` resource
    // the quick settings read and change — a change of the resource moves the bar's number, never a
    // copy of it.
    {
        let _home = tilo_test_env::TestHome::new().unwrap();
        let _settled = ui::motion::testing::settled();
        let mut session = session_with_devices(WIDTH, HEIGHT);
        until_settled(&mut session, |session| says(session, &time(session).clock));
        until_settled(&mut session, |session| says(session, "Volume, 40%"));
        keep(&mut session, "shell-bar-status");
        session
            .invoke(
                "sound",
                "set-volume",
                &serde_json::Map::from_iter([("level".to_owned(), serde_json::json!(0.65))]),
            )
            .expect("the sound takes its own volume");
        until_settled(&mut session, |session| says(session, "Volume, 65%"));
        keep(&mut session, "shell-bar-status-louder");
    }

    phase the_bells_count_stands_inside_the_bar;
    // The count's badge stands wholly inside the bell's face, which stands inside the bar: what the
    // person sees when a notification arrives, and where.
    {
        let _settled = ui::motion::testing::settled();
        let mut session = session();
        until(&mut session, |session| says(session, &time(session).clock));
        let before = keep(&mut session, "shell-bar-bell");
        session
            .notices()
            .post_quiet(tilo_session_core::desktop::notifications::Notification::new(
                "New mail", "From Ada",
            ));
        until(&mut session, |session| {
            waiting(session) == 1 && says(session, "Notifications, 1 unread")
        });
        let after = keep(&mut session, "shell-bar-bell-counted");
        let bell = bar_control(&session, "Notifications, 1 unread").expect("the bell");
        let band = bar_band(&session);
        assert!(
            bell.1 >= band.1 && bell.1 + bell.3 <= band.1 + band.3 + 0.5,
            "the bell stands inside the bar's band: {bell:?} of {band:?}"
        );
        // The count sits on the bell's corner, inside the bell's own bounds: a badge is clamped inside
        // the control it counts, so a bell inside the bar's band carries no pixel of its count
        // over the screen's top edge. (What the notice itself draws on the shell is not the bar's; the
        // pixels below the bar change with it.)
        let (inside, _) = changed_in(&before, &after, bell);
        assert!(inside > 20, "the count shows: {inside} pixels");
    }
}

#[test]
fn choosing_a_workspace_from_the_bars_menu_brings_it_to_the_front() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| {
        says(session, &shell(session).workspace)
    });
    let first = shell(&session);
    let first_key = first
        .workspaces
        .iter()
        .find(|workspace| workspace.current)
        .unwrap()
        .key
        .clone();
    // A second workspace, in front.
    session
        .invoke(
            tilo_session::shell::part::CHROME_RESOURCE,
            "new-workspace",
            &serde_json::Map::new(),
        )
        .unwrap();
    until(&mut session, |session| {
        let shell = shell(session);
        shell.workspaces.len() == 2
            && shell.workspace != first.workspace
            && says(session, &shell.workspace)
    });
    let second = shell(&session).workspace;
    // The workspace's control opens the workspaces; choosing the first brings it to the front.
    press(&mut session, &second);
    until(&mut session, |session| {
        session.shell_control(&first.workspace).is_some()
    });
    keep(&mut session, "shell-bar-workspace-menu");
    press(&mut session, &first.workspace);
    crate::common::until(&mut session, "the first workspace in front", |session| {
        session.handle().read(|tree| tree.active.to_string()) == first_key
    });
    assert_eq!(
        session.handle().read(|tree| tree.active.to_string()),
        first_key,
        "the first workspace is in front: {:?} {:?}",
        session.shell_work(),
        session.accessible_labels()
    );
    until(&mut session, |session| says(session, &first.workspace));
    assert_eq!(shell(&session).workspace, first.workspace);
}

/// A menu the person opened stands while the shell changes under it: a workspace made while they are
/// choosing one lists itself there, instead of the menu vanishing under the pointer.
#[test]
fn the_workspaces_menu_stands_while_the_change_feed_under_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| {
        says(session, &shell(session).workspace)
    });
    let first = shell(&session).workspace;
    press(&mut session, &first);
    until(&mut session, |session| {
        session.shell_control("Workspace settings").is_some()
    });
    // The shell changes under the menu: another workspace, in front.
    session
        .invoke(
            tilo_session::shell::part::CHROME_RESOURCE,
            "new-workspace",
            &serde_json::Map::new(),
        )
        .unwrap();
    crate::common::wait_for(
        &mut session,
        "the workspace made under the menu",
        |session| {
            let shell = shell(session);
            (shell.workspaces.len() == 2 && shell.workspace != first)
                .then(|| shell.workspace.clone())
        },
    );
    session.step(Duration::from_millis(16)).unwrap();
    let controls: Vec<String> = session
        .controls()
        .into_iter()
        .map(|control| control.name)
        .filter(|name| name.to_lowercase().contains("work"))
        .collect();
    assert!(
        session
            .shell_control("Workspace settings")
            .is_some_and(|(_, _, w, h)| w > 0.0 && h > 0.0),
        "the menu stands while the shell changes under it: the stack says {:?}; its controls \
         naming a workspace are {controls:?}; the session says: {:?}",
        session.read("desktop:overlays"),
        session.accessible_labels()
    );
    // And still chooses: the workspace in front is the one the person presses there.
    press(&mut session, &first);
    crate::common::until(
        &mut session,
        "the workspace chosen under the menu in front",
        |session| shell(session).workspace == first,
    );
}

/// Right-click the screen point `at`.
fn right_click(session: &mut Session, at: (f64, f64)) {
    session
        .compositor()
        .input(InputEvent::PointerMotion { x: at.0, y: at.1 })
        .unwrap();
    session.step(Duration::from_millis(1)).unwrap();
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: BTN_RIGHT,
                pressed,
            })
            .unwrap();
        session.step(Duration::from_millis(1)).unwrap();
    }
}

/// A point on the shell where nothing of it stands: under the bar and off every control drawn, so
/// a press there is the shell's own ground.
fn shell_ground(session: &Session) -> (f64, f64) {
    let controls: Vec<(f64, f64, f64, f64)> = session
        .controls()
        .into_iter()
        .map(|control| control.rect)
        .collect();
    let covered = |(x, y): (f64, f64)| {
        controls
            .iter()
            .any(|(cx, cy, cw, ch)| x >= *cx && x <= cx + cw && y >= *cy && y <= cy + ch)
    };
    for (across, down) in [
        (0.5, 0.85),
        (0.5, 0.7),
        (0.15, 0.9),
        (0.85, 0.9),
        (0.5, 0.5),
    ] {
        let at = (f64::from(WIDTH) * across, f64::from(HEIGHT) * down);
        if !covered(at) {
            return at;
        }
    }
    panic!("the shell draws a control everywhere: {controls:?}");
}

/// A secondary press on the shell, where nothing of it took the press, opens the workspaces' menu
/// at the pointer — the rows the bar's workspace button opens — and the row chosen runs what it
/// names.
#[test]
fn a_right_click_on_the_shell_opens_the_workspaces_menu() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| {
        says(session, &shell(session).workspace)
    });
    let before = shell(&session).workspaces.len();
    assert!(
        session.shell_control("New workspace").is_none(),
        "no menu stands on the shell at rest"
    );
    let at = shell_ground(&session);
    right_click(&mut session, at);
    until(&mut session, |session| {
        session
            .shell_control("New workspace")
            .is_some_and(|(_, _, w, h)| w > 0.0 && h > 0.0)
    });
    let row = session
        .shell_control("New workspace")
        .expect("the menu's row");
    assert!(
        row.1 >= at.1 - 200.0 && row.1 <= at.1 + 200.0,
        "the menu stands where the right-click landed: {row:?} at {at:?}"
    );
    keep(&mut session, "shell-workspaces-menu-at-pointer");
    press(&mut session, "New workspace");
    until(&mut session, |session| {
        shell(session).workspaces.len() == before + 1
    });
}

#[test]
fn with_nothing_changing_the_bar_does_no_work() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    // What the first frames started has landed.
    for _ in 0..20 {
        session.step(Duration::from_millis(1)).unwrap();
    }
    let minute = || {
        ui::clock::local_now()
            .timestamp()
            .as_second()
            .div_euclid(60)
    };
    loop {
        let before = session.shell_work().expect("the shell's bar runs");
        let at = minute();
        let started = std::time::Instant::now();
        let steps = 200;
        for _ in 0..steps {
            session.step(Duration::from_millis(1)).unwrap();
        }
        let idle = started.elapsed();
        let after = session.shell_work().unwrap();
        if minute() != at {
            // The minute turned over: the clock changed, so the bar worked.
            continue;
        }
        assert_eq!(
            (after.turns, after.drawn),
            (before.turns, before.drawn),
            "idle, the bar's view runs and draws nothing"
        );
        println!(
            "shell bar idle: {steps} session turns in {idle:?}, {} view turns, {} frames drawn",
            after.turns - before.turns,
            after.drawn - before.drawn
        );
        break;
    }
    // A change the bar shows (a workspace made) is one discrete turn.
    let before = session.shell_work().unwrap();
    session
        .invoke(
            tilo_session::shell::part::CHROME_RESOURCE,
            "new-workspace",
            &serde_json::Map::new(),
        )
        .unwrap();
    until(&mut session, |session| shell(session).workspaces.len() == 2);
    for _ in 0..20 {
        session.step(Duration::from_millis(1)).unwrap();
    }
    let after = session.shell_work().unwrap();
    let turns = after.turns - before.turns;
    let drawn = after.drawn - before.drawn;
    assert!(turns >= 1 && drawn >= 1, "the change reached the bar");
    assert!(turns <= 4, "a change is a few turns, not a loop: {turns}");
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    println!(
        "shell bar change: {turns} view turns ({:.2} ms: update {:.2}, commit {:.2}, receive {:.2}, layout {:.2}), {drawn} frames drawn ({:.2} ms)",
        ms(after.turning.saturating_sub(before.turning)),
        ms(after.updating.saturating_sub(before.updating)),
        ms(after.committing.saturating_sub(before.committing)),
        ms(after.receiving.saturating_sub(before.receiving)),
        ms(after.laying_out.saturating_sub(before.laying_out)),
        ms(after.drawing.saturating_sub(before.drawing)),
    );
}

/// The whole session frame for a change the bar shows: a new workspace, the frames the session runs
/// to bring it to the front, at the median. The tool that compares the tilolang bar to the Rust bar
/// holds this number.
#[test]
fn a_new_workspace_frame_is_measured() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    for _ in 0..20 {
        session.step(Duration::from_millis(1)).unwrap();
    }
    // The bar's turns before the change: its own share, measured the same way.
    let mut frame_ms: Vec<f64> = Vec::new();
    // One new workspace per round; the session runs on from where the last left it.
    for _ in 0..25 {
        let workspaces = shell(&session).workspaces.len();
        session
            .invoke(
                tilo_session::shell::part::CHROME_RESOURCE,
                "new-workspace",
                &serde_json::Map::new(),
            )
            .unwrap();
        // The frames it takes: each a step, timed to the frame the change reaches the bar.
        let mut reached = false;
        for _ in 0..8 {
            let started = std::time::Instant::now();
            session.step(Duration::from_millis(1)).unwrap();
            let took = started.elapsed().as_secs_f64() * 1000.0;
            if shell(&session).workspaces.len() > workspaces {
                frame_ms.push(took);
                reached = true;
                break;
            }
        }
        assert!(reached, "the new workspace reaches the shell");
        // What the frames after it started (a fade, a layout) land.
        for _ in 0..8 {
            session.step(Duration::from_millis(1)).unwrap();
        }
    }
    frame_ms.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = frame_ms[frame_ms.len() / 2];
    println!(
        "shell bar session frame (new workspace): median {median:.2} ms over {} changes",
        frame_ms.len()
    );
    assert!(frame_ms.len() == 25, "every change was measured");
}

/// A session bus of its own, as `dbus-run-session` starts one, stopped on drop.
struct SessionBus {
    daemon: std::process::Child,
    address: String,
    _directory: tempfile::TempDir,
}

impl SessionBus {
    fn start() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut daemon = std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--print-address=1"])
            .arg(format!(
                "--address=unix:path={}",
                directory.path().join("bus").display()
            ))
            .stdout(std::process::Stdio::piped())
            .spawn_locked()
            .expect("the session bus needs dbus-daemon, which the builder image installs");
        let mut address = String::new();
        std::io::BufReader::new(daemon.stdout.take().unwrap())
            .read_line(&mut address)
            .unwrap();
        Self {
            daemon,
            address: address.trim().to_owned(),
            _directory: directory,
        }
    }
}

impl Drop for SessionBus {
    fn drop(&mut self) {
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

// ---- the shell's overlays ------------------------------------------------------------------

/// A window of `app` on the shell, its tab's id: a tab of the strip in front.
fn open_window(session: &mut Session, app: &str) -> u64 {
    let handle = session.handle().clone();
    let mut tree = handle.read(Clone::clone);
    let tab = handle.mint_id();
    let workspace = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id() == tree.active)
        .expect("a workspace is in front");
    let strip = workspace.strips.first_mut().expect("a strip is in front");
    let group = handle.mint_id();
    strip.window_groups.push(tilo_desktop_model::WindowGroup {
        id: group,
        tabs: vec![tilo_desktop_model::Tab {
            id: tab,
            title: app.to_owned(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: app.to_owned(),
                label: app.to_owned(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }],
        active_tab: tab,
        width: tilo_desktop_model::WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    handle.replace_tree(tree);
    session.step(Duration::from_millis(1)).unwrap();
    tab
}

/// The shell's surface up now, as the shell's reading names it.
fn here(session: &Session) -> String {
    crate::common::shell_here(session)
}

#[test]
fn the_bars_overview_shows_every_window_and_picking_one_brings_it_front() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    let first = open_window(&mut session, "so.tilo.notes");
    open_window(&mut session, "so.tilo.mail");
    until(&mut session, |session| {
        crate::common::windows(session).len() == 2
    });
    assert_eq!(here(&session), "none", "the shell is at rest");
    // The bar's Overview opens the desktop's overview, as its menu item does.
    session
        .invoke(
            "desktop:overlays",
            "show-surface",
            &serde_json::Map::from_iter([(
                "surface".to_owned(),
                serde_json::Value::String("overview".to_owned()),
            )]),
        )
        .unwrap();
    until(&mut session, |session| here(session) == "overview");
    // The shell's overview is drawn over the shell, naming each window.
    until(&mut session, |session| {
        session
            .accessible_labels()
            .iter()
            .any(|label| label == "Overview")
    });
    for w in crate::common::windows(&session) {
        assert!(
            session.accessible_labels().contains(&w.title),
            "the overview shows {:?}: {:?}",
            w.title,
            session.accessible_labels()
        );
    }
    keep(&mut session, "shell-overview");
    // Picking a window's place goes to it. (Its place may not be on screen until the overlay has
    // been drawn, so the press comes once it is.)
    let notes = crate::common::windows(&session)
        .iter()
        .find(|w| w.key == first.to_string())
        .map(|w| w.title.clone())
        .expect("the notes window is one of them");
    until(&mut session, |session| {
        session.shell_control(&notes).is_some()
    });
    press(&mut session, &notes);
    until(&mut session, |session| {
        session.handle().read(|tree| {
            tree.workspaces
                .iter()
                .flat_map(|workspace| workspace.strips.iter())
                .flat_map(|strip| strip.window_groups.iter())
                .any(|group| {
                    group.tabs.iter().any(|tab| tab.id == first) && group.active_tab == first
                })
        })
    });
}

/// The shell's reading holds every workspace's windows, not only the workspace in front's: a
/// window of another workspace keeps its workspace's key, its share of its strip and its place,
/// so the overview's search over the reading finds it; and the walk Alt+Tab takes still crosses
/// the workspace in front alone.
#[test]
fn the_reading_holds_every_workspaces_windows_and_the_walk_keeps_to_the_front_one() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    let behind = open_window(&mut session, "so.tilo.notes");
    until(&mut session, |session| {
        crate::common::windows(session).len() == 1
    });
    let first = shell(&session);
    let first_key = first
        .workspaces
        .iter()
        .find(|workspace| workspace.current)
        .expect("a workspace is in front")
        .key
        .clone();
    // A second workspace, in front, with a window of its own.
    session
        .invoke(
            tilo_session::shell::part::CHROME_RESOURCE,
            "new-workspace",
            &serde_json::Map::new(),
        )
        .unwrap();
    until(&mut session, |session| {
        shell(session).workspaces.len() == 2 && shell(session).workspace != first.workspace
    });
    let front = open_window(&mut session, "so.tilo.mail");
    until(&mut session, |session| {
        crate::common::windows(session).len() == 2
    });
    let now = crate::common::windows(&session);
    let elsewhere = now
        .iter()
        .find(|window| window.key == behind.to_string())
        .unwrap_or_else(|| panic!("the other workspace's window is read too: {now:?}"));
    assert_eq!(
        elsewhere.workspace_key, first_key,
        "it names the workspace it is in, not the one in front"
    );
    assert_eq!(elsewhere.workspace, first.workspace);
    assert!(
        elsewhere.share > 0.0 && elsewhere.share <= 1.0,
        "it keeps its share of the strip's width: {}",
        elsewhere.share
    );
    assert!(
        !elsewhere.strip_key.is_empty() && !elsewhere.group_key.is_empty(),
        "and the strip and window it stands on"
    );
    assert_eq!(elsewhere.tab_index, 0, "it is the tab in front of it");
    assert!(elsewhere.place > 0, "it has a place in its workspace");
    assert!(!elsewhere.focused, "what has focus is the window in front");
    // A second window in the workspace in front, so the walk has somewhere to go.
    let second = open_window(&mut session, "so.tilo.mail");
    until(&mut session, |session| {
        crate::common::windows(session).len() == 3
    });
    let in_front = [front.to_string(), second.to_string()];
    // The walk crosses the workspace in front alone.
    for _ in 0..3 {
        walk(&mut session, 1, "workspace");
        session.step(Duration::from_millis(300)).unwrap();
        until(&mut session, |session| {
            crate::common::switcher_revealed(session)
        });
        assert!(
            in_front.contains(&reached(&session).key),
            "the walk keeps to the workspace in front: {:?}",
            crate::common::windows(&session)
        );
    }
}

/// The overview's search crosses the workspaces: a window the person left in another workspace is
/// found, named with the workspace it is in, and choosing it goes there.
#[test]
fn the_overviews_search_finds_a_window_of_another_workspace() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    let notes = open_window(&mut session, "so.tilo.notes");
    until(&mut session, |session| {
        crate::common::windows(session).len() == 1
    });
    let behind = shell(&session).workspace;
    session
        .invoke(
            tilo_session::shell::part::CHROME_RESOURCE,
            "new-workspace",
            &serde_json::Map::new(),
        )
        .unwrap();
    until(&mut session, |session| {
        shell(session).workspaces.len() == 2 && shell(session).workspace != behind
    });
    // The reading holds it, in the workspace behind the one in front.
    let read = shell(&session);
    let windows = crate::common::windows(&session);
    let placed = windows
        .iter()
        .find(|w| w.key == notes.to_string())
        .unwrap_or_else(|| panic!("the window behind is read: {windows:?}"));
    assert_eq!(placed.workspace, behind, "it keeps the workspace it is in");
    assert_ne!(
        placed.workspace, read.workspace,
        "the window is not in the workspace in front"
    );
    show(&mut session, "overview");
    let field = OVERVIEW_SEARCH;
    // The window's row, as a reader meets it: the app it shows, then its name.
    let row = "so.tilo.notes, Notes";
    until(&mut session, |session| {
        session.shell_control(field).is_some()
    });
    assert!(
        session.shell_control(row).is_none(),
        "the workspace in front has no window to show: {:?}",
        session.accessible_labels()
    );
    press(&mut session, field);
    type_text(&mut session, "notes");
    // It is found; its row names the workspace it is in.
    until(&mut session, |session| session.shell_control(row).is_some());
    assert!(
        session.shell_says(&behind),
        "the row names the workspace behind: {:?}",
        session.accessible_labels()
    );
    // Choosing it goes there.
    press(&mut session, row);
    until(&mut session, |session| shell(session).workspace == behind);
    assert!(
        crate::common::windows(&session)
            .iter()
            .any(|w| w.key == notes.to_string() && w.focused),
        "the window found is the one in front: {:?}",
        crate::common::windows(&session)
    );
    assert!(
        session.shell_refusals().is_empty(),
        "{:?}",
        session.shell_refusals()
    );
}

#[test]
fn the_launcher_opens_from_the_bar_and_searches_windows() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    open_window(&mut session, "so.tilo.notes");
    until(&mut session, |session| {
        crate::common::windows(session).len() == 1
    });
    // The shell's actions reach the shell's surface through the action API.
    session
        .invoke(
            "desktop:overlays",
            "show-surface",
            &serde_json::Map::from_iter([(
                "surface".to_owned(),
                serde_json::Value::String("launcher".to_owned()),
            )]),
        )
        .unwrap();
    until(&mut session, |session| here(session) == "launcher");
    // The launcher's rows are what it draws: the surface being up says where the shell is, and its
    // labels are the rows a person sees.
    crate::common::wait_for(&mut session, "the launcher's rows", |session| {
        (says(session, "Overview") && says(session, "Notifications")).then_some(())
    });
    keep(&mut session, "shell-launcher");
    // Closing it from the shell's reading puts the shell back at rest.
    session
        .invoke(
            "desktop:overlays",
            "close-surface",
            &serde_json::Map::from_iter([(
                "part".to_owned(),
                serde_json::Value::String("launcher".to_owned()),
            )]),
        )
        .unwrap();
    until(&mut session, |session| here(session) == "none");
}

#[test]
fn the_notification_centre_lists_a_notice_and_it_dismisses() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    // The session posts a notice of its own, as an app's would arrive.
    session.notices().post(
        tilo_session_core::desktop::notifications::Notification::new(
            "Backup finished",
            "12 GB safe",
        )
        .with_app_name("Settings"),
    );
    until(&mut session, |session| !centre(session).notices.is_empty());
    assert_eq!(centre(&session).notices[0].title, "Backup finished");
    session
        .invoke(
            "desktop:overlays",
            "show-surface",
            &serde_json::Map::from_iter([(
                "surface".to_owned(),
                serde_json::Value::String("centre".to_owned()),
            )]),
        )
        .unwrap();
    until(&mut session, |session| here(session) == "centre");
    until(&mut session, |session| says(session, "Backup finished"));
    keep(&mut session, "shell-notification-centre");
    // Dismissing it, as the centre's own Dismiss does, takes it off the shell.
    let id = centre(&session).notices[0].number;
    session
        .invoke(
            "session:notifications",
            "dismiss",
            &serde_json::Map::from_iter([("notification".to_owned(), serde_json::Value::from(id))]),
        )
        .unwrap();
    until(&mut session, |session| centre(session).notices.is_empty());
}

/// The centre's own Dismiss, pressed in the shell's view: the action the view runs is the centre's
/// `dismiss` on the notice the row is for, so the shell's surfaces and an agent dismiss the same
/// way.
#[test]
fn a_press_on_the_centres_dismiss_takes_the_notice_away() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    session.notices().post(
        tilo_session_core::desktop::notifications::Notification::new(
            "Backup finished",
            "12 GB safe",
        )
        .with_app_name("Settings"),
    );
    until(&mut session, |session| !centre(session).notices.is_empty());
    // A notice that arrives is unseen: the shell counts what the person has not seen, and its
    // bell says so.
    until(&mut session, |session| waiting(session) == 1);
    assert!(
        !centre(&session).notices[0].seen,
        "a fresh notice is unseen"
    );
    session
        .invoke(
            "desktop:overlays",
            "show-surface",
            &serde_json::Map::from_iter([(
                "surface".to_owned(),
                serde_json::Value::String("centre".to_owned()),
            )]),
        )
        .unwrap();
    until(&mut session, |session| says(session, "Backup finished"));
    // The centre opened, so the person saw them: every notice is seen, and the count the bell
    // shows is none.
    until(&mut session, |session| {
        waiting(session) == 0 && centre(session).notices.iter().all(|notice| notice.seen)
    });
    // The view draws each notice with its Dismiss; pressing it runs the centre's `dismiss` on
    // that notice.
    press(&mut session, "Dismiss");
    until(&mut session, |session| centre(session).notices.is_empty());
    assert!(
        session.shell_refusals().is_empty(),
        "the centre's Dismiss reached the shell: {:?}",
        session.shell_refusals()
    );
    until(&mut session, |session| {
        says(session, "Nothing to catch up on")
    });
}

#[test]
fn alt_tab_walks_the_windows_and_commits_to_the_one_it_reached() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    open_window(&mut session, "so.tilo.notes");
    open_window(&mut session, "so.tilo.mail");
    until(&mut session, |session| {
        crate::common::windows(session).len() == 2
    });
    // The shell's order, the window in front first: Alt+Tab opens on the next one.
    let before = crate::common::windows(&session);
    let front = before
        .iter()
        .position(|window| window.focused)
        .expect("a window is in front");
    let next = before
        .get((front + 1) % before.len())
        .expect("there is a window after it")
        .key
        .clone();
    let key = |keycode: u32, pressed: bool| InputEvent::Key { keycode, pressed };
    let plain = keep(&mut session, "shell-before-switcher");
    session.compositor().input(key(56, true)).unwrap();
    session.step(Duration::from_millis(1)).unwrap();
    session.compositor().input(key(15, true)).unwrap();
    until(&mut session, |session| here(session) == "switcher");
    // The switcher lifts the window its walk reached.
    until(&mut session, |session| {
        crate::common::windows(session)
            .iter()
            .any(|window| window.key == next && window.focused)
    });
    // The panel comes up on the frame after the reading moves: its keys are drawn with it.
    until(&mut session, |session| says(session, "Go there"));
    // Its keys are named with it, and stand where the frame that draws them lays them out.
    let keys = crate::common::wait_for(&mut session, "the keys in the switcher", |session| {
        let labels = session.accessible_labels();
        let keys = labels.iter().find(|label| label.contains("Alt + Tab"))?;
        session.shell_control(keys)
    });
    let held = keep(&mut session, "shell-switcher");
    // The switcher names every window it walks, each with the app it shows, and the keys stand
    // under them rather than over their titles.
    let labels = session.accessible_labels();
    let walked = crate::common::windows(&session);
    let mut titles = Vec::new();
    for window in &walked {
        // The window's name as a person reads it: its own title, or its app's name where the
        // title is only a spelling of it (the id a launch announces). The switcher names each
        // card by that one name, and its title stands under it.
        let name = tilo_desktop_search::names::window_name_of(
            &window.app_key,
            &window.origin_app,
            &window.title,
        );
        let named = labels
            .iter()
            .filter(|l| l.as_str() == name.as_str())
            .count();
        assert!(named > 0, "the switcher names {name:?}: {labels:?}");
        let bounds = session
            .shell_control(&name)
            .unwrap_or_else(|| panic!("{name:?} stands in the switcher"));
        titles.push(bounds);
    }
    for title in &titles {
        assert!(
            keys.1 >= title.1 + title.3,
            "the keys under the windows stand below every title, not over it: keys {keys:?} \
             titles {titles:?}"
        );
    }
    // Holding Alt shows the switcher: the screen is not the shell's own.
    let drawn = changed(&plain, &held);
    assert!(
        drawn > plain.rgba.len() / 4 / 100,
        "holding Alt+Tab draws the switcher: {drawn} pixels differ from the shell"
    );
    // Letting Alt go goes to it, and the shell is at rest again.
    session.compositor().input(key(15, false)).unwrap();
    session.step(Duration::from_millis(1)).unwrap();
    session.compositor().input(key(56, false)).unwrap();
    until(&mut session, |session| here(session) == "none");
    // The panel goes down on the frame after the reading moves, as it came up: the shell is kept
    // once a frame is drawn without it.
    until(&mut session, |session| !says(session, "Go there"));
    let rested =
        crate::common::wait_for(&mut session, "the switcher to leave the shell", |session| {
            let shot = session.compositor().screenshot("HEADLESS-1").ok()?;
            (changed(&held, &shot) > drawn / 2).then_some(shot)
        });
    save(&rested, "shell-after-switcher");
    assert!(
        changed(&held, &rested) > drawn / 2,
        "letting Alt go takes the switcher down again"
    );
    assert_eq!(
        crate::common::windows(&session)
            .iter()
            .find(|window| window.focused)
            .map(|window| window.key.clone()),
        Some(next),
        "the window Alt+Tab walked to is in front: {:?}",
        session.shell_refusals()
    );
}

/// Alt+Tab's press on the shell's API: the walk `steps` windows on, in `scope` .
fn walk(session: &mut Session, steps: i64, scope: &str) {
    session
        .invoke(
            "desktop:overlays",
            "walk-switcher",
            &serde_json::Map::from_iter([
                ("steps".to_owned(), serde_json::Value::from(steps)),
                (
                    "scope".to_owned(),
                    serde_json::Value::String(scope.to_owned()),
                ),
            ]),
        )
        .unwrap_or_else(|why| panic!("the walk starts: {why:?}"));
}

/// The window the switcher lifts, as the shell reads it: the one the walk reached while the panel
/// shows, and the window in front while the panel is down.
fn reached(session: &Session) -> tilo_session::windows::Window {
    crate::common::windows(session)
        .into_iter()
        .find(|window| window.focused)
        .expect("a window is in front")
}

/// [`reached`], by key.
fn lifted(session: &Session) -> String {
    reached(session).key
}

/// A flick past Alt+Tab switches windows and flashes nothing: the shell draws the same screen until
/// the walk outlasts the reveal delay, and then the panel comes up on its own — the one deadline
/// the session hands the shell — with the window the walk reached lifted in it.
#[test]
fn a_flick_shows_no_panel_and_holding_the_walk_outlasts_its_delay() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    open_window(&mut session, "so.tilo.notes");
    open_window(&mut session, "so.tilo.mail");
    until(&mut session, |session| {
        crate::common::windows(session).len() == 2
    });
    let before = crate::common::windows(&session);
    let front = before
        .iter()
        .position(|window| window.focused)
        .expect("a window is in front");
    let front_key = before[front].key.clone();
    let next = before[(front + 1) % before.len()].key.clone();
    let plain = keep(&mut session, "shell-flick");

    walk(&mut session, 1, "workspace");
    assert_eq!(crate::common::shell_here(&session), "switcher");
    assert!(
        !crate::common::switcher_revealed(&session),
        "a flick shorter than the reveal delay shows no panel"
    );
    assert_eq!(
        lifted(&session),
        front_key,
        "with the panel down the walk lifts nothing, so the shell is as it was"
    );
    let down = keep(&mut session, "shell-flick-panel-down");
    let drawn = changed(&plain, &down);
    assert!(
        drawn < plain.rgba.len() / 4 / 100,
        "a flick draws nothing over the shell: {drawn} pixels differ"
    );

    // Held past the delay, the panel comes up with the window the walk reached lifted.
    session.step(Duration::from_millis(300)).unwrap();
    until(&mut session, |session| {
        crate::common::switcher_revealed(session)
    });
    assert_eq!(
        lifted(&session),
        next.clone(),
        "the panel lifts the window the walk reached"
    );
    let up = keep(&mut session, "shell-flick-held");
    assert!(
        changed(&plain, &up) > plain.rgba.len() / 4 / 100,
        "the panel shows over the shell once the walk outlasts the delay"
    );

    // Letting go goes there, and the panel is down again.
    session
        .invoke(
            "desktop:overlays",
            "commit-switcher",
            &serde_json::Map::new(),
        )
        .unwrap();
    assert_eq!(crate::common::shell_here(&session), "none");
    assert!(!crate::common::switcher_revealed(&session));
    // The window comes to the front on the frames after letting go.
    until(&mut session, |session| lifted(session) == next);
    assert_eq!(
        lifted(&session),
        next,
        "the flick went to the window before the one in front: {:?}",
        session.shell_refusals()
    );
}

/// The grave key under Alt walks only the windows of the app in front; the same walk in the
/// workspace's scope crosses into another app's, and letting go goes to whichever it reached.
#[test]
fn alt_backtick_walks_only_the_front_apps_windows() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    for app in [
        "so.tilo.notes",
        "so.tilo.notes",
        "so.tilo.mail",
        "so.tilo.mail",
    ] {
        open_window(&mut session, app);
    }
    until(&mut session, |session| {
        crate::common::windows(session).len() == 4
    });
    let front = reached(&session);

    // The walk over this app's windows never leaves it, however far it walks.
    walk(&mut session, 1, "app");
    session.step(Duration::from_millis(300)).unwrap();
    until(&mut session, |session| {
        crate::common::switcher_revealed(session)
    });
    assert_eq!(
        session.read("desktop:overlays").unwrap()["switcher_scope"],
        "app"
    );
    let first = reached(&session);
    assert_eq!(
        first.app_key, front.app_key,
        "Alt+` keeps to the app in front: it reached {:?}",
        first.key
    );
    assert_ne!(
        first.key, front.key,
        "it walked on to the other window of the app"
    );
    walk(&mut session, 1, "app");
    assert_eq!(
        reached(&session).key,
        front.key,
        "an app of two windows cycles between them and reaches no other app's"
    );
    walk(&mut session, 1, "app");
    assert_eq!(reached(&session).key, first.key);

    // The workspace's walk crosses into the other app, within three presses: the app in front holds
    // only two of the shell's four windows.
    let mut crossed = None;
    for press in 1..=3 {
        walk(&mut session, 1, "workspace");
        assert_eq!(
            session.read("desktop:overlays").unwrap()["switcher_scope"],
            "workspace"
        );
        // The walk in the other scope starts again, so the panel comes up on holding it.
        session.step(Duration::from_millis(300)).unwrap();
        until(&mut session, |session| {
            crate::common::switcher_revealed(session)
        });
        let window = reached(&session);
        if window.app_key != front.app_key {
            crossed = Some(window);
            break;
        }
        assert!(
            press < 3,
            "the workspace's walk reaches another app's window"
        );
    }
    let crossed = crossed.expect("the workspace's walk reaches another app's window");

    // Letting go goes to the window the workspace's walk reached.
    session
        .invoke(
            "desktop:overlays",
            "commit-switcher",
            &serde_json::Map::new(),
        )
        .unwrap();
    assert_eq!(here(&session), "none");
    until(&mut session, |session| lifted(session) == crossed.key);
    assert_eq!(lifted(&session), crossed.key);
}

/// Who is here is one button of faces — up to three, then a face counting the rest — whose name
/// names the people, so no words take the bar's room at any width; pressing it calls the session's
/// own `toggle-people` , and the roster opens and lists them.
#[test]
fn a_persons_presence_shows_their_face_and_opens_the_roster() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    // Nobody here: the people button alone.
    let people = crate::common::wait_for(&mut session, "the people button", |session| {
        bar_control(session, "People")
    });
    assert!(
        session.shell_control("Mia Clayton").is_none(),
        "no face of a person who is not here"
    );
    // Someone here, as the peer network lists them: their face, named for assistive
    // technology, on the one button that opens the roster.
    set_here(&session, 1);
    until(&mut session, |session| says(session, "Mia Clayton is here"));
    let one = bar_control(&session, "Mia Clayton is here").expect("one face, named");
    assert!(
        one.2 < 60.0,
        "the group is faces, not the words that name them: {} wide",
        one.2
    );
    assert!(
        session.shell_control("Mia Clayton").is_some(),
        "each face on the bar is its own control, opening that person's card: {:?}",
        session.accessible_labels()
    );
    // Three here stand three faces, a face's step wider each; the fourth takes the face at the
    // group's end that counts the rest, and everyone past it is counted there, so a crowd takes no
    // more of the bar than four here.
    let mut widths = vec![one.2];
    for (count, label) in [
        (2, "Mia Clayton and Sam Okafor are here"),
        (3, "Mia Clayton and 2 others are here"),
        (4, "Mia Clayton and 3 others are here"),
        (5, "Mia Clayton and 4 others are here"),
    ] {
        set_here(&session, count);
        widths.push(
            crate::common::wait_for(&mut session, label, |session| bar_control(session, label)).2,
        );
    }
    assert!(
        widths[2] - widths[1] > 1.0 && widths[3] - widths[2] > 1.0,
        "a face joins the group, then the count's own face: {widths:?}"
    );
    assert!(
        (widths[4] - widths[3]).abs() <= 1.0,
        "a crowd is counted, not drawn: {widths:?}"
    );
    assert!(
        people.2 < 40.0,
        "nobody here: the glyph alone, {} wide",
        people.2
    );
    // The button opens who is here, where each person has their own name.
    assert!(
        !session
            .accessible_labels()
            .iter()
            .any(|label| label == "Mia Clayton"),
        "nobody's name stands anywhere while the roster is closed: {:?}",
        session.accessible_labels()
    );
    press(&mut session, "Mia Clayton and 4 others are here");
    until(&mut session, |session| {
        let labels = session.accessible_labels();
        HERE.into_iter()
            .all(|name| labels.iter().any(|label| label == name))
    });
    keep(&mut session, "shell-people-roster");
    // This session reaches no device daemon, so no workspace answers whether this person may
    // invite: the list offers no invite at all, where a session that may offers one
    // (`the_people_list_offers_the_invite_only_where_this_person_may_invite`).
    assert!(
        session.shell_control("Invite someone").is_none(),
        "no invite without a workspace that lets this person invite: {:?}",
        session.accessible_labels()
    );
    // The action such a button runs opens the session's own sheet; what the sheet draws is its
    // own, not the list's.
    session
        .invoke("session:invite", "show-invite", &serde_json::Map::new())
        .expect("the invite sheet opens");
}

// ---- the roster's rows and its header ------------------------------------------------------

/// Someone here, as the peer network lists them: a person by their device's presence, or software
/// by the member it is (an agent, or one of the workspace's bots) and the app it works in.
fn someone_here(
    at: usize,
    name: &str,
    member: Option<collab_types::MemberId>,
    app: &str,
) -> tilo_session_core::desktop::here::LiveMember {
    tilo_session_core::desktop::here::LiveMember {
        publisher: format!("{:064x}", at),
        kind: if member.is_some() {
            "agent".to_owned()
        } else {
            "person".to_owned()
        },
        label: name.to_owned(),
        member,
        focus: (!app.is_empty()).then(|| tilo_session_core::desktop::here::Focus {
            app: app.to_owned(),
            ..tilo_session_core::desktop::here::Focus::default()
        }),
        ..tilo_session_core::desktop::here::LiveMember::default()
    }
}

/// Someone nobody named, here as the person `person` (their user root, hex), the workspace's policy
/// having admitted them.
fn someone_unnamed(person: &str) -> tilo_session_core::desktop::here::LiveMember {
    tilo_session_core::desktop::here::LiveMember {
        publisher: person.to_owned(),
        kind: "person".to_owned(),
        label: String::new(),
        user: Some(person.to_owned()),
        ..tilo_session_core::desktop::here::LiveMember::default()
    }
}

/// Who is here, as the peer network lists them.
fn set_the_people(session: &Session, people: Vec<tilo_session_core::desktop::here::LiveMember>) {
    session
        .sync()
        .expect("the session takes part in the app wire")
        .here()
        .borrow_mut()
        .set_members(None, Some(people));
}

/// Who is here, as the session publishes it (`session:people`).
#[derive(serde::Deserialize)]
struct PeopleReading {
    #[serde(default)]
    rows: Vec<SomeoneReading>,
    #[serde(default)]
    following: String,
    #[serde(default)]
    waved_to: String,
    #[serde(default)]
    may_invite: bool,
}

/// One row of the roster, as the session publishes it.
#[derive(serde::Deserialize)]
struct SomeoneReading {
    #[serde(default)]
    key: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    agent: bool,
    #[serde(default)]
    bot: bool,
    #[serde(default)]
    automatic: bool,
    #[serde(default)]
    call: String,
}

fn people_reading(session: &Session) -> PeopleReading {
    serde_json::from_value(
        session
            .read("session:people")
            .expect("the session publishes who is here"),
    )
    .expect("who is here reads as the roster's reading")
}

/// Open the roster as its own action does, the way the bar's faces ask for it.
fn open_who_is_here(session: &mut Session) {
    session
        .invoke("session:people", "show", &serde_json::Map::new())
        .expect("the roster opens");
}

/// The people list's own rows and headings saying `label`, by where they stand, and what they say
/// of themselves: a row carries the lines under its name with the name ("Agent, Fieldwork, In
/// Notes"), so a row answers to the name among them. A person's name is also said by the bar's face
/// and by the control at the row's far end, which are no rows and stand elsewhere.
fn people_nodes(session: &Session, label: &str) -> Vec<(f64, f64, f64, f64, Option<String>)> {
    session
        .a11y_windows()
        .into_iter()
        .flat_map(|(_, _, tree)| {
            tree.nodes
                .iter()
                .filter(|(_, node)| {
                    matches!(
                        node.role(),
                        ui::accesskit::Role::ListItem | ui::accesskit::Role::Heading
                    ) && node.label().is_some_and(|said| says_that(said, label))
                })
                .filter_map(|(_, node)| {
                    let rect = node.bounds()?;
                    (rect.width() > 0.0 && rect.height() > 0.0).then(|| {
                        (
                            rect.x0,
                            rect.y0,
                            rect.x1,
                            rect.y1,
                            node.description().map(str::to_owned),
                        )
                    })
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// What each control the people list draws under `label` says of itself whole: its name and the
/// line under it, as a screen reader reads the two.
fn people_labels(session: &Session, label: &str) -> Vec<String> {
    session
        .a11y_windows()
        .into_iter()
        .flat_map(|(_, _, tree)| {
            tree.nodes
                .iter()
                .filter_map(|(_, node)| node.label().map(str::to_owned))
                .filter(|said| says_that(said, label))
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Whether a control named `said` is the one named `label`: a row carries the lines it draws with
/// the name it goes by, and software another word before it, so the name is one of the parts it
/// reads as ("Agent Fieldwork, In Notes" is Fieldwork's row).
fn says_that(said: &str, label: &str) -> bool {
    said == label
        || said.split(", ").any(|part| {
            part == label
                || part
                    .strip_suffix(label)
                    .is_some_and(|before| before.ends_with(' ') && !before.trim_end().contains(' '))
        })
}

/// Where the people list's single control labelled `label` stands, in the session's coordinates.
fn people_place(session: &Session, label: &str) -> (f64, f64, f64, f64) {
    let places = people_nodes(session, label);
    assert_eq!(
        places.len(),
        1,
        "the roster draws one {label:?}: {places:?}; it says {:?}",
        session.accessible_labels()
    );
    let (x0, y0, x1, y1, _) = places[0];
    (x0, y0, x1, y1)
}

/// The window the workspace in front shows: the active strip's first.
fn the_window_in_front(session: &Session) -> Option<tilo_desktop_model::TabId> {
    session.handle().read(|tree| {
        let workspace = tree.active_workspace()?;
        let strip = tree.active_strip_for(workspace)?;
        workspace
            .strips
            .iter()
            .find(|held| held.id == strip)?
            .window_groups
            .first()
            .map(|group| group.active_tab)
    })
}

/// The roster's header names the workspace in front and counts who is here besides this person,
/// counting each row as the list counts it; its rows stand people first, software after, each group
/// under its heading where both are here.
#[test]
fn the_people_list_header_says_the_workspace_and_groups_people_ahead_of_software() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    open_who_is_here(&mut session);
    // Nobody else: the header counts this person alone, and names the workspace the roster is of.
    until(&mut session, |session| says(session, "Just you"));
    let workspace = shell(&session).workspace;
    assert!(
        !workspace.is_empty()
            && people_nodes(&session, &workspace)
                .iter()
                .any(|(x0, _, _, _, _)| *x0 > f64::from(WIDTH) / 2.0),
        "the header names the workspace in front ({workspace:?}), at the panel's side of the \
         screen: {:?}",
        people_nodes(&session, &workspace)
    );
    // One person here: counted, never named, where the count says who is here.
    set_the_people(&session, vec![someone_here(1, "Mia Clayton", None, "")]);
    until(&mut session, |session| says(session, "You and 1 other"));
    assert!(
        !says(&session, "Agents"),
        "with only people here no group needs a heading: {:?}",
        session.accessible_labels()
    );
    // People and software: each group under its own heading, people above software, and each kind
    // of software tagged as what it is.
    set_the_people(
        &session,
        vec![
            someone_here(1, "Mia Clayton", None, ""),
            someone_here(2, "Sam Okafor", None, ""),
            someone_here(
                3,
                "Fieldwork",
                Some(collab_types::MemberId::agent("fieldwork", "ada")),
                "so.tilo.notes",
            ),
            someone_here(
                4,
                "Standup",
                Some(collab_types::MemberId::bot(
                    "6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6",
                )),
                "",
            ),
        ],
    );
    until(&mut session, |session| says(session, "You and 4 others"));
    let people_heading = people_place(&session, "People");
    let agents_heading = people_place(&session, "Agents");
    let mia = people_place(&session, "Mia Clayton");
    let sam = people_place(&session, "Sam Okafor");
    let fieldwork = people_place(&session, "Fieldwork");
    assert!(
        people_heading.1 < mia.1 && mia.1 < sam.1 && sam.3 <= agents_heading.1,
        "people stand under their heading, before software's: {people_heading:?} {mia:?} {sam:?} {agents_heading:?}"
    );
    assert!(
        agents_heading.1 < fieldwork.1,
        "software stands under its own heading: {agents_heading:?} {fieldwork:?}"
    );
    assert!(
        says(&session, "Agent") && says(&session, "Bot"),
        "an agent and a bot are each tagged as what they are: {:?}",
        session.accessible_labels()
    );
    // Software's row names where it works, with the way there; a person's offers none.
    assert!(
        says(&session, "In Notes") && session.shell_control("Go there").is_some(),
        "the agent's row names its app and offers the way there: {:?}",
        session.accessible_labels()
    );
    assert_eq!(
        session.shell_controls("Go there").len(),
        1,
        "only software works in a window this roster can go to"
    );
    let reading = people_reading(&session);
    let fieldwork = reading
        .rows
        .iter()
        .find(|row| row.name == "Fieldwork")
        .expect("the agent's row");
    assert!(fieldwork.agent && !fieldwork.bot, "an agent is never a bot");
    let standup = reading
        .rows
        .iter()
        .find(|row| row.name == "Standup")
        .expect("the bot's row");
    assert!(standup.agent && standup.bot, "a bot is software, and a bot");
    keep(&mut session, "shell-people-rows-and-headings");
}

/// The roster's rows do what the Rust roster's did without opening a card: follow and stop
/// following, wave — the row says it went for a moment — where software works, and neither wave at
/// software nor follow a row that is not there.
#[test]
fn a_people_row_follows_waves_and_goes_where_software_works() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    set_the_people(
        &session,
        vec![
            someone_here(1, "Mia Clayton", None, ""),
            someone_here(
                2,
                "Fieldwork",
                Some(collab_types::MemberId::agent("fieldwork", "ada")),
                "so.tilo.notes",
            ),
        ],
    );
    open_who_is_here(&mut session);
    until(&mut session, |session| {
        session.shell_control("Follow Mia Clayton").is_some()
    });
    let key = people_reading(&session)
        .rows
        .iter()
        .find(|row| row.name == "Mia Clayton")
        .expect("Mia's row")
        .key
        .clone();
    // Follow stands on her row: her view is the one the bar keeps in front from then on.
    press(&mut session, "Follow Mia Clayton");
    until(&mut session, |session| {
        people_reading(session).following == key
    });
    until(&mut session, |session| {
        session
            .shell_control("Stop following Mia Clayton")
            .is_some()
    });
    press(&mut session, "Stop following Mia Clayton");
    until(&mut session, |session| {
        people_reading(session).following.is_empty()
    });
    // A wave sent says so on the row it went to, to the pointer's eye and to assistive technology
    // alike, and goes when its moment is over.
    press(&mut session, "Wave at Mia Clayton");
    until(&mut session, |session| {
        people_reading(session).waved_to == key
    });
    until(&mut session, |session| {
        says(session, "You waved at Mia Clayton")
    });
    until(&mut session, |session| {
        people_reading(session).waved_to.is_empty()
    });
    assert!(
        !says(&session, "You waved at Mia Clayton"),
        "the sent wave goes when its moment is over: {:?}",
        session.accessible_labels()
    );
    // Nobody waves at software.
    assert!(
        session.shell_control("Wave at Fieldwork").is_none(),
        "software's row offers no wave: {:?}",
        session.accessible_labels()
    );
    // Where software works: pressing it brings that window forward, so what it is on stands in
    // front of this person.
    let notes = open_window(&mut session, "so.tilo.notes");
    until(&mut session, |session| says(session, "Go there"));
    assert_ne!(
        the_window_in_front(&session),
        Some(notes),
        "the agent's window is not in front yet"
    );
    press(&mut session, "Go there");
    until(&mut session, |session| {
        the_window_in_front(session) == Some(notes)
    });
}

/// Someone in a call wears the arc sweeping round their face on their row, where the bar's faces
/// wear it too: the face is not the one a person out of a call wears.
#[test]
fn a_face_in_a_call_wears_the_sweeping_ring_on_its_row() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    let mut mia = someone_here(1, "Mia Clayton", None, "");
    mia.activity = Some("Editing the release plan".to_owned());
    set_the_people(&session, vec![mia.clone()]);
    open_who_is_here(&mut session);
    until(&mut session, |session| {
        session.shell_control("Follow Mia Clayton").is_some()
    });
    let row = people_place(&session, "Mia Clayton");
    let quiet = keep(&mut session, "shell-people-row-not-in-call");
    // The same person, in a voice channel: their row says the call and their face wears the ring.
    mia.call = Some(
        collab_types::presence::CallPresence {
            channel: "c_voice".to_owned(),
            name: "voice".to_owned(),
            bridge: None,
        }
        .encode(),
    );
    set_the_people(&session, vec![mia]);
    until(&mut session, |session| {
        people_reading(session)
            .rows
            .iter()
            .any(|row| row.call == "voice")
    });
    settle(&mut session);
    let calling = keep(&mut session, "shell-people-row-in-call");
    assert_eq!(
        people_place(&session, "Mia Clayton"),
        row,
        "the row stands where it did: only its face changed"
    );
    let face = (row.0, row.1, row.0 + 56.0, row.1 + 48.0);
    let (inside, _) = changed_in(&quiet, &calling, face);
    assert!(
        inside > 40,
        "the face in a call wears its arc: {inside} pixels of it changed"
    );
}

/// A line their device read off their steps wears the sparkles on the row: the words are
/// software's, never shown as if the person had said them.
#[test]
fn a_line_software_wrote_wears_the_mark_on_its_row() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    let mut mia = someone_here(1, "Mia Clayton", None, "");
    mia.activity = Some("Editing the release plan".to_owned());
    mia.automatic = false;
    set_the_people(&session, vec![mia.clone()]);
    open_who_is_here(&mut session);
    until(&mut session, |session| {
        session.shell_control("Follow Mia Clayton").is_some()
    });
    let row = people_place(&session, "Mia Clayton");
    let theirs = keep(&mut session, "shell-people-row-their-own-words");
    // Their device wrote the line for them: the row marks it as software's.
    mia.automatic = true;
    set_the_people(&session, vec![mia]);
    until(&mut session, |session| {
        people_reading(session).rows.iter().any(|row| row.automatic)
    });
    settle(&mut session);
    let written = keep(&mut session, "shell-people-row-written-for-them");
    let (inside, _) = changed_in(&theirs, &written, row);
    assert!(
        inside > 20,
        "the mark for words software wrote is on the row: {inside} pixels of it changed"
    );
}

/// The invite stands on the list where the workspace lets this person invite, and nowhere else: a
/// session on its own device's daemon, in its person's own workspace, offers it, and a session
/// that reaches no workspace to invite into offers none.
#[test]
fn the_people_list_offers_the_invite_only_where_this_person_may_invite() {
    let _settled = ui::motion::testing::settled();
    let mut theirs =
        crate::common::on_its_daemon(&[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)], options());
    until(&mut theirs, |session| says(session, &time(session).clock));
    open_who_is_here(&mut theirs);
    until(&mut theirs, |session| says(session, "Just you"));
    assert!(
        people_reading(&theirs).may_invite,
        "the workspace this person owns lets them invite"
    );
    assert!(
        theirs.shell_control("Invite someone").is_some(),
        "the list offers the invite: {:?}",
        theirs.accessible_labels()
    );

    // A session that reaches no device daemon holds no workspace whose governance could let it
    // invite, and says so rather than offering a button whose sheet could only refuse.
    let mut alone = session();
    until(&mut alone, |session| says(session, &time(session).clock));
    open_who_is_here(&mut alone);
    until(&mut alone, |session| says(session, "Just you"));
    assert!(
        !people_reading(&alone).may_invite,
        "no workspace answers for this session"
    );
    assert!(
        alone.shell_control("Invite someone").is_none(),
        "and the list offers no invite: {:?}",
        alone.accessible_labels()
    );
}

/// Someone nobody named is told apart by when they joined, never a bare "Someone": two admitted
/// within one second of each other say the millisecond, and a reader is told it too.
#[test]
fn a_row_nobody_named_says_when_they_joined() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    // The workspace admitted the two within one second of each other, so the clock alone would
    // read alike.
    let early = "ab".repeat(32);
    let late = "cd".repeat(32);
    let handle = session.handle().clone();
    let mut tree = handle.read(Clone::clone);
    let workspace = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id() == tree.active)
        .expect("a workspace is in front");
    for (person, at) in [
        (early.as_str(), 1_765_000_000_000_u64),
        (late.as_str(), 1_765_000_000_402),
    ] {
        workspace
            .policy
            .members
            .insert(person.to_owned(), tilo_action_policy::Role::Editor);
        workspace.policy.joined.insert(person.to_owned(), at);
    }
    handle.replace_tree(tree);
    session.step(Duration::from_millis(1)).expect("step");
    set_the_people(
        &session,
        vec![someone_unnamed(&early), someone_unnamed(&late)],
    );
    open_who_is_here(&mut session);
    let unnamed = ui::presence::unnamed_person_label();
    let said = crate::common::wait_for(
        &mut session,
        "both unnamed rows to say when they joined",
        |session| {
            let rows = people_labels(session, &unnamed);
            (rows.len() == 2).then_some(rows)
        },
    );
    assert_eq!(
        said.len(),
        2,
        "each row says something of its own: {said:?}"
    );
    assert!(
        said.iter().all(|line| line.contains("Joined")),
        "a row nobody named says when they joined, never a bare Someone: {said:?}"
    );
    assert_ne!(said[0], said[1], "the two rows read apart: {said:?}");
    assert!(
        said.iter().all(|line| line.contains('.')) && said.iter().all(|line| line.contains(':')),
        "the clock carries seconds, and the millisecond where two came within one second: {said:?}"
    );
}

/// Another of the person's devices, from their own card: the session's own sheet, whose link a
/// new device's camera reads.
#[test]
fn the_persons_own_card_adds_a_device() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    press_where(&mut session, "the person's own face", |session| {
        bar_control(session, &format!("{PERSON}: open your card"))
    });
    until(&mut session, |session| {
        session.shell_control("Add a device").is_some()
    });
    // The action the button runs: the session's own sheet asks for the new device's link.
    session
        .invoke("session:people", "pair-device", &serde_json::Map::new())
        .expect("another device is asked for");
    assert!(
        session.shell_refusals().is_empty(),
        "adding a device is not refused: {:?}",
        session.shell_refusals()
    );
    keep(&mut session, "shell-people-own-card-device");
}

/// A question the session put to the person about themselves, open, stands as its card under
/// their own avatar: what it asks, what answering brings, and the way to put it off is the
/// session's own action.
#[test]
fn a_question_on_the_persons_avatar_stands_as_a_card() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    tilo_session::identity::offer(
        session.handle(),
        &Told(session.notifications()),
        HostAction {
            id: "keep-identity".to_owned(),
            title: "Keep with passkey".to_owned(),
            label: None,
            subtitle: None,
            icon: None,
            destructive: false,
            unasked: false,
            source: Some(HostActionSource::Identity),
        },
        "Keep your name and work?",
        "A passkey brings you back as yourself on this device.",
    );
    until(&mut session, |session| {
        let labels = session.accessible_labels();
        labels
            .iter()
            .any(|label| label.contains("Keep your name and work?"))
            && labels
                .iter()
                .any(|label| label.contains("A passkey brings you back"))
    });
    assert!(
        session.shell_control("Keep with passkey").is_some(),
        "the question's answer stands on its card: {:?}",
        session.accessible_labels()
    );
    keep(&mut session, "shell-people-question");
    // Not now is the session's own action: the question goes.
    press(&mut session, "Not now");
    until(&mut session, |session| {
        session.handle().personal_ask().is_none()
    });
}

/// The notices a test host shows, on the shell this session draws them on.
struct Told(DesktopNotifications);

impl tilo_session::platform::notice::Notices for Told {
    fn show(&self, notice: &tilo_session::platform::notice::Notice) {
        self.0
            .post(tilo_session_core::desktop::notifications::Notification::of_notice(notice));
    }

    fn withdraw(&self, _tag: &str) {}
}

/// An action host that keeps what it was asked to run: where the session starts an app.
#[derive(Default)]
struct Apps {
    invoked: std::cell::RefCell<Vec<tilo_actions::remote::RemoteActionTarget>>,
}

impl tilo_session::AppActions for Apps {
    fn invoke(
        &self,
        target: tilo_actions::remote::RemoteActionTarget,
        _: serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), tilo_actions::ApiError> {
        self.invoked.borrow_mut().push(target);
        Ok(())
    }
}

/// A sound card in memory: the speakers, at 40%.
struct Card {
    graph: std::sync::Mutex<media::audio::AudioGraph>,
}

impl tilo_audio_service::sound::Graph for Card {
    fn read(&self) -> tilo_audio_service::sound::Read {
        tilo_audio_service::sound::Read {
            graph: self.graph.lock().unwrap().clone(),
            uses: Vec::new(),
        }
    }

    fn control(&self) -> &dyn media::audio::GraphControl {
        self
    }
}

impl media::audio::GraphControl for Card {
    fn set_default_sink(&self, _node_id: u32) -> Result<(), String> {
        Ok(())
    }

    fn set_default_source(&self, _node_id: u32) -> Result<(), String> {
        Ok(())
    }

    fn route_stream(&self, _stream_id: u32, _target_serial: u32) -> Result<(), String> {
        Ok(())
    }

    fn unroute_stream(&self, _stream_id: u32) -> Result<(), String> {
        Ok(())
    }

    fn set_node_volume(&self, node_id: u32, perceptual: f32) -> Result<(), String> {
        for sink in &mut self.graph.lock().unwrap().sinks {
            if sink.id == node_id {
                sink.volume = Some(perceptual);
            }
        }
        Ok(())
    }

    fn set_node_mute(&self, node_id: u32, muted: bool) -> Result<(), String> {
        let mut graph = self.graph.lock().unwrap();
        let media::audio::AudioGraph {
            sinks,
            sources,
            playback_streams,
            record_streams,
            ..
        } = &mut *graph;
        for device in sinks.iter_mut().chain(sources.iter_mut()) {
            if device.id == node_id {
                device.muted = Some(muted);
            }
        }
        for stream in playback_streams.iter_mut().chain(record_streams.iter_mut()) {
            if stream.id == node_id {
                stream.muted = Some(muted);
            }
        }
        Ok(())
    }

    fn set_device_profile(&self, _device_id: u32, _profile_index: u32) -> Result<(), String> {
        Ok(())
    }
}

tilo_test_env::scenario! {
    fn the_launcher_opens_and_starts_what_it_lists;

    phase an_apps_open_the_shell_calls_starts_it;
    // An app's own `open` the shell calls (`session.open(app)`, a dock's press) starts the app in the
    // workspace in front, as the launcher's row of it does. (The press making that call is
    // `a_press_on_a_dock_app_asks_the_session_to_open_it` in `shell::part` .)
    {
        let _settled = ui::motion::testing::settled();
        let apps = Rc::new(Apps::default());
        let mut session = Session::headless(
            Options {
                apps: Some(Rc::clone(&apps) as Rc<dyn tilo_session::AppActions>),
                ..options()
            },
            &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
        )
        .expect("session");
        until(&mut session, |session| says(session, &time(session).clock));
        session
            .shell_link()
            .expect("the shell runs")
            .ask(tilo_session::shell::part::Ask {
                resource: "app.so.tilo.sheets".to_owned(),
                action: "open".to_owned(),
                arguments: std::collections::BTreeMap::new(),
                from: None,
            });
        until(&mut session, |_| !apps.invoked.borrow().is_empty());
        assert_eq!(
            apps.invoked.borrow()[0].address,
            tilo_actions::ActionAddress::new("app.so.tilo.sheets", "open"),
            "the app opens by the workspace's address for it"
        );
        assert!(
            session.shell_refusals().is_empty(),
            "{:?}",
            session.shell_refusals()
        );
    }

    phase a_press_on_the_docks_launcher_shows_the_launcher;
    // A press on the dock's launcher calls the shell's own `show-surface` : the launcher comes up.
    {
        let _settled = ui::motion::testing::settled();
        let mut session = session();
        until(&mut session, |session| says(session, &time(session).clock));
        until(&mut session, |session| {
            session.shell_control("Launcher").is_some()
        });
        press(&mut session, "Launcher");
        until(&mut session, |session| {
            crate::common::shell_here(session) == "launcher"
        });
    }
}

/// A sound service over a card in memory: the speakers, at 40%.
fn speaker() -> tilo_audio_service::sound::SoundService {
    let card = Card {
        graph: std::sync::Mutex::new(media::audio::AudioGraph {
            sinks: vec![media::audio::AudioDevice {
                id: 40,
                serial: 40,
                name: "node-40".to_owned(),
                description: "Speakers".to_owned(),
                is_default: true,
                volume: Some(0.4),
                muted: Some(false),
                channels: 2,
                codec: None,
            }],
            ..media::audio::AudioGraph::default()
        }),
    };
    let mut sound =
        tilo_audio_service::sound::SoundService::new(std::sync::Arc::new(card), None).unwrap();
    sound.settle();
    sound
}

/// A sound service over a card in memory with a microphone: the panel's microphone mute and level
/// are the sound's own.
fn speaker_with_microphone() -> tilo_audio_service::sound::SoundService {
    let card = Card {
        graph: std::sync::Mutex::new(media::audio::AudioGraph {
            sinks: vec![media::audio::AudioDevice {
                id: 40,
                serial: 40,
                name: "node-40".to_owned(),
                description: "Speakers".to_owned(),
                is_default: true,
                volume: Some(0.4),
                muted: Some(false),
                channels: 2,
                codec: None,
            }],
            sources: vec![media::audio::AudioDevice {
                id: 50,
                serial: 50,
                name: "node-50".to_owned(),
                description: "Microphone".to_owned(),
                is_default: true,
                volume: Some(0.6),
                muted: Some(false),
                channels: 1,
                codec: None,
            }],
            ..media::audio::AudioGraph::default()
        }),
    };
    let mut sound =
        tilo_audio_service::sound::SoundService::new(std::sync::Arc::new(card), None).unwrap();
    sound.settle();
    sound
}

/// A sound service over a card in memory playing one app's song, which the quick settings show as a
/// stream of its own: its level, its mute, and where it plays.
fn speaker_with_streams() -> tilo_audio_service::sound::SoundService {
    let card = Card {
        graph: std::sync::Mutex::new(media::audio::AudioGraph {
            sinks: vec![media::audio::AudioDevice {
                id: 40,
                serial: 40,
                name: "node-40".to_owned(),
                description: "Speakers".to_owned(),
                is_default: true,
                volume: Some(0.4),
                muted: Some(false),
                channels: 2,
                codec: None,
            }],
            playback_streams: vec![media::audio::AudioStream {
                id: 70,
                serial: 70,
                app: "Music".to_owned(),
                media_name: Some("A song".to_owned()),
                playback: true,
                target_id: None,
                volume: Some(0.6),
                muted: Some(false),
            }],
            ..media::audio::AudioGraph::default()
        }),
    };
    let mut sound =
        tilo_audio_service::sound::SoundService::new(std::sync::Arc::new(card), None).unwrap();
    sound.settle();
    sound
}

/// A session of `width` by `height` with the shell's bar, the person's settings and a sound card:
/// what the quick settings read and change.
fn session_with_devices(width: i32, height: i32) -> Session {
    session_with_services(width, height, Vec::new())
}

/// A session serving the settings and the sound, and `extra` beside them.
fn session_with_services(
    width: i32,
    height: i32,
    extra: Vec<Box<dyn tilo_session::Service>>,
) -> Session {
    session_where(width, height, extra, Vec::new())
}

/// A session serving `extra` beside the settings and the sound, with the surface apps in `surfaces`
/// installed.
fn session_where(
    width: i32,
    height: i32,
    extra: Vec<Box<dyn tilo_session::Service>>,
    surfaces: Vec<tilo_session::surfaces::Surface>,
) -> Session {
    let mut services: Vec<Box<dyn tilo_session::Service>> = vec![
        Box::new(settings_sections::service::SettingsService::new()),
        Box::new(speaker()),
    ];
    services.extend(extra);
    Session::headless(
        Options {
            services,
            collab: false,
            surfaces,
            ..options()
        },
        &[OutputConfig::new("HEADLESS-1", width, height)],
    )
    .expect("session")
}

/// Run frames, taking in what the services report, until `done` holds.
#[track_caller]
fn until_settled(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) {
    crate::common::wait_for(session, "the services' reading", |session| {
        session.settle_services();
        // What the services reported reaches the shell on the frame the loop runs.
        session.step(Duration::from_millis(1)).expect("step");
        done(session).then_some(())
    });
}

/// Press the control named `name` once the bar stands still: what the services report while the
/// session starts (the workspace, who is here) adds controls beside it and moves it, and a person
/// presses the control where they see it at rest.
fn press_at_rest(session: &mut Session, name: &str) {
    until_settled(session, |session| {
        let before = (
            session.shell_control(name),
            session.shell_work().map(|w| w.turns),
        );
        session.settle_services();
        session.step(Duration::from_millis(1)).expect("step");
        let after = (
            session.shell_control(name),
            session.shell_work().map(|w| w.turns),
        );
        before.0.is_some() && before == after
    });
    press(session, name);
}

/// The person's appearance mode, as the session publishes their settings.
fn appearance_mode(session: &Session) -> serde_json::Value {
    session
        .read("settings/appearance")
        .map(|section| section["mode"].clone())
        .unwrap_or_default()
}

/// The bar's status opens the quick settings over the shell, and the panel's Dark mode tile runs the
/// appearance settings' own published action: the published mode turns from what it was to the
/// other, as Settings and an agent turn it.
fn dark_mode_from_quick_settings_at(width: i32, height: i32) {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = device_session_over(width, height, Device { charging: false });
    until_settled(&mut session, |session| says(session, &time(session).clock));
    // The status opens the settings: the sound's control on a shell, the charge's on a phone,
    // where the sound gives way and the bell opens the notification centre.
    let opener = if width < 480 { "61%" } else { "Volume, 40%" };
    press_at_rest(&mut session, opener);
    until_settled(&mut session, |session| here(session) == "quick-settings");
    until_settled(&mut session, |session| says(session, "Dark mode"));
    until_settled(&mut session, |session| says(session, "Volume"));
    let dark = serde_json::json!("dark");
    let was_dark = appearance_mode(&session) == dark;
    let name = |dark: bool| {
        format!(
            "shell-quick-settings-{width}-{}",
            if dark { "dark" } else { "light" }
        )
    };
    keep(&mut session, &name(was_dark));
    press_at_rest(&mut session, "Dark mode");
    until_settled(&mut session, |session| {
        (appearance_mode(session) == dark) != was_dark
    });
    assert!(
        session.shell_refusals().is_empty(),
        "the tile's press reached the settings: {:?}",
        session.shell_refusals()
    );
    for _ in 0..20 {
        session.settle_services();
        session.step(Duration::from_millis(1)).unwrap();
    }
    keep(&mut session, &name(!was_dark));
}

#[test]
fn quick_settings_dark_mode_changes_the_published_appearance() {
    dark_mode_from_quick_settings_at(WIDTH, HEIGHT);
}

#[test]
fn quick_settings_dark_mode_changes_the_published_appearance_on_a_phone() {
    dark_mode_from_quick_settings_at(390, 844);
}

/// The volume slider shows the sound's published volume, and pressing its track runs the sound's
/// own `set-volume` : the published volume follows.
#[test]
fn quick_settings_volume_slider_sets_the_published_volume() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = session_with_devices(WIDTH, HEIGHT);
    until_settled(&mut session, |session| says(session, &time(session).clock));
    let volume = |session: &Session| {
        session
            .read("sound")
            .and_then(|sound| sound["volume"].as_f64())
            .unwrap_or_default()
    };
    assert!(
        (volume(&session) - 0.4).abs() < 0.01,
        "the card starts at 40%"
    );
    press_at_rest(&mut session, "Volume, 40%");
    until_settled(&mut session, |session| {
        session.shell_control("Volume").is_some()
    });
    // Press the track near its end.
    let (x, y, w, h) = session.shell_control("Volume").unwrap();
    let (x, y) = (x + w * 0.9, y + h / 2.0);
    session
        .compositor()
        .input(InputEvent::PointerMotion { x, y })
        .unwrap();
    session.step(Duration::from_millis(1)).unwrap();
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::PointerButton {
                button: BTN_LEFT,
                pressed,
            })
            .unwrap();
        session.step(Duration::from_millis(1)).unwrap();
    }
    until_settled(&mut session, |session| volume(session) > 0.7);
    assert!(
        session.shell_refusals().is_empty(),
        "the slider's change reached the sound: {:?}",
        session.shell_refusals()
    );
}

/// Run the session at its display's refresh until the screen stops changing: the panel has drawn
/// the frame its last action asked for.
fn drawn(session: &mut Session) -> tilo_compositor::Screenshot {
    let mut last = session.compositor().screenshot("HEADLESS-1").expect("shot");
    let deadline = tilo_test_env::hang_deadline();
    loop {
        session.settle_services();
        session.step(Duration::from_millis(16)).expect("step");
        let frame = session.compositor().screenshot("HEADLESS-1").expect("shot");
        if frame.rgba == last.rgba {
            return frame;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the screen never settled; the parts say: {:?}",
            session.shell_parts()
        );
        last = frame;
    }
}

// ---- each press of the shell's reaches the session ------------------------------------------
//
// Every call of the session's actions the shell makes (`session.focus(window(w))`) is an
// invocation on the session's API: each test below presses the shell's own view and reads the
// session's state, so a call the API cannot run fails here.

/// Bring the shell's surface `surface` up, as the bar's own buttons do.
fn show(session: &mut Session, surface: &str) {
    session
        .invoke(
            "desktop:overlays",
            "show-surface",
            &serde_json::Map::from_iter([(
                "surface".to_owned(),
                serde_json::Value::String(surface.to_owned()),
            )]),
        )
        .unwrap();
    until(session, |session| here(session) == surface);
}

/// Where the tab `tab` stands: its workspace's id and its strip's name, while it is open.
fn tab_place(session: &Session, tab: u64) -> Option<(tilo_desktop_model::WorkspaceId, String)> {
    session.handle().read(|tree| {
        tree.workspaces.iter().find_map(|workspace| {
            workspace.strips.iter().find_map(|strip| {
                strip
                    .window_groups
                    .iter()
                    .any(|group| group.tabs.iter().any(|t| t.id == tab))
                    .then(|| (workspace.id(), strip.name.clone()))
            })
        })
    })
}

/// The tab `tab` is the window in front, as the shell publishes it.
fn in_front(session: &Session, tab: u64) -> bool {
    crate::common::windows(session)
        .iter()
        .any(|window| window.focused && window.key == tab.to_string())
}

/// Name the strip in front `first` , and add a strip `second` beside it holding a window of `app` ,
/// its tab's id.
fn second_strip(session: &mut Session, first: &str, second: &str, app: &str) -> u64 {
    let handle = session.handle().clone();
    let mut tree = handle.read(Clone::clone);
    let tab = handle.mint_id();
    let group = handle.mint_id();
    let strip = tilo_desktop_model::StripId(handle.mint_id());
    let workspace = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id() == tree.active)
        .expect("a workspace is in front");
    first.clone_into(&mut workspace.strips[0].name);
    let mut added = tilo_desktop_model::Strip::new(strip, second);
    added.window_groups.push(tilo_desktop_model::WindowGroup {
        id: group,
        tabs: vec![tilo_desktop_model::Tab {
            id: tab,
            title: app.to_owned(),
            tile: tilo_desktop_model::TileContent::LocalApp {
                app_id: app.to_owned(),
                label: app.to_owned(),
                note: String::new(),
            },
            app_title: None,
            view: String::new(),
            pinned: false,
        }],
        active_tab: tab,
        width: tilo_desktop_model::WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    });
    workspace.strips.push(added);
    handle.replace_tree(tree);
    session.step(Duration::from_millis(1)).unwrap();
    tab
}

/// Open the overview's menu of its (only, or first) window, and wait for `item` in it.
fn window_menu(session: &mut Session, item: &str) {
    until(session, |session| {
        session.shell_control("This window").is_some()
    });
    press(session, "This window");
    until(session, |session| session.shell_control(item).is_some());
}

/// The launcher's row of a window goes to it: `focus` on the window's own resource.
#[test]
fn a_press_on_a_launcher_row_brings_its_window_front() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    open_window(&mut session, "so.tilo.notes");
    open_window(&mut session, "so.tilo.mail");
    until(&mut session, |session| {
        crate::common::windows(session).len() == 2
    });
    // The window not in front is the one to go to.
    let behind = crate::common::windows(&session)
        .into_iter()
        .find(|w| !w.focused)
        .expect("one window is behind the other");
    let tab: u64 = behind.key.parse().unwrap();
    show(&mut session, "launcher");
    until(&mut session, |session| {
        session.shell_control(&behind.title).is_some()
    });
    press(&mut session, &behind.title);
    until(&mut session, |session| in_front(session, tab));
    assert!(
        session.shell_refusals().is_empty(),
        "{:?}",
        session.shell_refusals()
    );
}

/// The launcher's row to the overview shows it: `show-surface` on the shell.
#[test]
fn a_press_on_a_launcher_row_shows_that_surface() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    show(&mut session, "launcher");
    until(&mut session, |session| {
        session.shell_control("Workspaces").is_some()
    });
    press(&mut session, "Workspaces");
    until(&mut session, |session| here(session) == "workspaces");
    assert!(
        session.shell_refusals().is_empty(),
        "{:?}",
        session.shell_refusals()
    );
}

/// What the launcher holds no window for, Search everywhere hands the shell's search: it opens with
/// what was typed.
#[test]
fn searching_everywhere_from_the_launcher_opens_the_shells_search_with_the_query() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    show(&mut session, "launcher");
    until(&mut session, |session| {
        session.shell_control(LAUNCHER_SEARCH).is_some()
    });
    press(&mut session, LAUNCHER_SEARCH);
    type_text(&mut session, "quarterly");
    let button = "Search everywhere for quarterly";
    until(&mut session, |session| {
        session.shell_control(button).is_some()
    });
    press(&mut session, button);
    until(&mut session, |session| {
        session.resources().iter().any(|resource| {
            resource.id.as_str() == "desktop:shell"
                && resource.children.iter().any(|child| {
                    child.id.as_str() == tilo_session_core::desktop::verbs::SEARCH_RESOURCE
                        && child.label == "quarterly"
                })
        })
    });
    assert!(
        session.shell_refusals().is_empty(),
        "{:?}",
        session.shell_refusals()
    );
}

/// A window's Close in the overview closes it: `close` on the window's own resource.
#[test]
fn closing_a_window_from_the_overview_takes_it_off_the_shell() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    let notes = open_window(&mut session, "so.tilo.notes");
    until(&mut session, |session| {
        crate::common::windows(session).len() == 1
    });
    show(&mut session, "overview");
    window_menu(&mut session, "Close");
    press(&mut session, "Close");
    until(&mut session, |session| tab_place(session, notes).is_none());
    assert!(
        session.shell_refusals().is_empty(),
        "{:?}",
        session.shell_refusals()
    );
}

/// A window carried to another strip from the overview stands on it: the chrome's
/// `move-tab-to-strip` .
#[test]
fn moving_a_window_to_a_strip_from_the_overview_carries_it_there() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    let notes = open_window(&mut session, "so.tilo.notes");
    let mail = second_strip(&mut session, "Main", "Research", "so.tilo.mail");
    until(&mut session, |session| {
        crate::common::windows(session).len() == 2
    });
    show(&mut session, "overview");
    // The first window's menu offers the other window's strip, whichever window it is.
    until(&mut session, |session| {
        session.shell_control("This window").is_some()
    });
    press(&mut session, "This window");
    until(&mut session, |session| {
        ["Move to Main", "Move to Research"]
            .iter()
            .any(|item| session.shell_control(item).is_some())
    });
    let (item, strip) = if session.shell_control("Move to Research").is_some() {
        ("Move to Research", "Research")
    } else {
        ("Move to Main", "Main")
    };
    press(&mut session, item);
    until(&mut session, |session| {
        [notes, mail]
            .iter()
            .all(|tab| tab_place(session, *tab).is_some_and(|(_, on)| on == strip))
    });
    assert!(
        session.shell_refusals().is_empty(),
        "{:?}",
        session.shell_refusals()
    );
}

/// A window carried to another workspace from the overview is in it: the chrome's `move-tab` .
#[test]
fn moving_a_window_to_a_workspace_from_the_overview_carries_it_there() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    let first = shell(&session);
    let first_key = first
        .workspaces
        .iter()
        .find(|workspace| workspace.current)
        .unwrap()
        .key
        .clone();
    session
        .invoke(
            tilo_session::shell::part::CHROME_RESOURCE,
            "new-workspace",
            &serde_json::Map::new(),
        )
        .unwrap();
    until(&mut session, |session| shell(session).workspaces.len() == 2);
    let second = shell(&session)
        .workspaces
        .iter()
        .find(|workspace| workspace.current)
        .unwrap()
        .clone();
    session
        .invoke(
            tilo_session::shell::part::CHROME_RESOURCE,
            "switch-workspace",
            &serde_json::Map::from_iter([(
                "workspace".to_owned(),
                serde_json::Value::String(first_key),
            )]),
        )
        .unwrap();
    until(&mut session, |session| {
        shell(session).workspace == first.workspace
    });
    let notes = open_window(&mut session, "so.tilo.notes");
    until(&mut session, |session| {
        crate::common::windows(session).len() == 1
    });
    show(&mut session, "overview");
    let item = format!("Move to {}", second.name);
    window_menu(&mut session, &item);
    press(&mut session, &item);
    until(&mut session, |session| {
        tab_place(session, notes).is_some_and(|(workspace, _)| workspace.to_string() == second.key)
    });
    assert!(
        session.shell_refusals().is_empty(),
        "{:?}",
        session.shell_refusals()
    );
}

/// The centre's Clear all empties it: the centre's `clear`.
#[test]
fn a_press_on_the_centres_clear_all_empties_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    for (title, body) in [
        ("Backup finished", "12 GB safe"),
        ("Update ready", "Restart"),
    ] {
        session.notices().post(
            tilo_session_core::desktop::notifications::Notification::new(title, body)
                .with_app_name("Settings"),
        );
    }
    until(&mut session, |session| centre(session).notices.len() == 2);
    show(&mut session, "centre");
    until(&mut session, |session| {
        session.shell_control("Clear all").is_some()
    });
    press(&mut session, "Clear all");
    until(&mut session, |session| centre(session).notices.is_empty());
    assert!(
        session.shell_refusals().is_empty(),
        "{:?}",
        session.shell_refusals()
    );
}

/// The centre's Undo brings back what a Clear all took, through the centre's `undo-clear`: the
/// notices return and the cleared band goes.
#[test]
fn a_press_on_the_centres_undo_brings_the_cleared_notices_back() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    for (title, body) in [
        ("Backup finished", "12 GB safe"),
        ("Update ready", "Restart"),
    ] {
        session.notices().post(
            tilo_session_core::desktop::notifications::Notification::new(title, body)
                .with_app_name("Settings"),
        );
    }
    until(&mut session, |session| centre(session).notices.len() == 2);
    show(&mut session, "centre");
    until(&mut session, |session| {
        session.shell_control("Clear all").is_some()
    });
    press(&mut session, "Clear all");
    until(&mut session, |session| {
        centre(session).notices.is_empty()
            && centre(session).undo
            && session.shell_control("Undo").is_some()
    });
    press(&mut session, "Undo");
    until(&mut session, |session| centre(session).notices.len() == 2);
    assert!(
        session.shell_refusals().is_empty(),
        "{:?}",
        session.shell_refusals()
    );
}

/// Four banners are a stack: three stand, and one row names the one waiting behind them, which
/// opens the centre.
#[test]
fn the_banners_stack_and_say_how_many_wait() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    for index in 0..4 {
        session.notices().post(
            tilo_session_core::desktop::notifications::Notification::new(
                format!("Notice {index}"),
                "Something happened",
            )
            .with_app_name("Settings"),
        );
    }
    until(&mut session, |session| {
        centre(session).banners.len() == 4 && says(session, "1 more notification")
    });
}

/// A notice posted through the session's own `notify` rises as a banner a person reads and
/// dismisses while a surface of the shell's stands over the places — the launcher, as a person
/// leaves it open when a notice arrives: the shell publishes it as a live banner, its title and
/// its Dismiss stand where the person can read and press them, and pressing Dismiss takes the
/// notice off the shell and out of the centre. A surface does not hold a notice back: a notice
/// reaches a person wherever they are.
#[test]
fn a_notice_rises_as_a_banner_over_a_surface_with_a_dismiss_that_takes_it_away() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    show(&mut session, "launcher");
    session
        .invoke(
            "session:notifications",
            "notify",
            &serde_json::Map::from_iter([
                ("title".to_owned(), serde_json::json!("Lunch at one")),
                (
                    "body".to_owned(),
                    serde_json::json!("Table for four at the corner place"),
                ),
                ("from".to_owned(), serde_json::json!("Ada")),
            ]),
        )
        .unwrap_or_else(|error| panic!("notify: {error:?}"));
    // The session publishes it as a live banner, not only as a row the centre keeps.
    let live = crate::common::wait_maybe(&mut session, |session| {
        let centre = centre(session);
        (!centre.banners.is_empty() && centre.banner.is_some()).then_some(())
    });
    assert!(
        live.is_some(),
        "the notice rises as a live banner: the session publishes {:?}",
        session.read("session:notifications"),
    );
    // The shell draws it where a person reads and presses it: the title and the Dismiss are
    // controls with bounds on the screen, as the walk reads what stands there.
    let mut in_tree = false;
    let mut as_control = false;
    let found = crate::common::wait_maybe(&mut session, |session| {
        in_tree |= session.shell_says("Lunch at one");
        let found = Some((
            session.shell_control("Lunch at one")?,
            session.shell_control("Dismiss")?,
        ));
        as_control = true;
        found
    });
    let Some((title, dismiss)) = found else {
        panic!(
            "the banner's title and Dismiss stand on screen: it was in the tree {in_tree} and a \
             control {as_control}; the title is at {:?}, the Dismiss at {:?}, the tree says it at \
             {:?}; the session says {:?}",
            session.shell_control("Lunch at one"),
            session.shell_control("Dismiss"),
            session.shell_text_bounds("Lunch at one"),
            session.accessible_labels(),
        );
    };
    for (what, (x, y, w, h)) in [("the title", title), ("the Dismiss", dismiss)] {
        assert!(
            w > 0.0 && h > 0.0 && x >= 0.0 && y >= 0.0,
            "{what} stands on the screen: {:?}",
            (x, y, w, h)
        );
    }
    // Dismissing it from the banner takes it off the shell and out of the centre.
    press(&mut session, "Dismiss");
    until(&mut session, |session| {
        session.shell_control("Lunch at one").is_none() && centre(session).notices.is_empty()
    });
    assert!(
        session.shell_refusals().is_empty(),
        "the banner's Dismiss reached the centre: {:?}",
        session.shell_refusals()
    );
}

/// A banner's action is the banners' own `act`, apart from the centre's action of that name: the
/// press runs the sender's choice, and the banner it chose from goes.
#[test]
fn a_banners_action_runs_and_the_banner_goes() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    let mut notice = tilo_session_core::desktop::notifications::Notification::new(
        "Download finished",
        "report.pdf",
    )
    .with_app_name("Files");
    notice.actions = vec![("open".to_owned(), "Show the file".to_owned())];
    session.notices().post(notice);
    until(&mut session, |session| {
        centre(session).banners.len() == 1 && says(session, "Show the file")
    });
    press(&mut session, "Show the file");
    until(&mut session, |session| centre(session).banners.is_empty());
    assert!(
        session.shell_refusals().is_empty(),
        "the banner's action reached the banners: {:?}",
        session.shell_refusals()
    );
}

/// The quick settings' Night light tile runs the night light's own `toggle`: the published night
/// light turns from what it was.
#[test]
fn quick_settings_night_light_toggles_the_published_night_light() {
    let _settled = ui::motion::testing::settled();
    let mut session = session_with_devices(WIDTH, HEIGHT);
    until_settled(&mut session, |session| says(session, &time(session).clock));
    press_at_rest(&mut session, "Volume, 40%");
    until_settled(&mut session, |session| here(session) == "quick-settings");
    until_settled(&mut session, |session| {
        session.shell_control("Night light").is_some()
    });
    let was = night_light(&session);
    press_at_rest(&mut session, "Night light");
    until_settled(&mut session, |session| night_light(session) != was);
    assert!(
        session.shell_refusals().is_empty(),
        "the tile's press reached the night light: {:?}",
        session.shell_refusals()
    );
}

/// Whether the screen's night light is on, as the session publishes it.
fn night_light(session: &Session) -> bool {
    session
        .read("session:night-light")
        .and_then(|reading| reading["on"].as_bool())
        .unwrap_or_default()
}

/// A workspace named in the workspaces surface is made and in front: the chrome's `new-workspace`
/// with its name.
#[test]
fn creating_a_named_workspace_from_the_workspaces_surface_makes_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    show(&mut session, "workspaces");
    until(&mut session, |session| {
        session.shell_control(WORKSPACE_NAME).is_some()
    });
    press(&mut session, WORKSPACE_NAME);
    type_text(&mut session, "studio");
    press(&mut session, "Create");
    until(&mut session, |session| {
        shell(session)
            .workspaces
            .iter()
            .any(|workspace| workspace.name == "studio")
    });
    assert!(
        session.shell_refusals().is_empty(),
        "{:?}",
        session.shell_refusals()
    );
}

// ---- the bar's trailing group, its material and a compact bar ---------------------------

/// The people who come here in these tests, in the order they arrive.
const HERE: [&str; 5] = [
    "Mia Clayton",
    "Sam Okafor",
    "Jo Park",
    "Ada Byron",
    "Nils Berg",
];

/// Who is here, as the peer network lists them: the first `count` of [`HERE`].
fn set_here(session: &Session, count: usize) {
    let here: Vec<_> = HERE
        .iter()
        .take(count)
        .enumerate()
        .map(|(at, name)| tilo_session_core::desktop::here::LiveMember {
            publisher: format!("{:064x}", at + 1),
            kind: "person".to_owned(),
            label: (*name).to_owned(),
            ..tilo_session_core::desktop::here::LiveMember::default()
        })
        .collect();
    session
        .sync()
        .expect("the session takes part in the app wire")
        .here()
        .borrow_mut()
        .set_members(None, Some(here));
}

/// Where the bar's control whose name holds `words` stands, logical `(x, y, w, h)` .
fn bar_control(session: &Session, words: &str) -> Option<(f64, f64, f64, f64)> {
    session
        .accessible_labels()
        .into_iter()
        .filter(|label| label.contains(words))
        .find_map(|label| session.shell_control(&label))
}

/// Every control the shell shows, by the name assistive technology hears, with where it stands: the
/// bar's and the dock's.
fn bar_controls(session: &Session) -> Vec<(String, (f64, f64, f64, f64))> {
    session
        .accessible_labels()
        .into_iter()
        .filter_map(|label| session.shell_control(&label).map(|rect| (label, rect)))
        .collect()
}

/// The bar's band, logical `(x, y, w, h)` : the shell's own bar, as tall as the theme and the text
/// scale make it.
fn bar_band(session: &Session) -> (f64, f64, f64, f64) {
    // The bar's own ground, as assistive technology names it.
    if let Some(rect) = session.shell_control("Top bar") {
        return rect;
    }
    // The band the bar holds at the top of the output it stands on, when the ground has no bounds
    // of its own; the bar's controls stand inside it, and nothing of the shell does.
    session
        .shell_parts()
        .first()
        .map_or((0.0, 0.0, 0.0, 0.0), |bar| {
            let (x, y, w, _) = bar.rect;
            (f64::from(x), f64::from(y), f64::from(w), BAR_BAND_HEIGHT)
        })
}

/// The height of the bar's band: its ground's padding around the controls it holds.
const BAR_BAND_HEIGHT: f64 = 40.0;

/// The controls in the bar's band, by where their middle is: what a person sees on
/// the bar, not on the shell. A control that reaches out of the band is still one of the bar's,
/// which is what lets a test see it overflow.
fn bar_band_controls(session: &Session) -> Vec<(String, (f64, f64, f64, f64))> {
    let band = bar_band(session);
    let holds = |x: f64, y: f64, w: f64, h: f64| {
        let (x, y) = (x + w / 2.0, y + h / 2.0);
        x >= band.0 && x <= band.0 + band.2 && y >= band.1 && y <= band.1 + band.3
    };
    bar_controls(session)
        .into_iter()
        .filter(|(_, (x, y, w, h))| holds(*x, *y, *w, *h))
        .collect()
}

/// Run frames until what a setting wrote is drawn: nothing the views read reports itself.
fn settle(session: &mut Session) {
    drawn(session);
}

/// A device whose pack charges (or drains) and whose link is Wi-Fi, in memory: what the bar's
/// status reads where this machine's own services do not run.
#[derive(Clone, Copy, Debug)]
struct Device {
    charging: bool,
}

impl tilo_session::Service for Device {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        vec![
            tilo_actions::ResourceSpec::new("power:battery", "power:battery", "Battery"),
            tilo_actions::ResourceSpec::new("network", "network", "Network"),
        ]
    }

    fn read(&self, resource: &str) -> Option<serde_json::Value> {
        match resource {
            "power:battery" => Some(serde_json::json!({
                "present": true,
                "percentage": 61,
                "state": if self.charging { "charging" } else { "discharging" },
                "on_battery": !self.charging,
            })),
            "network" => Some(serde_json::json!({
                "link": { "kind": "wifi", "name": "Studio", "signal": 4 },
            })),
            _ => None,
        }
    }

    fn invoke(
        &mut self,
        _resource: &str,
        _action: &str,
        _arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::ApiError> {
        Err(tilo_actions::ApiError::Unavailable {
            code: "test".to_owned(),
            reason: "this device takes no actions: its readings are the test's".to_owned(),
        })
    }
}

/// A session at `width` by `height` whose device charges and is on Wi-Fi, with the person's
/// settings published: what the bar's status and its appearance read.
fn device_session(width: i32, height: i32) -> Session {
    device_session_over(width, height, Device { charging: true })
}

/// The same session over `device` .
fn device_session_over(width: i32, height: i32, device: Device) -> Session {
    let mut session = Session::headless(
        Options {
            services: vec![
                Box::new(settings_sections::service::SettingsService::new()),
                Box::new(speaker()),
                Box::new(device),
            ],
            ..options()
        },
        &[OutputConfig::new("HEADLESS-1", width, height)],
    )
    .expect("session");
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(1),
        rate: 0,
    });
    session
}

/// Turn the person's dark mode on or off, as the quick settings' tile does: the settings' own
/// published action.
fn set_appearance(session: &mut Session, dark: bool) {
    session
        .invoke(
            "settings/appearance",
            "set-dark-mode",
            &serde_json::Map::from_iter([("on".to_owned(), serde_json::json!(dark))]),
        )
        .expect("the appearance changes");
}

/// How many pixels of `after` differ from `before` by more than a shade, inside `rect` and outside
/// every rect of `allowed` : what changed where the test points, and what changed anywhere else.
fn changed_near(
    before: &tilo_compositor::Screenshot,
    after: &tilo_compositor::Screenshot,
    rect: (f64, f64, f64, f64),
    allowed: &[(f64, f64, f64, f64)],
) -> (usize, usize) {
    changed_by(before, after, rect, allowed, 8)
}

/// The same, for a change of more than `shade` in a channel: a material's tint can move the bar by
/// a level or two and still be another material.
fn changed_by(
    before: &tilo_compositor::Screenshot,
    after: &tilo_compositor::Screenshot,
    rect: (f64, f64, f64, f64),
    allowed: &[(f64, f64, f64, f64)],
    shade: u8,
) -> (usize, usize) {
    assert_eq!(
        (before.width, before.height),
        (after.width, after.height),
        "the two frames are of one screen"
    );
    let holds = |rect: (f64, f64, f64, f64), x: f64, y: f64| {
        x >= rect.0 && x < rect.0 + rect.2 && y >= rect.1 && y < rect.1 + rect.3
    };
    let mut inside = 0;
    let mut outside = 0;
    for (index, (a, b)) in before
        .rgba
        .chunks_exact(4)
        .zip(after.rgba.chunks_exact(4))
        .enumerate()
    {
        if !a
            .iter()
            .zip(b.iter())
            .any(|(a, b)| u8::abs_diff(*a, *b) > shade)
        {
            continue;
        }
        let x = (index % before.width as usize) as f64;
        let y = (index / before.width as usize) as f64;
        if holds(rect, x, y) {
            inside += 1;
        } else if !allowed.iter().any(|allowed| holds(*allowed, x, y)) {
            outside += 1;
        }
    }
    (inside, outside)
}

/// How many pixels of `after` differ from `before` by more than a shade, inside `rect` and outside
/// it.
fn changed_in(
    before: &tilo_compositor::Screenshot,
    after: &tilo_compositor::Screenshot,
    rect: (f64, f64, f64, f64),
) -> (usize, usize) {
    changed_near(before, after, rect, &[rect])
}

/// The bar's trailing group, in order: who is here, the device's status, the bell and the person's
/// own face, with a gap between who is here and the status, and the person's face at the far end.
#[test]
fn the_trailing_group_stands_in_order_and_ends_with_the_persons_face() {
    let _settled = ui::motion::testing::settled();
    let mut session = device_session(WIDTH, HEIGHT);
    until_settled(&mut session, |session| says(session, &time(session).clock));
    set_here(&session, 2);
    until_settled(&mut session, |session| says(session, "are here"));
    let people = bar_control(&session, "are here").expect("who is here");
    let sound = bar_control(&session, "Volume").expect("the sound's level");
    let link = bar_control(&session, "Wi-Fi").expect("the connection");
    let pack = bar_control(&session, "61%, charging").expect("the charge");
    let bell = bar_control(&session, "Notifications").expect("the bell");
    let me = bar_control(&session, PERSON).expect("the person's own face");
    println!(
        "trailing group at {WIDTH}: who is here {people:?}, sound {sound:?}, link {link:?}, \
         charge {pack:?}, bell {bell:?}, me {me:?}"
    );
    let order = [
        ("who is here", people),
        ("the sound", sound),
        ("the connection", link),
        ("the charge", pack),
        ("the bell", bell),
        ("the person's face", me),
    ];
    for pair in order.windows(2) {
        let (first, second) = (pair[0], pair[1]);
        assert!(
            first.1.0 + first.1.2 <= second.1.0,
            "{} stands before {}: {first:?} {second:?}",
            first.0,
            second.0
        );
    }
    assert!(
        sound.0 - (people.0 + people.2) > 6.0,
        "a gap stands between who is here and the device's status: {people:?} {sound:?}"
    );
    let rightmost = bar_band_controls(&session)
        .into_iter()
        .map(|(_, rect)| rect.0 + rect.2)
        .fold(f64::MIN, f64::max);
    assert!(
        (me.0 + me.2 - rightmost).abs() < 0.5,
        "the person's own face is the last control, at the far end: {me:?} of {rightmost}"
    );
    assert!(
        f64::from(WIDTH) - (me.0 + me.2) < 24.0,
        "and stands at the bar's trailing edge: {me:?}"
    );
}

/// The bar is the theme's own bar material, chosen by the person's style: the glass of the
/// translucent style and the calm ground of the solid one, on one view, over one shell.
#[test]
fn the_bar_draws_the_material_of_the_style_the_person_chose() {
    let _settled = ui::motion::testing::settled();
    let mut session = device_session(WIDTH, HEIGHT);
    until_settled(&mut session, |session| says(session, &time(session).clock));
    set_here(&session, 2);
    until_settled(&mut session, |session| says(session, "are here"));
    // The shell's first frames draw every translucent body before the compositor has handed it what
    // stands below it; let the session reach its rest before the frame is kept, as the captures
    // after a setting change do.
    settle(&mut session);
    let calm = keep(&mut session, "shell-bar-calm");
    session
        .run_shell_action("set_bar_finish", vec![serde_json::json!("translucent")])
        .expect("the bar takes the theme's glass");
    settle(&mut session);
    let glass = keep(&mut session, "shell-bar-glass");
    let band = bar_band(&session);
    // The theme's glass lets the ground through it: over the shell's flat backdrop it moves the
    // whole band by a level rather than by a shade, so every pixel of the band that differs at all
    // counts as the other material. The ground paints the bar's whole body, the Chrome's padding
    // around the controls included: what changes outside the controls' bounds is still the bar.
    let ground = (band.0 - 8.0, band.1 - 8.0, band.2 + 16.0, band.3 + 16.0);
    let (in_bar, under) = changed_by(
        &calm,
        &glass,
        (0.0, 0.0, f64::from(glass.width), band.3),
        &[ground],
        0,
    );
    assert!(
        in_bar > 10_000,
        "the bar's ground is another material: {in_bar} pixels in its band {band:?}"
    );
    assert_eq!(
        under, 0,
        "the shell under the bar is the same in both styles: one view, not two"
    );
    session
        .run_shell_action("set_bar_finish", vec![serde_json::json!("solid")])
        .expect("and back to the calm ground");
    settle(&mut session);
    let back = keep(&mut session, "shell-bar-calm-again");
    // The clock may turn its minute between the two frames; nothing else may differ.
    let mut allowed = vec![];
    allowed.extend(bar_control(&session, &time(&session).clock));
    let (_, outside) = changed_by(&calm, &back, (0.0, 0.0, 0.0, 0.0), &allowed, 1);
    assert_eq!(outside, 0, "the same style draws the same bar");
}

/// The charge's control is the power service's own state: a pack taking charge shows the charging
/// battery, a pack that drains the plain one, and the bar says which.
#[test]
fn the_charge_shows_the_charging_battery_while_the_pack_takes_charge() {
    let _settled = ui::motion::testing::settled();
    let mut taking = device_session_over(WIDTH, HEIGHT, Device { charging: true });
    until_settled(&mut taking, |session| says(session, "61%, charging"));
    let charging = keep(&mut taking, "shell-bar-charging");
    let mut draining = device_session_over(WIDTH, HEIGHT, Device { charging: false });
    until_settled(&mut draining, |session| {
        says(session, "61%") && !says(session, "charging")
    });
    let plain = keep(&mut draining, "shell-bar-draining");
    let people = bar_control(&taking, "People").expect("who is here");
    let plain_people = bar_control(&draining, "People").expect("who is here, draining");
    let pack = bar_control(&taking, "61%, charging").expect("the charge");
    // The trailing group is laid out from the width its readings take: the longer charge moves
    // everyone who stands at its leading side over, so what differs is the trailing group and
    // nothing before it — not the clock, the tabs or the launcher.
    let (_, _, _, band) = bar_band(&taking);
    let from = people.0.min(plain_people.0);
    // The two frames are of two sessions, kept seconds apart: the clock may turn its minute between
    // them, and its digits are not the charge.
    // The reading is the minute's, and the pass that draws it comes after the minute the shell now
    // says: wait for the bar to show it rather than take the minute the last frame drew.
    let drawn_clock = |session: &mut Session| bar_control(session, &time(session).clock);
    let clock = crate::common::wait_for(&mut taking, "the bar's clock", drawn_clock);
    let plain_clock =
        crate::common::wait_for(&mut draining, "the bar's clock, draining", drawn_clock);
    let left = clock.0.min(plain_clock.0);
    let right = (clock.0 + clock.2).max(plain_clock.0 + plain_clock.2);
    let allowed = [
        (from, 0.0, f64::from(charging.width) - from, band),
        (left, 0.0, right - left, band),
    ];
    let (glyph, outside) = changed_near(&charging, &plain, pack, &allowed);
    assert!(
        glyph > 20,
        "the charging battery's glyph is another glyph: {glyph} pixels"
    );
    assert_eq!(
        outside, 0,
        "and only the charge's reading changed between the two: {outside} pixels elsewhere"
    );
}

/// A compact bar keeps what a glance needs — the clock, the connection, the charge, the bell and
/// the person's own face — and gives way on who is here and the sound: nothing stands past the
/// screen's edge, however much the device reads.
#[test]
fn a_compact_bar_keeps_a_glance_and_nothing_overflows_it() {
    let _settled = ui::motion::testing::settled();
    for (width, height) in [(320, 568), (390, 844)] {
        let mut session = device_session(width, height);
        until_settled(&mut session, |session| says(session, &time(session).clock));
        set_here(&session, 4);
        until_settled(&mut session, |session| shell(session).people.len() == 4);
        let clock = time(&session).clock;
        for words in [
            clock.as_str(),
            "Wi-Fi",
            "61%, charging",
            "Notifications",
            PERSON,
        ] {
            crate::common::wait_for(
                &mut session,
                format!("a compact bar keeping {words:?}"),
                |session| says(session, words).then_some(()),
            );
        }
        for words in ["Volume", "are here"] {
            crate::common::wait_for(
                &mut session,
                format!("a compact bar giving way on {words:?}"),
                |session| (!says(session, words)).then_some(()),
            );
        }
        crate::common::wait_for(
            &mut session,
            format!("the bar to stand inside {width}'s edge"),
            |session| {
                bar_band_controls(session)
                    .into_iter()
                    .find(|(_, (x, _, w, _))| *x < -0.5 || x + w > f64::from(width) + 0.5)
                    .is_none()
                    .then_some(())
            },
        );
        // The trailing group ends inside the bar's band, not over its edge.
        let (_, y, _, h) = bar_control(&session, PERSON).expect("the person's face");
        let band = bar_band(&session);
        assert!(
            y >= band.1 && y + h <= band.1 + band.3 + 0.5,
            "and stands in the bar's band: {y} {h} of {band:?}"
        );
        keep(&mut session, &format!("shell-bar-{width}-compact"));
    }
}

/// The bar at 1280, 390 and 320 wide, in both styles, light and dark, with nobody, one person and
/// four here: what a person sees, and that nothing overflows any of them.
#[test]
fn the_bar_draws_inside_every_width_style_and_appearance() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let roomy = |width: i32| width >= 600;
    for (width, height) in [(1280, 800), (390, 844), (320, 568)] {
        for count in [0, 1, 4] {
            // Who is here first, on a bar of the plain theme: a person is told who is here before
            // they change how the bar looks, and the frames kept are that bar.
            let mut session = device_session(width, height);
            until_settled(&mut session, |session| says(session, &time(session).clock));
            set_here(&session, count);
            let who = if count == 1 { "is here" } else { "are here" };
            until_settled(&mut session, |session| {
                shell(session).people.len() == count
                    && (count == 0 || !roomy(width) || says(session, who))
            });
            for (appearance, dark) in [("light", false), ("dark", true)] {
                set_appearance(&mut session, dark);
                until_settled(&mut session, |session| {
                    appearance_mode(session) == serde_json::json!(appearance)
                });
                for style in ["calm", "glass"] {
                    let finish = if style == "glass" {
                        "translucent"
                    } else {
                        "solid"
                    };
                    session
                        .run_shell_action("set_bar_finish", vec![serde_json::json!(finish)])
                        .expect("the bar takes the style");
                    settle(&mut session);
                    let shot = keep(
                        &mut session,
                        &format!("bar-{width}-{style}-{appearance}-{count}"),
                    );
                    assert_eq!(
                        (shot.width, shot.height),
                        (
                            u32::try_from(width).unwrap(),
                            u32::try_from(height).unwrap()
                        ),
                        "the capture is of the whole screen"
                    );
                    crate::common::wait_for(
                        &mut session,
                        format!(
                            "{width} wide, {style} in {appearance}, {count} here: nothing overflows"
                        ),
                        |session| {
                            let band = bar_band(session);
                            bar_band_controls(session)
                                .into_iter()
                                .find(|(_, (x, y, w, h))| {
                                    *x < -0.5
                                        || x + w > f64::from(width) + 0.5
                                        || *y < band.1 - 0.5
                                        || y + h > band.1 + band.3 + 0.5
                                })
                                .is_none()
                                .then_some(())
                        },
                    );
                }
            }
        }
    }
}

/// The panel's own tile works at the display's scale: at 2 a press at the centre of "Dark mode"
/// turns the published appearance, as it does at 1. A control reported at half its size put a press
/// at its centre past it, so a click did nothing.
#[test]
fn a_press_on_a_quick_settings_tile_works_at_every_scale() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    for (scale, pixels) in [(1.0, 1), (2.0, 2)] {
        let mut session = Session::headless(
            Options {
                services: vec![
                    Box::new(settings_sections::service::SettingsService::new()),
                    Box::new(speaker()),
                ],
                collab: false,
                ..options()
            },
            &[OutputConfig::new("HEADLESS-1", WIDTH * pixels, HEIGHT * pixels).with_scale(scale)],
        )
        .expect("session");
        until_settled(&mut session, |session| says(session, &time(session).clock));
        session
            .invoke(
                tilo_session::shell::part::CHROME_RESOURCE,
                "quick-settings",
                &serde_json::Map::new(),
            )
            .unwrap();
        until_settled(&mut session, |session| says(session, "Dark mode"));
        // The tile as the session publishes it, in the shell's logical pixels: a press at its centre
        // is the one the person makes.
        until_settled(&mut session, |session| {
            session
                .controls()
                .iter()
                .any(|control| control.name == "Dark mode")
        });
        let (x, y, w, h) = session
            .controls()
            .into_iter()
            .find(|control| control.name == "Dark mode")
            .map(|control| control.rect)
            .expect("the panel shows its Dark mode tile");
        let dark = serde_json::json!("dark");
        let was = appearance_mode(&session) == dark;
        let (x, y) = (x + w / 2.0, y + h / 2.0);
        session
            .compositor()
            .input(InputEvent::PointerMotion { x, y })
            .unwrap();
        session.step(Duration::from_millis(1)).unwrap();
        for pressed in [true, false] {
            session
                .compositor()
                .input(InputEvent::PointerButton {
                    button: BTN_LEFT,
                    pressed,
                })
                .unwrap();
            session.step(Duration::from_millis(1)).unwrap();
        }
        until_settled(&mut session, |session| {
            (appearance_mode(session) == dark) != was
        });
        assert!(
            (appearance_mode(&session) == dark) != was,
            "at {scale} a press at the Dark mode tile turns the appearance: it shows at {:?}",
            session.controls()
        );
    }
}

/// Step 1 of shell.md §6, "The shell loads from the store": the shell the session runs is the
/// installed `so.tilo.shell` — its package as `tilolang build` signed it, read from the app
/// store — and not this build's copy. The test's store holds the shell (the common app store
/// installs it as it installs every app), so the shell's instance runs the installed module:
/// its per-app id is the one it declares, and the shell it draws is the same shell.
#[test]
fn the_shell_loads_from_the_package_store_and_not_the_builds_copy() {
    let _settled = ui::motion::testing::settled();
    // The store the session reads: the test's, where the shell is installed as an app is.
    let store = crate::common::app_store();
    assert!(
        app_catalogue::apps::read_one_from_env("so.tilo.shell")
            .expect("the store reads")
            .is_some(),
        "the shell is installed in the session's app store at {}",
        store.display()
    );
    // The installed package is the one the session compiles: the same app id, and the module
    // the session runs is the store's, not this build's `include_str!` copy.
    let (installed, from_store) =
        tilo_session::shell::shell_packages().expect("the shell package loads");
    assert!(
        from_store,
        "the session loads the shell from the store, not its fallback copy"
    );
    assert_eq!(
        installed.last().map(|package| package.meta.app.as_str()),
        Some("so.tilo.shell"),
        "the installed shell is the last package, as a bundle holds it"
    );
    // The store's module is the one the signed `.tapp` carries; this build's copy is compiled
    // from source and cannot be byte-identical to it, so the two differ where the path did its
    // work: a module compiled from `include_str!` and one the build staged are not the same
    // bytes.
    let fallback = tilo_session::shell::part::compile_shell().expect("the fallback compiles");
    assert_ne!(
        fallback.last().map(|package| package.wasm.as_slice()),
        installed.last().map(|package| package.wasm.as_slice()),
        "the installed shell runs its own signed module, not the build's compiled copy"
    );
    let mut session = session();
    until(&mut session, |session| says(session, &time(session).clock));
    assert!(
        session.shell_apps().is_empty(),
        "a place shows no app yet: the shell alone runs"
    );
    // The shell the installed shell draws is the shell: its bar names the person the session
    // publishes, so what runs is a shell and not a blank screen.
    assert!(
        says(&session, PERSON),
        "the installed shell draws the bar: {:?}",
        session.accessible_labels()
    );
}

/// A Nearby Send resource as a test publishes it: the rows the panel shows and the actions its
/// presses reach, so the panel is read without a device fabric.
struct Nearby {
    reading: serde_json::Value,
    calls: Rc<std::cell::RefCell<Vec<(String, serde_json::Map<String, serde_json::Value>)>>>,
}

impl tilo_session::Service for Nearby {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        vec![tilo_workspace_service::apps::nearby_send::resource(
            "Nearby Send",
        )]
    }

    fn read(&self, resource: &str) -> Option<serde_json::Value> {
        (resource == tilo_workspace_service::apps::nearby_send::RESOURCE)
            .then(|| self.reading.clone())
    }

    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::ApiError> {
        if resource != tilo_workspace_service::apps::nearby_send::RESOURCE {
            return Err(tilo_actions::ApiError::Unknown(
                tilo_actions::ActionAddress::new(resource, action),
            ));
        }
        self.calls
            .borrow_mut()
            .push((action.to_owned(), arguments.clone()));
        Ok(tilo_service::Started::Done(
            tilo_actions::ActionOutput::default(),
        ))
    }
}

/// The Devices section lists what is in reach, and its rows run the resource's own actions: an
/// offer is kept by naming the transfer the row showed, and a failed send is offered again.
#[test]
fn the_quick_settings_devices_section_keeps_an_offer_and_tries_again() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let calls = Rc::new(std::cell::RefCell::new(Vec::new()));
    let nearby = Nearby {
        reading: serde_json::json!({
            "devices": [
                {"name": "Ada's laptop", "kind": "device", "reachable": true,
                 "address": "", "port": 0, "handle": "11111111111111111111111111111111"}
            ],
            "destinations": [
                {"name": "Documents", "handle": "22222222222222222222222222222222", "remembered": false}
            ],
            "transfers": [
                {"state": "offered", "progress": 0, "peer": "Ada's laptop", "destination": "",
                 "time": 0, "incoming": true, "name": "notes.txt",
                 "handle": "33333333333333333333333333333333"},
                {"state": "failed", "progress": 40, "peer": "Ada's laptop", "destination": "",
                 "time": 0, "incoming": false, "name": "report.pdf",
                 "handle": "44444444444444444444444444444444"}
            ],
        }),
        calls: Rc::clone(&calls),
    };
    let mut session = Session::headless(
        Options {
            services: vec![
                Box::new(settings_sections::service::SettingsService::new()),
                Box::new(speaker()),
                Box::new(Device { charging: true }),
                Box::new(nearby),
            ],
            ..options()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, TALL)],
    )
    .expect("session");
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(1),
        rate: 0,
    });
    until_settled(&mut session, |session| says(session, &time(session).clock));
    press_at(&mut session, "Volume, 40%", WIDTH, TALL);
    until_settled(&mut session, |session| here(session) == "quick-settings");
    until_settled(&mut session, |session| says(session, "notes.txt"));
    assert!(
        says(&session, "Ada's laptop offers this"),
        "the offer names who made it: {:?}",
        session.accessible_labels()
    );
    press_at(&mut session, "Keep it", WIDTH, TALL);
    until_settled(&mut session, |_| {
        calls.borrow().iter().any(|(action, _)| action == "accept")
    });
    assert!(
        calls.borrow().iter().any(|(action, arguments)| {
            action == "accept"
                && arguments.get("transfer").and_then(|value| value.as_str())
                    == Some("33333333333333333333333333333333")
        }),
        "the press names the offer the row showed: {:?}",
        calls.borrow()
    );
    press_at(&mut session, "Try again", WIDTH, TALL);
    until_settled(&mut session, |_| {
        calls
            .borrow()
            .iter()
            .any(|(action, _)| action == "retry-transfer")
    });
    keep(&mut session, "shell-devices-nearby");
}

/// Each app playing now stands on its own row: its level and mute, and where it plays, so one app
/// can be silenced without the rest. The press is the sound's published `set-stream-muted`.
#[test]
fn the_quick_settings_silences_one_app_and_says_where_it_plays() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = Session::headless(
        Options {
            services: vec![
                Box::new(settings_sections::service::SettingsService::new()),
                Box::new(speaker_with_streams()),
                Box::new(Device { charging: true }),
            ],
            ..options()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, TALL)],
    )
    .expect("session");
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(1),
        rate: 0,
    });
    until_settled(&mut session, |session| says(session, &time(session).clock));
    press_at(&mut session, "Volume, 40%", WIDTH, TALL);
    until_settled(&mut session, |session| here(session) == "quick-settings");
    until_settled(&mut session, |session| says(session, "A song"));
    assert!(
        says(&session, "Play Music through"),
        "the app's row says where it plays: {:?}",
        session.accessible_labels()
    );
    let muted = |session: &Session| -> bool {
        session
            .read("sound")
            .and_then(|reading| reading["streams"][0]["muted"].as_bool())
            .unwrap_or(false)
    };
    assert!(!muted(&session), "the song plays at first");
    press_at(&mut session, "Mute Music", WIDTH, TALL);
    until_settled(&mut session, |session| muted(session));
    assert!(
        says(&session, "Unmute Music"),
        "the row offers the app back: {:?}",
        session.accessible_labels()
    );
    keep(&mut session, "shell-quick-settings-streams");
}

/// The hardware keys' readout: the volume key raises the sound and the shell says so over what it
/// shows, from the reading the session publishes, without a window.
#[test]
fn a_volume_key_shows_the_readout_over_the_shell() {
    const VOLUME_UP: u32 = 115;
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = device_session(WIDTH, HEIGHT);
    until_settled(&mut session, |session| says(session, &time(session).clock));
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::Key {
                keycode: VOLUME_UP,
                pressed,
            })
            .expect("the key is taken");
        session.step(Duration::from_millis(1)).expect("step");
    }
    let osd = session
        .read("session:osd")
        .expect("the readout is published");
    assert_eq!(
        osd["visible"],
        serde_json::json!(true),
        "the key's readout stands: {osd}"
    );
    assert_eq!(osd["kind"], serde_json::json!("volume"), "{osd}");
    assert!(
        osd["level"].as_f64().is_some_and(|level| level > 0.4),
        "the readout is the level the key set, above the 40% it started at: {osd}"
    );
    until_settled(&mut session, |session| says(session, "Volume"));
    keep(&mut session, "shell-osd-volume");
}

/// A laptop's radios in memory, as the connectivity service reads them: the panel's tiles change
/// the radios themselves, and the reading that follows is the service's, not the view's.
#[derive(Default)]
struct Radios {
    readings: std::sync::Mutex<tilo_connectivity_service::device::Readings>,
}

impl tilo_connectivity_service::device::Device for Radios {
    fn read(&self) -> tilo_connectivity_service::device::Readings {
        self.readings.lock().unwrap().clone()
    }

    fn change(&self, change: &tilo_connectivity_service::device::Change) -> Result<(), String> {
        let mut readings = self.readings.lock().unwrap();
        match change {
            tilo_connectivity_service::device::Change::Wifi(on) => {
                readings.network.wifi.on = *on;
            }
            tilo_connectivity_service::device::Change::Airplane(on) => {
                readings.network.airplane_mode = *on;
            }
            tilo_connectivity_service::device::Change::Bluetooth(on) => {
                readings.bluetooth.on = *on;
            }
            tilo_connectivity_service::device::Change::Join { network, .. } => {
                for reachable in &mut readings.network.wifi.networks {
                    reachable.connected = reachable.name == *network;
                }
            }
            tilo_connectivity_service::device::Change::Autoconnect { profile, on } => {
                for saved in &mut readings.network.saved {
                    if saved.profile == *profile {
                        saved.autoconnect = *on;
                    }
                }
            }
            tilo_connectivity_service::device::Change::Vpn { profile, on } => {
                for vpn in &mut readings.vpn.profiles {
                    if vpn.id == *profile {
                        vpn.connected = *on;
                    }
                }
            }
            other => return Err(format!("{other:?} is not part of this laptop")),
        }
        Ok(())
    }

    fn scan_operators(&self, _modem: &str) -> Result<Vec<os_actions::Operator>, String> {
        Err("this laptop has no modem".into())
    }

    fn send_code(&self, _modem: &str, _command: &str) -> Result<os_actions::CodeAnswer, String> {
        Err("this laptop has no modem".into())
    }

    fn answer_code(&self, _modem: &str, _response: &str) -> Result<os_actions::CodeAnswer, String> {
        Err("this laptop has no modem".into())
    }

    fn cancel_code(&self, _modem: &str) -> Result<(), String> {
        Err("this laptop has no modem".into())
    }
}

/// A session whose laptop has Wi-Fi with one open network in reach and one it keeps, Bluetooth,
/// the person's settings, and a sound card: what the panel's switches change.
fn wireless_session() -> Session {
    let radios = Radios::default();
    {
        let mut readings = radios.readings.lock().unwrap();
        readings.network.wifi.available = true;
        readings.network.wifi.on = true;
        readings.network.wifi.networks = vec![tilo_connectivity_service::model::WifiNetwork {
            name: "Cafe".to_owned(),
            signal: 3,
            secure: false,
            ..tilo_connectivity_service::model::WifiNetwork::default()
        }];
        readings.network.saved = vec![tilo_connectivity_service::model::SavedNetwork {
            name: "Home".to_owned(),
            profile: "uuid-home".to_owned(),
            ..tilo_connectivity_service::model::SavedNetwork::default()
        }];
        readings.bluetooth.available = true;
        readings.vpn.profiles = vec![tilo_connectivity_service::model::VpnProfile {
            name: "Work".to_owned(),
            id: "work".to_owned(),
            kind: "wireguard".to_owned(),
            connected: false,
            ..tilo_connectivity_service::model::VpnProfile::default()
        }];
    }
    let mut connectivity =
        tilo_connectivity_service::Connectivity::new(std::sync::Arc::new(radios), None)
            .expect("the radios serve");
    connectivity.settle();
    let mut session = Session::headless(
        Options {
            services: vec![
                Box::new(connectivity),
                Box::new(settings_sections::service::SettingsService::new()),
                Box::new(speaker_with_microphone()),
            ],
            ..options()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, TALL)],
    )
    .expect("session");
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(1),
        rate: 0,
    });
    until_settled(&mut session, |session| says(session, &time(session).clock));
    session
}

/// The panel's switches are the published actions of the resources they name: the radios change,
/// the person's settings change, and the sound's mute and the microphone's are the sound's own.
#[test]
fn the_quick_settings_switches_change_what_they_publish() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = wireless_session();
    let published = |session: &Session, resource: &str| {
        session.read(resource).expect("the resource is published")
    };
    let section =
        |kind: &str| settings_sections::address(kind).expect("a section the session keeps");
    press_at(&mut session, "Volume, 40%", WIDTH, TALL);
    until_settled(&mut session, |session| here(session) == "quick-settings");
    until_settled(&mut session, |session| says(session, "Cafe"));

    press_at(&mut session, "Wi-Fi", WIDTH, TALL);
    until_settled(&mut session, |session| {
        published(session, "network")["wifi"]["on"] == serde_json::json!(false)
    });
    press_at(&mut session, "Wi-Fi", WIDTH, TALL);
    until_settled(&mut session, |session| {
        published(session, "network")["wifi"]["on"] == serde_json::json!(true)
    });
    // The network in reach stands in the panel, and its row joins it: the press is the
    // network's own `join`, on its own resource, as any caller's would be. The row then says so,
    // and the Wi-Fi tile takes the network's name.
    until_settled(&mut session, |session| says(session, "Cafe, Open"));
    press_at(&mut session, "Cafe", WIDTH, TALL);
    until_settled(&mut session, |session| {
        published(session, "network")["wifi"]["networks"][0]["connected"] == serde_json::json!(true)
    });
    until_settled(&mut session, |session| says(session, "Cafe, Connected"));
    press_at(&mut session, "Bluetooth", WIDTH, TALL);
    until_settled(&mut session, |session| {
        published(session, "bluetooth")["on"] == serde_json::json!(true)
    });
    press_at(&mut session, "Do not disturb", WIDTH, TALL);
    until_settled(&mut session, |session| {
        published(session, &section("settings.notifications"))["do_not_disturb"]
            == serde_json::json!(true)
    });
    press_at(&mut session, "Rotation lock", WIDTH, TALL);
    until_settled(&mut session, |session| {
        published(session, &section("settings.display"))["rotation_lock"] == serde_json::json!(true)
    });
    press_at(&mut session, "VPN", WIDTH, TALL);
    until_settled(&mut session, |session| {
        published(session, "vpn")["profiles"][0]["connected"] == serde_json::json!(true)
    });
    press_at(&mut session, "Work", WIDTH, TALL);
    until_settled(&mut session, |session| {
        published(session, "vpn")["profiles"][0]["connected"] == serde_json::json!(false)
    });
    press_at(&mut session, "Mute", WIDTH, TALL);
    until_settled(&mut session, |session| {
        published(session, "sound")["muted"] == serde_json::json!(true)
    });
    press_at(&mut session, "Mute the microphone", WIDTH, TALL);
    until_settled(&mut session, |session| {
        published(session, "sound")["input-muted"] == serde_json::json!(true)
    });
    press_at(&mut session, "Airplane mode", WIDTH, TALL);
    until_settled(&mut session, |session| {
        published(session, "network")["airplane-mode"] == serde_json::json!(true)
    });
    keep(&mut session, "shell-quick-settings-switches");
}

/// A network the laptop keeps is its own resource with its own actions: the row's menu sets it to
/// join by itself, the saved network's own `set-autoconnect` on its own address, and the row says
/// so once the network does.
#[test]
fn a_saved_networks_menu_sets_it_to_join_by_itself() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = wireless_session();
    let saved = |session: &Session| {
        session.read("network").expect("the network is published")["saved"].clone()
    };
    press_at(&mut session, "Volume, 40%", WIDTH, TALL);
    until_settled(&mut session, |session| here(session) == "quick-settings");
    until_settled(&mut session, |session| says(session, "Home, Saved"));
    press_at(&mut session, "This network", WIDTH, TALL);
    until_settled(&mut session, |session| says(session, "Join by itself"));
    press_at(&mut session, "Join by itself", WIDTH, TALL);
    until_settled(&mut session, |session| {
        saved(session)[0]["autoconnect"] == serde_json::json!(true)
    });
    until_settled(&mut session, |session| {
        says(session, "Home, Joins by itself")
    });
    assert!(
        session.shell_refusals().is_empty(),
        "the menu's choice reached the saved network: {:?}",
        session.shell_refusals()
    );
}

/// The panel draws the tiles the person's setting keeps, and hiding one takes it off: the
/// editor's press is the published `hide-control`, and the panel that follows draws without it.
#[test]
fn hiding_a_tile_takes_it_off_the_panel() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = device_session_over(WIDTH, TALL, Device { charging: true });
    let controls = |session: &Session| -> Vec<String> {
        session
            .read("session:quick-settings")
            .and_then(|reading| {
                serde_json::from_value::<Vec<String>>(reading["controls"].clone()).ok()
            })
            .unwrap_or_default()
    };
    assert!(controls(&session).contains(&"wifi".to_owned()));
    until_settled(&mut session, |session| says(session, &time(session).clock));
    press_at(&mut session, "Volume, 40%", WIDTH, TALL);
    until_settled(&mut session, |session| here(session) == "quick-settings");
    until_settled(&mut session, |session| says(session, "Customise the panel"));
    press_at(&mut session, "Customise the panel", WIDTH, TALL);
    until_settled(&mut session, |session| says(session, "Hide Bluetooth"));
    press_at(&mut session, "Hide Bluetooth", WIDTH, TALL);
    until_settled(&mut session, {
        |session| !controls(session).contains(&"bluetooth".to_owned())
    });
    assert!(
        says(&session, "Not on the panel"),
        "the editor offers the hidden tile back: {:?}",
        session.accessible_labels()
    );
    keep(&mut session, "shell-quick-settings-editor");
}
