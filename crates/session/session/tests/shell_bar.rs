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
use tilo_test_env::SpawnUnderEnvLock as _;

use tilo_compositor::{InputEvent, KeyRepeat, OutputConfig};
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
        shell: Some(tilo_surface_host::shell_part::ShellLayer::Bar),
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
        "condition not reached; the shell's refusals: {:?}; effects handed {:?} served {:?}; the session says: {:?}",
        session.shell_refusals(),
        session.shell_work().map(|work| work.handed),
        session.shell_work().map(|work| work.served),
        session.accessible_labels()
    );
}

/// Whether the bar says `words`: a control's label carries what it shows and
/// what it opens ("Show strips, now on Main", "Pat (you): your profile"), so
/// naming the words is what the bar does.
fn says(session: &Session, words: &str) -> bool {
    session
        .accessible_labels()
        .iter()
        .any(|label| label.contains(words))
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

/// How many pixels of `after` differ from `before` by more than a shade: what a person sees
/// change on the screen.
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

/// Alt+Tab's press on the desk's door: the walk `steps` windows on, in `scope`.
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

/// The window the switcher lifts, as the desk reads it: the one the walk reached while the
/// panel shows, and the window in front while the panel is down.
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

/// A flick past Alt+Tab switches windows and flashes nothing: the desk draws the same screen
/// until the walk outlasts the reveal delay, and then the panel comes up on its own — the one
/// deadline the session hands the shell — with the window the walk reached lifted in it.
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

    // The workspace's walk crosses into the other app, within three presses: the app in front
    // holds only two of the desk's four windows.
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

/// Who else is here stands on the bar as their face and as the chip that names them, both read
/// from the session's presence; the chip calls the session's own `toggle-collaborators`: the
/// roster opens and lists them.
#[test]
fn a_persons_presence_shows_their_face_and_opens_the_roster() {
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
    until(&mut session, |session| says(session, "Mia Clayton is here"));
    let shown = |session: &Session| {
        session
            .accessible_labels()
            .iter()
            .filter(|label| label.as_str() == "Mia Clayton")
            .count()
    };
    assert!(
        shown(&session) > 0,
        "their face is on the bar, named for assistive technology: {:?}",
        session.accessible_labels()
    );
    let before = shown(&session);
    press(&mut session, "Mia Clayton is here");
    until(&mut session, |session| shown(session) > before);
    keep(&mut session, "shell-people-roster");
}

/// An action host that keeps what it was asked to run: where the session starts an app.
#[derive(Default)]
struct Apps {
    invoked: std::cell::RefCell<Vec<tilo_actions::remote::RemoteActionTarget>>,
}

impl tilo_session::AppActions for Apps {
    fn published(&self) -> Vec<tilo_desktop_search::providers::PublishedActions> {
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

/// An app's own `open` the shell calls (`session.open(app)`, a dock's press) starts the app
/// in the workspace in front, as the launcher's row of it does. (The press making that call is
/// `tilo-surface-host`'s `a_press_on_a_dock_app_asks_the_session_to_open_it`.)
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

/// A session of `width` by `height` with the shell's bar, the person's settings and a sound
/// card: what the quick settings read and change.
fn session_with_devices(width: i32, height: i32) -> Session {
    session_with_services(width, height, Vec::new())
}

/// The connectivity service as a session reaches it: a Wi-Fi radio joined to
/// Home, Bluetooth with a paired headset, and one VPN profile — the devices a
/// laptop has, so the panel's controls and their details are worked as a
/// person works them.
struct Radio;

fn radio_network() -> tilo_connectivity_service::model::Network {
    use tilo_connectivity_service::model::{Link, LinkKind, Network, WifiNetwork};
    let mut network = Network::default();
    network.wifi.available = true;
    network.wifi.on = true;
    network.wifi.networks = vec![
        WifiNetwork {
            name: "Home".into(),
            signal: 4,
            secure: true,
            known: true,
            connected: true,
        },
        WifiNetwork {
            name: "Office".into(),
            signal: 3,
            known: true,
            ..WifiNetwork::default()
        },
        WifiNetwork {
            name: "Cafe guest".into(),
            signal: 2,
            ..WifiNetwork::default()
        },
    ];
    network.link = Some(Link {
        kind: LinkKind::Wifi,
        name: Some("Home".into()),
        signal: 4,
    });
    network
}

fn radio_bluetooth() -> tilo_connectivity_service::model::Bluetooth {
    use tilo_connectivity_service::model::{Accessory, Bluetooth};
    Bluetooth {
        available: true,
        on: true,
        accessories: vec![Accessory {
            name: "Studio headphones".into(),
            address: "00:1B:66:A1:B2:C3".into(),
            battery: Some(80),
            connected: true,
        }],
        ..Bluetooth::default()
    }
}

fn radio_vpn() -> tilo_connectivity_service::model::Vpn {
    use tilo_connectivity_service::model::{Vpn, VpnProfile};
    Vpn {
        profiles: vec![VpnProfile {
            name: "Work".into(),
            id: "7c1e3a5e-0d61-4f3c-9b1a-2d6f9f0e1a11".into(),
            kind: "wireguard".into(),
            connected: false,
        }],
        ..Vpn::default()
    }
}

impl tilo_session::Service for Radio {
    fn resources(&self) -> Vec<tilo_actions::ResourceSpec> {
        vec![
            tilo_connectivity_service::network_resource(&radio_network()),
            tilo_connectivity_service::bluetooth_resource(&radio_bluetooth()),
            tilo_connectivity_service::vpn_resource(&radio_vpn()),
        ]
    }

    fn read(&self, resource: &str) -> Option<serde_json::Value> {
        match resource {
            tilo_connectivity_service::NETWORK => serde_json::to_value(radio_network()).ok(),
            tilo_connectivity_service::BLUETOOTH => serde_json::to_value(radio_bluetooth()).ok(),
            tilo_connectivity_service::VPN => serde_json::to_value(radio_vpn()).ok(),
            _ => None,
        }
    }

    fn invoke(
        &mut self,
        _: &str,
        _: &str,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<tilo_service::Started, tilo_actions::DoorError> {
        Ok(tilo_service::Started::Accepted)
    }
}

/// The devices session, on a machine with a radio too.
fn session_with_radio(width: i32, height: i32) -> Session {
    session_with_services(width, height, vec![Box::new(Radio)])
}

/// A session serving the settings and the sound, and `extra` beside them.
fn session_with_services(
    width: i32,
    height: i32,
    extra: Vec<Box<dyn tilo_session::Service>>,
) -> Session {
    use tilo_session::Service as _;
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
    let mut services: Vec<Box<dyn tilo_session::Service>> = vec![
        Box::new(tilo_settings_service::SettingsService::new()),
        Box::new(sound),
    ];
    services.extend(extra);
    Session::headless(
        Options {
            services,
            collab: false,
            ..options()
        },
        &[OutputConfig::new("HEADLESS-1", width, height)],
    )
    .expect("session")
}

/// Run frames, taking in what the services report, until `done` holds.
#[track_caller]
fn until_settled(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) {
    for _ in 0..2000 {
        session.settle_services();
        session.step(Duration::from_millis(1)).expect("step");
        if done(session) {
            return;
        }
    }
    panic!(
        "condition not reached; the session says: {:?}; the shell refused: {:?}",
        session.accessible_labels(),
        session.shell_refusals()
    );
}

/// A press on the dock's launcher calls the desk's own `show-surface`: the launcher comes up.
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

/// Press the control named `name` once the bar stands still: what the services report while
/// the session starts (the workspace, who is here) adds controls beside it and moves it, and a
/// person presses the control where they see it at rest.
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

/// The bar's status is the device's own reading: the sound's level, from the same `sound`
/// resource the quick settings read and change — a change of the resource moves the bar's
/// number, never a copy of it.
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

/// The bar's status opens the quick settings over the desk, and the panel's Dark mode tile runs
/// the appearance settings' own published action: the published mode turns from what it was to
/// the other, as Settings and an agent turn it.
fn dark_mode_from_quick_settings_at(width: i32, height: i32) {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = session_with_devices(width, height);
    until_settled(&mut session, |session| says(session, &desk(session).clock));
    // A phone's bar keeps the connection and the charge, the sound's control giving way
    // (`status_icons`): there the bell is the door to the device's settings.
    let opener = if width < 480 {
        "Notifications"
    } else {
        "Volume, 40%"
    };
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

/// The volume slider shows the sound's published volume, and pressing its track runs the
/// sound's own `set-volume`: the published volume follows.
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

/// The panel says at a glance which radios are on, and each tile's chevron
/// opens its own details in place under the tiles and closes them again:
/// the networks this laptop knows and the devices it is paired with, in both
/// appearance modes. Each resting panel is kept for the capture that shows
/// it (`TL_SHELL_SHOTS`).
#[test]
fn quick_settings_opens_each_radios_details_in_place() {
    let _home = tilo_test_env::TestHome::new().unwrap();
    let _settled = ui::motion::testing::settled();
    let mut session = session_with_radio(WIDTH, HEIGHT);
    until_settled(&mut session, |session| says(session, &desk(session).clock));
    press_at_rest(&mut session, "Volume, 40%");
    until_settled(&mut session, |session| here(session) == "quick-settings");
    until_settled(&mut session, |session| {
        says(session, "Wi-Fi") && says(session, "Bluetooth") && says(session, "VPN")
    });
    assert!(
        !says(&session, "Office"),
        "nothing is open when the panel opens"
    );
    let dark = serde_json::json!("dark");
    let mode = |session: &Session| {
        if appearance_mode(session) == dark {
            "dark"
        } else {
            "light"
        }
    };
    let showing = mode(&session);
    keep(&mut session, &format!("quick-settings-{showing}"));
    // Wi-Fi's chevron: the networks under the tiles, the one in use first.
    press_at_rest(&mut session, "Wi-Fi networks");
    until_settled(&mut session, |session| says(session, "Office"));
    keep(&mut session, &format!("quick-settings-{showing}-wifi"));
    press_at_rest(&mut session, "Wi-Fi networks");
    until_settled(&mut session, |session| !says(session, "Office"));
    // Bluetooth's: the paired devices under them.
    press_at_rest(&mut session, "Bluetooth devices");
    until_settled(&mut session, |session| says(session, "Studio headphones"));
    keep(&mut session, &format!("quick-settings-{showing}-bluetooth"));
    press_at_rest(&mut session, "Bluetooth devices");
    until_settled(&mut session, |session| !says(session, "Studio headphones"));
    // The panel in the other appearance, with the same details.
    press_at_rest(&mut session, "Dark mode");
    until_settled(&mut session, |session| mode(session) != showing);
    let other = mode(&session);
    keep(&mut session, &format!("quick-settings-{other}"));
    press_at_rest(&mut session, "Wi-Fi networks");
    until_settled(&mut session, |session| says(session, "Office"));
    keep(&mut session, &format!("quick-settings-{other}-wifi"));
    press_at_rest(&mut session, "Bluetooth devices");
    until_settled(&mut session, |session| says(session, "Studio headphones"));
    keep(&mut session, &format!("quick-settings-{other}-bluetooth"));
    assert!(
        session.shell_refusals().is_empty(),
        "the panel's presses reached the radios: {:?}",
        session.shell_refusals()
    );
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

/// Type `text` (lower-case letters) on the keyboard, a key at a time.
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
        for pressed in [true, false] {
            session
                .compositor()
                .input(InputEvent::Key { keycode, pressed })
                .unwrap();
            session.step(Duration::from_millis(1)).unwrap();
        }
    }
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
                    .then(|| (workspace.id.0, strip.name.clone()))
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

/// Name the strip in front `first`, and add a strip `second` beside it holding a window of
/// `app`, its tab's id.
fn second_strip(session: &mut Session, first: &str, second: &str, app: &str) -> u64 {
    let handle = session.handle().clone();
    let mut tree = handle.read(Clone::clone);
    let tab = handle.mint_id();
    let group = handle.mint_id();
    let strip = tilo_desktop_model::StripId(handle.mint_id());
    let workspace = tree
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.id == tree.active)
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

/// What the launcher holds no window for, Search everywhere hands the desk's search: it opens
/// with what was typed.
#[test]
fn searching_everywhere_from_the_launcher_opens_the_desks_search_with_the_query() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
    show(&mut session, "launcher");
    until(&mut session, |session| {
        session.shell_control("Query").is_some()
    });
    press(&mut session, "Query");
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
/// `move-tab-to-strip`.
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

/// A window carried to another workspace from the overview is in it: the chrome's `move-tab`.
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

/// The centre's Clear all empties it: the desk's `clear-notices`.
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

/// A workspace named in the workspaces surface is made and in front: the chrome's
/// `new-workspace` with its name.
#[test]
fn creating_a_named_workspace_from_the_workspaces_surface_makes_it() {
    let _settled = ui::motion::testing::settled();
    let mut session = session();
    until(&mut session, |session| says(session, &desk(session).clock));
    show(&mut session, "workspaces");
    until(&mut session, |session| {
        session.shell_control("Name").is_some()
    });
    press(&mut session, "Name");
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
