//! The tilolang shell's top bar on the native session (headless backend):
//! it shows the desk the session publishes on its door (the time, the
//! workspace and strip in front, the person, the unread notifications),
//! switching workspace from it brings that workspace to the front, and with
//! nothing changing it does no work.

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

use std::collections::HashMap;
use std::io::BufRead as _;
use std::rc::Rc;
use std::time::Duration;

use tilo_compositor::{InputEvent, OutputConfig};
use tilo_session::desktop_services::{Bus, Buses, DesktopServices};
use tilo_session::{Options, Session};
use tilo_surface_host::DeskReading;
use tilo_surface_host::shell_part::DESK_RESOURCE;

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
const BTN_LEFT: u32 = 0x110;
const PERSON: &str = "Pat";

fn options() -> Options {
    Options {
        presentation: ui::headless::fixture_runner_inputs,
        person: PERSON.to_owned(),
        shell_bar: true,
        ..Options::default()
    }
}

fn session() -> Session {
    Session::headless(options(), &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)])
        .expect("session")
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

/// Run frames until `done` holds.
#[track_caller]
fn until(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) {
    for _ in 0..2000 {
        session.step(Duration::from_millis(1)).expect("step");
        if done(session) {
            return;
        }
    }
    panic!(
        "condition not reached; the session says: {:?}",
        session.accessible_labels()
    );
}

fn says(session: &Session, words: &str) -> bool {
    session
        .accessible_labels()
        .iter()
        .any(|label| label == words)
}

/// Keep the screen as `name.png` where `TL_SHELL_SHOTS` names, and its
/// bar's band as `name-bar.png`.
fn keep(session: &mut Session, name: &str) -> tilo_compositor::Screenshot {
    let shot = session.compositor().screenshot("HEADLESS-1").unwrap();
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
    shot
}

/// Press the bar's control named `name` with the pointer.
fn press(session: &mut Session, name: &str) {
    let (x, y, w, h) = session
        .shell_control(name)
        .unwrap_or_else(|| panic!("the bar shows {name}: {:?}", session.accessible_labels()));
    let (x, y) = (x + w / 2.0, y + h / 2.0);
    let compositor = session.compositor();
    compositor
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
}

#[test]
fn the_bar_shows_the_time_the_workspace_its_strip_and_the_person_the_session_publishes() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| {
        let desk = desk(session);
        says(session, &desk.clock) && says(session, &desk.workspace)
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
    let shot = keep(&mut session, "shell-bar-empty-desk");
    // The bar draws in its band: its controls stand on the desk there.
    let band = (0..shot.width)
        .step_by(2)
        .filter_map(|x| shot.pixel(x, 16))
        .collect::<std::collections::BTreeSet<_>>();
    assert!(band.len() > 8, "the bar draws in its band");
    // Its controls stand where the bar's band is.
    let (_, y, _, h) = session.shell_control(&desk.workspace).unwrap();
    assert!(
        y >= 0.0 && y + h <= f64::from(tilo_desktop::topbar::TOPBAR_HEIGHT_PX) + 0.5,
        "the workspace's control stands in the bar's band: {y} {h}"
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
    // The workspace's control opens the workspaces; choosing the first
    // brings it to the front.
    press(&mut session, &second);
    until(&mut session, |session| {
        session.shell_control(&first.workspace).is_some()
    });
    keep(&mut session, "shell-bar-workspace-menu");
    press(&mut session, &first.workspace);
    for _ in 0..300 {
        session.step(Duration::from_millis(1)).unwrap();
        if session.handle().read(|tree| tree.active.0.to_string()) == first_key {
            break;
        }
    }
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

/// The whole session frame for a change the bar shows: a new workspace, the frames the session
/// runs to bring it to the front, at the median. The tool that compares the tilolang bar to the
/// Rust bar holds this number.
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
            .spawn()
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
        desk(session).unread == 1 && says(session, "Notifications, 1 unread")
    });
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
        .find(|workspace| workspace.id == tree.active)
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
    // Picking a window's place goes to it. (Its place may not be on screen until the
    // overlay has been drawn, so the press comes once it is.)
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
    assert!(
        says(&session, "Overview") && says(&session, "Notifications"),
        "the launcher shows the desk's doors: {:?}",
        session.accessible_labels()
    );
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

/// The centre's own Dismiss, pressed in the shell's view: the action the view runs is the
/// desk's `dismiss-notice` on the notice the row is for, so the shell's surfaces and an agent
/// dismiss the same way.
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
    // A notice that arrives is unread: the desk's own count says so, the same number the bar's
    // bell shows.
    until(&mut session, |session| desk(session).unread == 1);
    assert!(!desk(&session).notices[0].read, "a fresh notice is unread");
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
    // The centre opened, so the person saw them: every notice is read, and the desk's count
    // the bell shows is none.
    until(&mut session, |session| {
        desk(session).unread == 0 && desk(session).notices.iter().all(|notice| notice.read)
    });
    // The view draws each notice with its Dismiss; pressing it runs the desk's
    // `dismiss-notice` on that notice, as any action on the desk runs.
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
    keep(&mut session, "shell-switcher");
    // Letting Alt go goes to it, and the desk is at rest again.
    session.compositor().input(key(15, false)).unwrap();
    session.step(Duration::from_millis(1)).unwrap();
    session.compositor().input(key(56, false)).unwrap();
    until(&mut session, |session| here(session) == "none");
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

#[test]
fn a_persons_presence_opens_their_menu() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
    // Someone else here, as the collaboration plane lists them.
    let member = tilo_desktop::here::LiveMember {
        publisher: "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd".to_owned(),
        kind: "person".to_owned(),
        label: "Mia Clayton".to_owned(),
        ..tilo_desktop::here::LiveMember::default()
    };
    session
        .sync()
        .expect("the session takes part in the plane")
        .here()
        .borrow_mut()
        .set_members(None, Some(vec![member]));
    until(&mut session, |session| says(session, "Mia Clayton"));
    // Their presence opens their menu: follow, message, and what they are working on.
    press(&mut session, "Mia Clayton");
    until(&mut session, |session| {
        session.shell_control("Follow").is_some()
    });
    assert!(
        session.shell_control("Message").is_some(),
        "the menu offers a message: {:?}",
        session.accessible_labels()
    );
    assert!(
        session.shell_control("What they are working on").is_some(),
        "the menu offers what they are on: {:?}",
        session.accessible_labels()
    );
    keep(&mut session, "shell-person-menu");
}
