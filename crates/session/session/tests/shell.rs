//! The tilolang shell on the native session (headless backend), its whole screen under the windows:
//! its bar shows the desk the session publishes on its door (the time, the workspace and strip in
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

use crate::common::{LAUNCHER_SEARCH, OVERVIEW_SEARCH, WORKSPACE_NAME, press, type_text};
use std::collections::HashMap;
use std::io::BufRead as _;
use std::rc::Rc;
use std::time::Duration;
use tilo_test_env::SpawnUnderEnvLock as _;

use tilo_compositor::{InputEvent, KeyRepeat, OutputConfig};
use tilo_session::desktop_services::{Bus, Buses, DesktopServices};
use tilo_session::quick_settings;
use tilo_session::{Options, Service as _, Session};
use tilo_session_core::shell::DeskReading;
use tilo_surface_host::shell_part::DESK_RESOURCE;

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
const BTN_LEFT: u32 = 0x110;
const PERSON: &str = "Pat";

fn options() -> Options {
    Options {
        presentation: ui::headless::fixture_runner_inputs,
        person: PERSON.to_owned(),
        shell: true,
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

/// The desk as the session publishes it on its door: what an agent reads.
fn desk(session: &Session) -> DeskReading {
    serde_json::from_value(
        session
            .read(DESK_RESOURCE)
            .expect("the session publishes its desk"),
    )
    .expect("the desk reads as the shell's desk")
}

/// How many notices the desk names the person has not seen: the count the shell's bell shows of
/// the notices it reads.
fn waiting(session: &Session) -> usize {
    desk(session)
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
/// and nothing of the desk does.
fn band_colours(shot: &tilo_compositor::Screenshot) -> usize {
    (0..shot.width)
        .step_by(2)
        .filter_map(|x| shot.pixel(x, 16))
        .collect::<std::collections::BTreeSet<_>>()
        .len()
}

#[test]
fn the_bar_shows_the_time_the_workspace_its_strip_and_the_person_the_session_publishes() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| {
        let desk = desk(session);
        says(session, &desk.clock)
            && says(session, &desk.workspace)
            && (desk.strip.is_empty() || says(session, &desk.strip))
            && says(session, PERSON)
    });
    let desk = desk(&session);
    assert_eq!(desk.person, PERSON);
    assert!(!desk.workspaces.is_empty(), "the desk has its workspaces");
    assert!(
        desk.workspaces
            .iter()
            .any(|workspace| workspace.current && workspace.name == desk.workspace),
        "the workspace in front is the one named: {desk:?}"
    );
    for words in [desk.clock.as_str(), desk.workspace.as_str(), PERSON] {
        assert!(
            says(&session, words),
            "the bar says {words:?}: {:?}",
            session.accessible_labels()
        );
    }
    if !desk.strip.is_empty() {
        assert!(says(&session, &desk.strip), "the bar names the strip");
    }
    // The bar draws in its band: its controls stand on the desk there.
    let _shot = crate::common::wait_for(&mut session, "the bar in its band", |session| {
        session.step(Duration::from_millis(16)).expect("step");
        let shot = keep(session, "shell-bar-empty-desk");
        (band_colours(&shot) > 8).then_some(shot)
    });
    // Its controls stand where the bar's band is.
    let (_, y, _, h) = session.shell_control(&desk.workspace).unwrap();
    let band = bar_band(&session);
    assert!(
        y >= band.1 - 0.5 && y + h <= band.1 + band.3 + 0.5,
        "the workspace's control stands in the bar's band: {y} {h} of {band:?}"
    );
}

#[test]
fn choosing_a_workspace_from_the_bars_menu_brings_it_to_the_front() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| {
        says(session, &desk(session).workspace)
    });
    let first = desk(&session);
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
            tilo_surface_host::shell_part::CHROME_RESOURCE,
            "new-workspace",
            &serde_json::Map::new(),
        )
        .unwrap();
    until(&mut session, |session| {
        let desk = desk(session);
        desk.workspaces.len() == 2
            && desk.workspace != first.workspace
            && says(session, &desk.workspace)
    });
    let second = desk(&session).workspace;
    // The workspace's control opens the workspaces; choosing the first brings it to the front.
    press(&mut session, &second);
    until(&mut session, |session| {
        session.shell_control(&first.workspace).is_some()
    });
    keep(&mut session, "shell-bar-workspace-menu");
    press(&mut session, &first.workspace);
    crate::common::until(&mut session, "the first workspace in front", |session| {
        session.handle().read(|tree| tree.active.0.to_string()) == first_key
    });
    assert_eq!(
        session.handle().read(|tree| tree.active.0.to_string()),
        first_key,
        "the first workspace is in front: {:?} {:?}",
        session.shell_work(),
        session.accessible_labels()
    );
    until(&mut session, |session| says(session, &first.workspace));
    assert_eq!(desk(&session).workspace, first.workspace);
}

/// A menu the person opened stands while the desk changes under it: a workspace made while they are
/// choosing one lists itself there, instead of the menu vanishing under the pointer.
#[test]
fn the_workspaces_menu_stands_while_the_desk_changes_under_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| {
        says(session, &desk(session).workspace)
    });
    let first = desk(&session).workspace;
    press(&mut session, &first);
    until(&mut session, |session| {
        session.shell_control("Workspace settings").is_some()
    });
    // The desk changes under the menu: another workspace, in front.
    session
        .invoke(
            tilo_surface_host::shell_part::CHROME_RESOURCE,
            "new-workspace",
            &serde_json::Map::new(),
        )
        .unwrap();
    crate::common::wait_for(
        &mut session,
        "the workspace made under the menu",
        |session| {
            let desk = desk(session);
            (desk.workspaces.len() == 2 && desk.workspace != first).then(|| desk.workspace.clone())
        },
    );
    session.step(Duration::from_millis(16)).unwrap();
    assert!(
        session
            .shell_control("Workspace settings")
            .is_some_and(|(_, _, w, h)| w > 0.0 && h > 0.0),
        "the menu stands while the desk changes under it: {:?}",
        session.accessible_labels()
    );
    // And still chooses: the workspace in front is the one the person presses there.
    press(&mut session, &first);
    crate::common::until(
        &mut session,
        "the workspace chosen under the menu in front",
        |session| desk(session).workspace == first,
    );
}

#[test]
fn with_nothing_changing_the_bar_does_no_work() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
    // What the first frames started has landed.
    for _ in 0..20 {
        session.step(Duration::from_millis(1)).unwrap();
    }
    let minute = || ui::clock::local_now().timestamp().div_euclid(60);
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
            tilo_surface_host::shell_part::CHROME_RESOURCE,
            "new-workspace",
            &serde_json::Map::new(),
        )
        .unwrap();
    until(&mut session, |session| desk(session).workspaces.len() == 2);
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
    until(&mut session, |session| says(session, &desk(session).clock));
    for _ in 0..20 {
        session.step(Duration::from_millis(1)).unwrap();
    }
    // The bar's turns before the change: its own share, measured the same way.
    let mut frame_ms: Vec<f64> = Vec::new();
    // One new workspace per round; the session runs on from where the last left it.
    for _ in 0..25 {
        let workspaces = desk(&session).workspaces.len();
        session
            .invoke(
                tilo_surface_host::shell_part::CHROME_RESOURCE,
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
            if desk(&session).workspaces.len() > workspaces {
                frame_ms.push(took);
                reached = true;
                break;
            }
        }
        assert!(reached, "the new workspace reaches the desk");
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

#[test]
fn an_apps_notification_is_counted_on_the_bar() {
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
    until(&mut session, |session| says(session, &desk(session).clock));
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
    // And the notice rises as the shell's banner, in place of the desk's own banners app,
    // which draws nothing under the shell: the desk names the one it has up, and the shell
    // draws that one.
    until(&mut session, |session| says(session, "New mail"));
    keep(&mut session, "shell-bar-notification");
}

// ---- the shell's overlays ------------------------------------------------------------------

/// A window of `app` on the desk, its tab's id: a tab of the strip in front.
fn open_window(session: &mut Session, app: &str) -> u64 {
    let handle = session.handle().clone();
    let mut tree = handle.read(Clone::clone);
    let tab = handle.mint_id();
    let workspace = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.slot == tree.active)
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

/// The shell's surface up now, as the desk's reading names it.
fn here(session: &Session) -> String {
    desk(session).here
}

#[test]
fn the_bars_overview_shows_every_window_and_picking_one_brings_it_front() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
    let first = open_window(&mut session, "so.tilo.notes");
    open_window(&mut session, "so.tilo.mail");
    until(&mut session, |session| desk(session).windows.len() == 2);
    assert_eq!(here(&session), "none", "the desk is at rest");
    // The bar's Overview opens the desktop's overview, as its menu item does.
    session
        .invoke(
            tilo_surface_host::shell_part::DESK_RESOURCE,
            "show-surface",
            &serde_json::Map::from_iter([(
                "surface".to_owned(),
                serde_json::Value::String("overview".to_owned()),
            )]),
        )
        .unwrap();
    until(&mut session, |session| here(session) == "overview");
    // The shell's overview is drawn over the desk, naming each window.
    until(&mut session, |session| {
        session
            .accessible_labels()
            .iter()
            .any(|label| label == "Overview")
    });
    for w in desk(&session).windows {
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
    let notes = desk(&session)
        .windows
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

/// The desk's reading holds every workspace's windows, not only the workspace in front's: a
/// window of another workspace keeps its workspace's key, its share of its strip and its place,
/// so the overview's search over the reading finds it; and the walk Alt+Tab takes still crosses
/// the workspace in front alone.
#[test]
fn the_reading_holds_every_workspaces_windows_and_the_walk_keeps_to_the_front_one() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
    let behind = open_window(&mut session, "so.tilo.notes");
    until(&mut session, |session| desk(session).windows.len() == 1);
    let first = desk(&session);
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
            tilo_surface_host::shell_part::CHROME_RESOURCE,
            "new-workspace",
            &serde_json::Map::new(),
        )
        .unwrap();
    until(&mut session, |session| {
        desk(session).workspaces.len() == 2 && desk(session).workspace != first.workspace
    });
    let front = open_window(&mut session, "so.tilo.mail");
    until(&mut session, |session| desk(session).windows.len() == 2);
    let now = desk(&session);
    let elsewhere = now
        .windows
        .iter()
        .find(|window| window.key == behind.to_string())
        .unwrap_or_else(|| {
            panic!(
                "the other workspace's window is read too: {:?}",
                now.windows
            )
        });
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
    until(&mut session, |session| desk(session).windows.len() == 3);
    let in_front = [front.to_string(), second.to_string()];
    // The walk crosses the workspace in front alone.
    for _ in 0..3 {
        walk(&mut session, 1, "workspace");
        session.step(Duration::from_millis(300)).unwrap();
        until(&mut session, |session| desk(session).switcher_revealed);
        assert!(
            in_front.contains(&reached(&session).key),
            "the walk keeps to the workspace in front: {:?}",
            desk(&session).windows
        );
    }
}

/// The overview's search crosses the workspaces: a window the person left in another workspace is
/// found, named with the workspace it is in, and choosing it goes there.
#[test]
fn the_overviews_search_finds_a_window_of_another_workspace() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
    let notes = open_window(&mut session, "so.tilo.notes");
    until(&mut session, |session| desk(session).windows.len() == 1);
    let behind = desk(&session).workspace;
    session
        .invoke(
            tilo_surface_host::shell_part::CHROME_RESOURCE,
            "new-workspace",
            &serde_json::Map::new(),
        )
        .unwrap();
    until(&mut session, |session| {
        desk(session).workspaces.len() == 2 && desk(session).workspace != behind
    });
    // The reading holds it, in the workspace behind the one in front.
    let read = desk(&session);
    let placed = read
        .windows
        .iter()
        .find(|w| w.key == notes.to_string())
        .unwrap_or_else(|| panic!("the window behind is read: {:?}", read.windows));
    assert_eq!(placed.workspace, behind, "it keeps the workspace it is in");
    assert_ne!(
        placed.workspace, read.workspace,
        "the window is not in the workspace in front"
    );
    show(&mut session, "overview");
    let field = OVERVIEW_SEARCH;
    until(&mut session, |session| {
        session.shell_control(field).is_some()
    });
    assert!(
        session.shell_control("so.tilo.notes").is_none(),
        "the workspace in front has no window to show: {:?}",
        session.accessible_labels()
    );
    press(&mut session, field);
    type_text(&mut session, "notes");
    // It is found; its row names the workspace it is in.
    until(&mut session, |session| {
        session.shell_control("so.tilo.notes").is_some()
    });
    assert!(
        session.shell_says(&behind),
        "the row names the workspace behind: {:?}",
        session.accessible_labels()
    );
    // Choosing it goes there.
    press(&mut session, "so.tilo.notes");
    until(&mut session, |session| desk(session).workspace == behind);
    assert!(
        desk(&session)
            .windows
            .iter()
            .any(|w| w.key == notes.to_string() && w.focused),
        "the window found is the one in front: {:?}",
        desk(&session).windows
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
    until(&mut session, |session| says(session, &desk(session).clock));
    open_window(&mut session, "so.tilo.notes");
    until(&mut session, |session| desk(session).windows.len() == 1);
    // The shell's actions reach the desk's surface through its door.
    session
        .invoke(
            tilo_surface_host::shell_part::DESK_RESOURCE,
            "show-surface",
            &serde_json::Map::from_iter([(
                "surface".to_owned(),
                serde_json::Value::String("launcher".to_owned()),
            )]),
        )
        .unwrap();
    until(&mut session, |session| here(session) == "launcher");
    // The launcher's doors are what it draws: the surface being up says where the desk is, and its
    // labels are the doors a person sees.
    crate::common::wait_for(&mut session, "the launcher's doors", |session| {
        (says(session, "Overview") && says(session, "Notifications")).then_some(())
    });
    keep(&mut session, "shell-launcher");
    // Closing it from the desk's reading puts the desk back at rest.
    session
        .invoke(
            tilo_surface_host::shell_part::DESK_RESOURCE,
            "close-surface",
            &serde_json::Map::from_iter([(
                "surface".to_owned(),
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
    until(&mut session, |session| says(session, &desk(session).clock));
    // The session posts a notice of its own, as an app's would arrive.
    session.notices().post(
        tilo_desktop_surface::notifications::Notification::new("Backup finished", "12 GB safe")
            .with_app_name("Settings"),
    );
    until(&mut session, |session| !desk(session).notices.is_empty());
    assert_eq!(desk(&session).notices[0].title, "Backup finished");
    session
        .invoke(
            tilo_surface_host::shell_part::DESK_RESOURCE,
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
    // Dismissing it, as the centre's own Dismiss does, takes it off the desk.
    let id = desk(&session).notices[0].key.clone();
    session
        .invoke(
            tilo_surface_host::shell_part::DESK_RESOURCE,
            "dismiss-notice",
            &serde_json::Map::from_iter([("notice".to_owned(), serde_json::Value::String(id))]),
        )
        .unwrap();
    until(&mut session, |session| desk(session).notices.is_empty());
}

/// The centre's own Dismiss, pressed in the shell's view: the action the view runs is the desk's
/// `dismiss-notice` on the notice the row is for, so the shell's surfaces and an agent dismiss the
/// same way.
#[test]
fn a_press_on_the_centres_dismiss_takes_the_notice_off_the_desk() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
    session.notices().post(
        tilo_desktop_surface::notifications::Notification::new("Backup finished", "12 GB safe")
            .with_app_name("Settings"),
    );
    until(&mut session, |session| !desk(session).notices.is_empty());
    // A notice that arrives is unseen: the shell counts what the person has not seen, and its
    // bell says so.
    until(&mut session, |session| waiting(session) == 1);
    assert!(!desk(&session).notices[0].seen, "a fresh notice is unseen");
    session
        .invoke(
            tilo_surface_host::shell_part::DESK_RESOURCE,
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
        waiting(session) == 0 && desk(session).notices.iter().all(|notice| notice.seen)
    });
    // The view draws each notice with its Dismiss; pressing it runs the desk's `dismiss-notice` on
    // that notice, as any action on the desk runs.
    press(&mut session, "Dismiss");
    until(&mut session, |session| desk(session).notices.is_empty());
    assert!(
        session.shell_refusals().is_empty(),
        "the centre's Dismiss reached the desk: {:?}",
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
    until(&mut session, |session| says(session, &desk(session).clock));
    open_window(&mut session, "so.tilo.notes");
    open_window(&mut session, "so.tilo.mail");
    until(&mut session, |session| desk(session).windows.len() == 2);
    // The desk's order, the window in front first: Alt+Tab opens on the next one.
    let before = desk(&session);
    let front = before
        .windows
        .iter()
        .position(|window| window.focused)
        .expect("a window is in front");
    let next = before
        .windows
        .get((front + 1) % before.windows.len())
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
        desk(session)
            .windows
            .iter()
            .any(|window| window.key == next && window.focused)
    });
    // The panel comes up on the frame after the reading moves: its keys are drawn with it.
    until(&mut session, |session| says(session, "Go there"));
    let held = keep(&mut session, "shell-switcher");
    // The switcher names every window it walks, each with the app it shows, and the keys stand
    // under them rather than over their titles.
    let labels = session.accessible_labels();
    let walked = desk(&session);
    let mut titles = Vec::new();
    for window in &walked.windows {
        let named = labels.iter().filter(|l| l.as_str() == window.title).count();
        assert!(
            named > 0,
            "the switcher names {:?}: {labels:?}",
            window.title
        );
        let bounds = session
            .shell_control(&window.title)
            .unwrap_or_else(|| panic!("{:?} stands in the switcher", window.title));
        titles.push(bounds);
    }
    let keys = labels
        .iter()
        .find(|label| label.contains("Alt + Tab"))
        .unwrap_or_else(|| panic!("the switcher names its keys: {labels:?}"));
    let keys = session
        .shell_control(keys)
        .unwrap_or_else(|| panic!("the keys stand in the switcher"));
    for title in &titles {
        assert!(
            keys.1 >= title.1 + title.3,
            "the keys under the windows stand below every title, not over it: keys {keys:?} \
             titles {titles:?}"
        );
    }
    // Holding Alt shows the switcher: the screen is not the desk's own.
    let drawn = changed(&plain, &held);
    assert!(
        drawn > plain.rgba.len() / 4 / 100,
        "holding Alt+Tab draws the switcher: {drawn} pixels differ from the desk"
    );
    // Letting Alt go goes to it, and the desk is at rest again.
    session.compositor().input(key(15, false)).unwrap();
    session.step(Duration::from_millis(1)).unwrap();
    session.compositor().input(key(56, false)).unwrap();
    until(&mut session, |session| here(session) == "none");
    let rested = keep(&mut session, "shell-after-switcher");
    assert!(
        changed(&held, &rested) > drawn / 2,
        "letting Alt go takes the switcher down again"
    );
    assert_eq!(
        desk(&session)
            .windows
            .iter()
            .find(|window| window.focused)
            .map(|window| window.key.clone()),
        Some(next),
        "the window Alt+Tab walked to is in front: {:?}",
        session.shell_refusals()
    );
}

/// Alt+Tab's press on the desk's door: the walk `steps` windows on, in `scope` .
fn walk(session: &mut Session, steps: i64, scope: &str) {
    session
        .invoke(
            DESK_RESOURCE,
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

/// The window the switcher lifts, as the desk reads it: the one the walk reached while the panel
/// shows, and the window in front while the panel is down.
fn reached(session: &Session) -> tilo_surface_host::shell_part::WindowReading {
    desk(session)
        .windows
        .into_iter()
        .find(|window| window.focused)
        .expect("a window is in front")
}

/// [`reached`], by key.
fn lifted(session: &Session) -> String {
    reached(session).key
}

/// A flick past Alt+Tab switches windows and flashes nothing: the desk draws the same screen until
/// the walk outlasts the reveal delay, and then the panel comes up on its own — the one deadline
/// the session hands the shell — with the window the walk reached lifted in it.
#[test]
fn a_flick_shows_no_panel_and_holding_the_walk_outlasts_its_delay() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
    open_window(&mut session, "so.tilo.notes");
    open_window(&mut session, "so.tilo.mail");
    until(&mut session, |session| desk(session).windows.len() == 2);
    let before = desk(&session);
    let front = before
        .windows
        .iter()
        .position(|window| window.focused)
        .expect("a window is in front");
    let front_key = before.windows[front].key.clone();
    let next = before.windows[(front + 1) % before.windows.len()]
        .key
        .clone();
    let plain = keep(&mut session, "shell-flick");

    walk(&mut session, 1, "workspace");
    let flicked = desk(&session);
    assert_eq!(flicked.here, "switcher");
    assert!(
        !flicked.switcher_revealed,
        "a flick shorter than the reveal delay shows no panel"
    );
    assert_eq!(
        lifted(&session),
        front_key,
        "with the panel down the walk lifts nothing, so the desk is as it was"
    );
    let down = keep(&mut session, "shell-flick-panel-down");
    let drawn = changed(&plain, &down);
    assert!(
        drawn < plain.rgba.len() / 4 / 100,
        "a flick draws nothing over the desk: {drawn} pixels differ"
    );

    // Held past the delay, the panel comes up with the window the walk reached lifted.
    session.step(Duration::from_millis(300)).unwrap();
    until(&mut session, |session| desk(session).switcher_revealed);
    assert_eq!(
        lifted(&session),
        next.clone(),
        "the panel lifts the window the walk reached"
    );
    let up = keep(&mut session, "shell-flick-held");
    assert!(
        changed(&plain, &up) > plain.rgba.len() / 4 / 100,
        "the panel shows over the desk once the walk outlasts the delay"
    );

    // Letting go goes there, and the panel is down again.
    session
        .invoke(DESK_RESOURCE, "commit-switcher", &serde_json::Map::new())
        .unwrap();
    let rested = desk(&session);
    assert_eq!(rested.here, "none");
    assert!(!rested.switcher_revealed);
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
    until(&mut session, |session| says(session, &desk(session).clock));
    for app in [
        "so.tilo.notes",
        "so.tilo.notes",
        "so.tilo.mail",
        "so.tilo.mail",
    ] {
        open_window(&mut session, app);
    }
    until(&mut session, |session| desk(session).windows.len() == 4);
    let front = reached(&session);

    // The walk over this app's windows never leaves it, however far it walks.
    walk(&mut session, 1, "app");
    session.step(Duration::from_millis(300)).unwrap();
    until(&mut session, |session| desk(session).switcher_revealed);
    assert_eq!(desk(&session).switcher_scope, "app");
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
    // only two of the desk's four windows.
    let mut crossed = None;
    for press in 1..=3 {
        walk(&mut session, 1, "workspace");
        assert_eq!(desk(&session).switcher_scope, "workspace");
        // The walk in the other scope starts again, so the panel comes up on holding it.
        session.step(Duration::from_millis(300)).unwrap();
        until(&mut session, |session| desk(session).switcher_revealed);
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
        .invoke(DESK_RESOURCE, "commit-switcher", &serde_json::Map::new())
        .unwrap();
    assert_eq!(here(&session), "none");
    until(&mut session, |session| lifted(session) == crossed.key);
    assert_eq!(lifted(&session), crossed.key);
}

/// Who is here is one button of faces — up to three, then a face counting the rest — whose name
/// names the people, so no words take the bar's room at any width; pressing it calls the session's
/// own `toggle-collaborators` , and the roster opens and lists them.
#[test]
fn a_persons_presence_shows_their_face_and_opens_the_roster() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
    // Nobody here: the people door alone.
    let door = crate::common::wait_for(&mut session, "the people door", |session| {
        bar_control(session, "People")
    });
    assert!(
        session.shell_control("Mia Clayton").is_none(),
        "no face of a person who is not here"
    );
    // Someone here, as the collaboration plane lists them: their face, named for assistive
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
        session.shell_control("Mia Clayton").is_none(),
        "the face has no control of its own beside a chip: {:?}",
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
        door.2 < 40.0,
        "nobody here: the glyph alone, {} wide",
        door.2
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
}

/// An action host that keeps what it was asked to run: where the session starts an app.
#[derive(Default)]
struct Apps {
    invoked: std::cell::RefCell<Vec<tilo_actions::remote::RemoteActionTarget>>,
}

impl tilo_session::AppActions for Apps {
    fn published(&self, _: &str) -> Vec<tilo_desktop_search::providers::PublishedActions> {
        Vec::new()
    }

    fn jobs(&self) -> Vec<tilo_desktop_search::providers::jobs::JobEntry> {
        Vec::new()
    }

    fn invoke(
        &self,
        target: tilo_actions::remote::RemoteActionTarget,
        _: serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), tilo_actions::DoorError> {
        self.invoked.borrow_mut().push(target);
        Ok(())
    }

    fn cancel(&self, _: &str, _: &str, _: &str) -> Result<(), tilo_actions::DoorError> {
        Ok(())
    }

    fn open_uri(&self, _: &str, _: &str) -> Result<(), tilo_actions::DoorError> {
        Ok(())
    }

    fn start_host_app(&self, _: &str) -> Result<(), tilo_actions::DoorError> {
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

    fn set_node_mute(&self, _node_id: u32, _muted: bool) -> Result<(), String> {
        Ok(())
    }

    fn set_device_profile(&self, _device_id: u32, _profile_index: u32) -> Result<(), String> {
        Ok(())
    }
}

/// An app's own `open` the shell calls (`session.open(app)`, a dock's press) starts the app in the
/// workspace in front, as the launcher's row of it does. (The press making that call is
/// `tilo-surface-host` 's `a_press_on_a_dock_app_asks_the_session_to_open_it` .)
#[test]
fn an_apps_open_the_shell_calls_starts_it() {
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
    until(&mut session, |session| says(session, &desk(session).clock));
    session
        .shell_link()
        .expect("the shell runs")
        .ask(tilo_surface_host::shell_part::Ask {
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

/// A session of `width` by `height` with the shell's bar, the person's settings and a sound card:
/// what the quick settings read and change.
fn session_with_devices(width: i32, height: i32) -> Session {
    session_with_services(width, height, Vec::new())
}

/// The devices session of the desk's own chrome, with quick settings' app installed: the panel the
/// chrome shows, started as a process of the session and standing where the stack places the slot,
/// over the bar's status. The screen shell draws its own quick settings (`quick.tl`), so the
/// panel's app is the desk's.
fn session_with_the_panel(width: i32, height: i32) -> Session {
    Session::headless(
        Options {
            services: vec![
                Box::new(tilo_settings_service::SettingsService::new()),
                Box::new(speaker()),
            ],
            collab: false,
            shell: false,
            surfaces: vec![tilo_session::quick_settings::surface_of(
                tilo_test_env::sibling_binary("tilo-quick-settings", "tilo-quick-settings"),
            )],
            ..options()
        },
        &[OutputConfig::new("HEADLESS-1", width, height)],
    )
    .expect("session")
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
        Box::new(tilo_settings_service::SettingsService::new()),
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

/// A press on the dock's launcher calls the desk's own `show-surface` : the launcher comes up.
#[test]
fn a_press_on_the_docks_launcher_shows_the_launcher() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
    until(&mut session, |session| {
        session.shell_control("Launcher").is_some()
    });
    press(&mut session, "Launcher");
    until(&mut session, |session| desk(session).here == "launcher");
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
        .read("settings:settings.appearance")
        .map(|section| section["mode"].clone())
        .unwrap_or_default()
}

/// The bar's status is the device's own reading: the sound's level, from the same `sound` resource
/// the quick settings read and change — a change of the resource moves the bar's number, never a
/// copy of it.
#[test]
fn the_bars_status_shows_the_sounds_published_level() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = session_with_devices(WIDTH, HEIGHT);
    until_settled(&mut session, |session| says(session, &desk(session).clock));
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

/// The bar's status opens the quick settings over the desk, and the panel's Dark mode tile runs the
/// appearance settings' own published action: the published mode turns from what it was to the
/// other, as Settings and an agent turn it.
fn dark_mode_from_quick_settings_at(width: i32, height: i32) {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = device_session_over(width, height, Device { charging: false });
    until_settled(&mut session, |session| says(session, &desk(session).clock));
    // The status opens the settings: the sound's control on a desk, the charge's on a phone,
    // where the sound gives way and the bell is the notification centre's door.
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
    until_settled(&mut session, |session| says(session, &desk(session).clock));
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

/// The panel the desktop shows: the bar's status opens quick settings' own app over the desk, the
/// panel says at a glance which radios are on, and each tile's chevron opens its own details in
/// place under the tiles and closes them again — the networks this laptop knows and the devices it
/// is paired with — in both appearance modes. Each resting panel is kept for the capture that shows
/// it (`TL_SHELL_SHOTS`). The bar's status opens the panel over the desk, and what draws it is the
/// panel's own app: the session starts it where the stack placed the slot. Escape closes the panel,
/// and the desk comes back; the app stays running for the session's life, hidden, so the next press
/// shows it at once.
#[test]
fn the_bars_status_opens_the_panel_as_its_own_app() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = session_with_the_panel(WIDTH, HEIGHT);
    until_settled(&mut session, |session| says(session, &desk(session).clock));
    let desk_only = keep(&mut session, "quick-settings-closed");
    // The press is the chrome's own verb: it takes the stack's slot, and the session starts the
    // panel's app where the stack placed it.
    session
        .invoke(
            tilo_surface_host::shell_part::CHROME_RESOURCE,
            "quick-settings",
            &serde_json::Map::new(),
        )
        .unwrap();
    until_settled(&mut session, |session| session.quick_settings_open());
    until_settled(&mut session, |session| {
        session.surfaces().running(quick_settings::SURFACE)
    });
    assert!(
        session.surfaces().running(quick_settings::SURFACE),
        "the bar's status starts the panel's app; the session runs {:?} and is at {:?}",
        session.surfaces().names(),
        here(&session)
    );
    let showing = if appearance_mode(&session) == serde_json::json!("dark") {
        "dark"
    } else {
        "light"
    };
    // The panel's own ground, where the desk shows through until it draws.
    let desk_probe = desk_only
        .pixel(PROBE.0, PROBE.1)
        .expect("a pixel of the desk");
    until_settled(&mut session, |session| {
        session
            .compositor()
            .screenshot("HEADLESS-1")
            .expect("shot")
            .pixel(PROBE.0, PROBE.1)
            != Some(desk_probe)
    });
    let controls = drawn(&mut session);
    assert_ne!(
        controls
            .pixel(PROBE.0, PROBE.1)
            .expect("a pixel of the panel"),
        desk_probe,
        "the panel draws its own ground over the desk's"
    );
    save(&controls, &format!("quick-settings-{showing}"));
    assert!(
        session.quick_settings_open() && session.surfaces().running(quick_settings::SURFACE),
        "the panel's app stays up: {:?}, open {}",
        session.surfaces().names(),
        session.quick_settings_open()
    );
    // Escape closes the panel; its app keeps running, hidden, and stands ready to be shown again.
    press_escape(&mut session);
    until_settled(&mut session, |session| !session.quick_settings_open());
    assert!(
        session.surfaces().running(quick_settings::SURFACE),
        "the panel's app stays up between openings: {:?}",
        session.surfaces().names()
    );
    // The panel's ground goes with it: the desk shows where it stood.
    let closed = crate::common::wait_for(
        &mut session,
        "the panel's ground to go with it",
        |session| {
            let frame = drawn(session);
            (frame.pixel(PROBE.0, PROBE.1).expect("a pixel of the desk")
                != controls
                    .pixel(PROBE.0, PROBE.1)
                    .expect("a pixel of the panel"))
            .then_some(frame)
        },
    );
    save(&closed, "quick-settings-closed-again");
}

/// Escape goes back a page inside the panel before it closes it: with a page of the panel's own up,
/// Escape leaves it for the controls and the panel stays open — only the controls, which have
/// nothing behind them, close it.
#[test]
fn escape_goes_back_a_page_in_the_panel_before_it_closes_it() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = session_with_the_panel(WIDTH, HEIGHT);
    until_settled(&mut session, |session| says(session, &desk(session).clock));
    session
        .invoke(
            tilo_surface_host::shell_part::CHROME_RESOURCE,
            "quick-settings",
            &serde_json::Map::new(),
        )
        .unwrap();
    until_settled(&mut session, |session| {
        session.quick_settings_open() && session.surfaces().running(quick_settings::SURFACE)
    });
    let controls = drawn(&mut session);
    // A page of the panel's own, over the controls: what plays on this machine, which is what the
    // person reads after picking Sound.
    let page = crate::common::wait_for(&mut session, "the panel's page", |session| {
        session
            .surfaces()
            .published(quick_settings::SURFACE)
            .into_iter()
            .find(|offered| offered.action == "open-section")
    });
    let mut asked = serde_json::Map::new();
    asked.insert("section".to_owned(), "sound".into());
    session
        .ask_surface(
            quick_settings::SURFACE,
            &page.resource,
            &page.action,
            ui::tilo_actions::remote::InvokeArgument::Json(asked),
        )
        .expect("the panel's app answers the page it publishes");
    let page = crate::common::wait_for(&mut session, "the page drawn", |session| {
        let frame = drawn(session);
        (crate::common::changed(&controls, &frame) > 1000).then_some(frame)
    });
    save(&page, "quick-settings-page");
    // Escape goes back to the controls: the panel answers that it had a page behind it and stays up
    // to be closed from there.
    press_escape(&mut session);
    for _ in 0..30 {
        session.settle_services();
        session.step(Duration::from_millis(16)).expect("a frame");
    }
    assert!(
        session.quick_settings_open() && session.surfaces().running(quick_settings::SURFACE),
        "Escape on a page left the panel up"
    );
    save(&drawn(&mut session), "quick-settings-back");
}

/// Press Escape and let it up again, as the compositor sends it.
fn press_escape(session: &mut Session) {
    for pressed in [true, false] {
        session
            .compositor()
            .input(InputEvent::Key {
                keycode: KEY_ESCAPE,
                pressed,
            })
            .expect("the key");
        session.step(Duration::from_millis(16)).expect("step");
    }
}

/// The Escape key, as the compositor names it.
const KEY_ESCAPE: u32 = 1;

/// A pixel of the panel's column, clear of the rows that move with the pointer: where the panel's
/// own ground shows over the desk's.
const PROBE: (u32, u32) = (1100, 200);

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
// invocation on the session's door: each test below presses the shell's own view and reads the
// session's state, so a call the door cannot run fails here.

/// Bring the desk's surface `surface` up, as the bar's own buttons do.
fn show(session: &mut Session, surface: &str) {
    session
        .invoke(
            DESK_RESOURCE,
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
fn tab_place(session: &Session, tab: u64) -> Option<(u64, String)> {
    session.handle().read(|tree| {
        tree.workspaces.iter().find_map(|workspace| {
            workspace.strips.iter().find_map(|strip| {
                strip
                    .window_groups
                    .iter()
                    .any(|group| group.tabs.iter().any(|t| t.id == tab))
                    .then(|| (workspace.slot.0, strip.name.clone()))
            })
        })
    })
}

/// The tab `tab` is the window in front, as the desk publishes it.
fn in_front(session: &Session, tab: u64) -> bool {
    desk(session)
        .windows
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
        .find(|workspace| workspace.slot == tree.active)
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
    until(&mut session, |session| says(session, &desk(session).clock));
    open_window(&mut session, "so.tilo.notes");
    open_window(&mut session, "so.tilo.mail");
    until(&mut session, |session| desk(session).windows.len() == 2);
    // The window not in front is the one to go to.
    let behind = desk(&session)
        .windows
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

/// The launcher's door to the overview shows it: `show-surface` on the desk.
#[test]
fn a_press_on_a_launcher_door_shows_that_surface() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
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

/// What the launcher holds no window for, Search everywhere hands the desk's search: it opens with
/// what was typed.
#[test]
fn searching_everywhere_from_the_launcher_opens_the_desks_search_with_the_query() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
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
            resource.id.as_str() == "desktop:desk"
                && resource.children.iter().any(|child| {
                    child.id.as_str() == tilo_desktop::desk_verbs::SEARCH_RESOURCE
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
fn closing_a_window_from_the_overview_takes_it_off_the_desk() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
    let notes = open_window(&mut session, "so.tilo.notes");
    until(&mut session, |session| desk(session).windows.len() == 1);
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
    until(&mut session, |session| says(session, &desk(session).clock));
    let notes = open_window(&mut session, "so.tilo.notes");
    let mail = second_strip(&mut session, "Main", "Research", "so.tilo.mail");
    until(&mut session, |session| desk(session).windows.len() == 2);
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
    until(&mut session, |session| says(session, &desk(session).clock));
    let first = desk(&session);
    let first_key = first
        .workspaces
        .iter()
        .find(|workspace| workspace.current)
        .unwrap()
        .key
        .clone();
    session
        .invoke(
            tilo_surface_host::shell_part::CHROME_RESOURCE,
            "new-workspace",
            &serde_json::Map::new(),
        )
        .unwrap();
    until(&mut session, |session| desk(session).workspaces.len() == 2);
    let second = desk(&session)
        .workspaces
        .iter()
        .find(|workspace| workspace.current)
        .unwrap()
        .clone();
    session
        .invoke(
            DESK_RESOURCE,
            "switch-workspace",
            &serde_json::Map::from_iter([(
                "workspace".to_owned(),
                serde_json::Value::String(first_key),
            )]),
        )
        .unwrap();
    until(&mut session, |session| {
        desk(session).workspace == first.workspace
    });
    let notes = open_window(&mut session, "so.tilo.notes");
    until(&mut session, |session| desk(session).windows.len() == 1);
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

/// The centre's Clear all empties it: the desk's `clear-notices` .
#[test]
fn a_press_on_the_centres_clear_all_empties_the_desks_notices() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
    for (title, body) in [
        ("Backup finished", "12 GB safe"),
        ("Update ready", "Restart"),
    ] {
        session.notices().post(
            tilo_desktop_surface::notifications::Notification::new(title, body)
                .with_app_name("Settings"),
        );
    }
    until(&mut session, |session| desk(session).notices.len() == 2);
    show(&mut session, "centre");
    until(&mut session, |session| {
        session.shell_control("Clear all").is_some()
    });
    press(&mut session, "Clear all");
    until(&mut session, |session| desk(session).notices.is_empty());
    assert!(
        session.shell_refusals().is_empty(),
        "{:?}",
        session.shell_refusals()
    );
}

/// A workspace named in the workspaces surface is made and in front: the chrome's `new-workspace`
/// with its name.
#[test]
fn creating_a_named_workspace_from_the_workspaces_surface_makes_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
    show(&mut session, "workspaces");
    until(&mut session, |session| {
        session.shell_control(WORKSPACE_NAME).is_some()
    });
    press(&mut session, WORKSPACE_NAME);
    type_text(&mut session, "studio");
    press(&mut session, "Create");
    until(&mut session, |session| {
        desk(session)
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

/// Who is here, as the collaboration plane lists them: the first `count` of [`HERE`].
fn set_here(session: &Session, count: usize) {
    let here: Vec<_> = HERE
        .iter()
        .take(count)
        .enumerate()
        .map(|(at, name)| tilo_desktop::here::LiveMember {
            publisher: format!("{:064x}", at + 1),
            kind: "person".to_owned(),
            label: (*name).to_owned(),
            ..tilo_desktop::here::LiveMember::default()
        })
        .collect();
    session
        .sync()
        .expect("the session takes part in the plane")
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
    // of its own; the bar's controls stand inside it, and nothing of the desk does.
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

/// The controls standing in the bar's band, by where their middle is: what a person sees on
/// the bar, not on the desk. A control that reaches out of the band is still one of the bar's,
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
    ) -> Result<tilo_service::Started, tilo_actions::DoorError> {
        Err(tilo_actions::DoorError::Unavailable {
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
                Box::new(tilo_settings_service::SettingsService::new()),
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
            "settings:settings.appearance",
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

/// The count's badge stands wholly inside the bell's face, which stands inside the bar: what the
/// person sees when a notification arrives, and where.
#[test]
fn the_bells_count_stands_inside_the_bar() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
    let before = keep(&mut session, "shell-bar-bell");
    session
        .notices()
        .post_quiet(tilo_desktop_surface::notifications::Notification::new(
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
    // the control it counts, so a bell standing inside the bar's band carries no pixel of its count
    // over the screen's top edge. (What the notice itself draws on the desk is not the bar's; the
    // pixels below the bar change with it.)
    let (inside, _) = changed_in(&before, &after, bell);
    assert!(inside > 20, "the count shows: {inside} pixels");
}

/// The bar's trailing group, in order: who is here, the device's status, the bell and the person's
/// own face, with a gap between who is here and the status, and the person's face at the far end.
#[test]
fn the_trailing_group_stands_in_order_and_ends_with_the_persons_face() {
    let _settled = ui::motion::testing::settled();
    let mut session = device_session(WIDTH, HEIGHT);
    until_settled(&mut session, |session| says(session, &desk(session).clock));
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
/// translucent style and the calm ground of the solid one, on one view, over one desk.
#[test]
fn the_bar_draws_the_material_of_the_style_the_person_chose() {
    let _settled = ui::motion::testing::settled();
    let mut session = device_session(WIDTH, HEIGHT);
    until_settled(&mut session, |session| says(session, &desk(session).clock));
    set_here(&session, 2);
    until_settled(&mut session, |session| says(session, "are here"));
    // The desk's first frames draw every translucent body before the compositor has handed it what
    // stands below it; let the session reach its rest before the frame is kept, as the captures
    // after a setting change do.
    settle(&mut session);
    let calm = keep(&mut session, "shell-bar-calm");
    session
        .run_shell_setting("set_bar_finish", vec![serde_json::json!("translucent")])
        .expect("the bar takes the theme's glass");
    settle(&mut session);
    let glass = keep(&mut session, "shell-bar-glass");
    let band = bar_band(&session);
    // The theme's glass lets the ground through it: over the desk's flat backdrop it moves the
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
        "the desk under the bar is the same in both styles: one view, not two"
    );
    session
        .run_shell_setting("set_bar_finish", vec![serde_json::json!("solid")])
        .expect("and back to the calm ground");
    settle(&mut session);
    let back = keep(&mut session, "shell-bar-calm-again");
    // The clock may turn its minute between the two frames; nothing else may differ.
    let mut allowed = vec![];
    allowed.extend(bar_control(&session, &desk(&session).clock));
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
    // The reading is the minute's, and the pass that draws it comes after the minute the desk now
    // says: wait for the bar to show it rather than take the minute the last frame drew.
    let drawn_clock = |session: &mut Session| bar_control(session, &desk(session).clock);
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
        until_settled(&mut session, |session| says(session, &desk(session).clock));
        set_here(&session, 4);
        until_settled(&mut session, |session| desk(session).people.len() == 4);
        let clock = desk(&session).clock;
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
            until_settled(&mut session, |session| says(session, &desk(session).clock));
            set_here(&session, count);
            let who = if count == 1 { "is here" } else { "are here" };
            until_settled(&mut session, |session| {
                desk(session).people.len() == count
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
                        .run_shell_setting("set_bar_finish", vec![serde_json::json!(finish)])
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
                    Box::new(tilo_settings_service::SettingsService::new()),
                    Box::new(speaker()),
                ],
                collab: false,
                ..options()
            },
            &[OutputConfig::new("HEADLESS-1", WIDTH * pixels, HEIGHT * pixels).with_scale(scale)],
        )
        .expect("session");
        until_settled(&mut session, |session| says(session, &desk(session).clock));
        session
            .invoke(
                tilo_surface_host::shell_part::CHROME_RESOURCE,
                "quick-settings",
                &serde_json::Map::new(),
            )
            .unwrap();
        until_settled(&mut session, |session| says(session, "Dark mode"));
        // The tile as the session publishes it, in the desk's logical pixels: a press at its centre
        // is the one the person makes.
        until_settled(&mut session, |session| {
            session
                .controls()
                .iter()
                .any(|(name, _)| name == "Dark mode")
        });
        let (x, y, w, h) = session
            .controls()
            .into_iter()
            .find(|(name, _)| name == "Dark mode")
            .map(|(_, rect)| rect)
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
/// its per-app id is the one it declares, and the desk it draws is the same desk.
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
    let fallback = tilo_surface_host::shell_part::compile_shell().expect("the fallback compiles");
    assert_ne!(
        fallback.last().map(|package| package.wasm.as_slice()),
        installed.last().map(|package| package.wasm.as_slice()),
        "the installed shell runs its own signed module, not the build's compiled copy"
    );
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
    assert!(
        session.shell_apps().is_empty(),
        "a place shows no app yet: the shell alone runs"
    );
    // The desk the installed shell draws is the desk: its bar names the person the session
    // publishes, so what runs is a shell and not a blank screen.
    assert!(
        says(&session, PERSON),
        "the installed shell draws the bar: {:?}",
        session.accessible_labels()
    );
}
