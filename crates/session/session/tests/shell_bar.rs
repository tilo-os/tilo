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
    reason = "test fixtures: a failed step is the assertion"
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
    until(&mut session, |session| {
        desk(session).unread == 1 && says(session, "1")
    });
    keep(&mut session, "shell-bar-notification");
}
