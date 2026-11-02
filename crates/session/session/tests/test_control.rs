//! The development session as `dist/dev/shell-run.sh` runs it: seen through the test-control
//! socket `tilo-control snapshot` writes a PNG over, with the tilolang apps seeded, and ending on
//! SIGTERM as it ends on its own, the files people choose for apps unmounted.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and a session lives for its whole test"
)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tilo_compositor::OutputConfig;
use tilo_service::Service as _;
use tilo_session::{Options, Session};
use tilo_surface_host::shell_part::ShellLayer;
use tilo_test_env::SpawnUnderEnvLock as _;

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;
const PERSON: &str = "Pat";

/// A directory only this user reaches, for the sockets.
fn private_home() -> tempfile::TempDir {
    use std::os::unix::fs::PermissionsExt as _;
    let root = tempfile::tempdir().expect("a directory to serve the session in");
    std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700))
        .expect("the session's directory is this user's");
    root
}

/// Run frames until `done` holds; `false` when it never did.
fn reached(session: &mut Session, mut done: impl FnMut(&mut Session) -> bool) -> bool {
    for _ in 0..600 {
        session.step(Duration::from_millis(1)).expect("step");
        if done(session) {
            return true;
        }
    }
    false
}

/// Send `request` on the control socket at `socket` as `tilo-control` does, running the session
/// until it answers.
fn ask(session: &mut Session, socket: &Path, request: &Value) -> Value {
    let socket = socket.to_path_buf();
    let request = request.to_string();
    let (answer, answered) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut stream = UnixStream::connect(&socket).expect("the test-control socket");
        stream
            .write_all(format!("{request}\n").as_bytes())
            .expect("the socket reads");
        let mut line = String::new();
        BufReader::new(stream)
            .read_line(&mut line)
            .expect("an answer");
        answer
            .send(serde_json::from_str::<Value>(&line).expect("a JSON answer"))
            .unwrap();
    });
    for _ in 0..30_000 {
        if let Ok(answer) = answered.try_recv() {
            return answer;
        }
        session.step(Duration::from_millis(1)).expect("step");
    }
    panic!("the test-control socket never answered");
}

/// A headless session with the shell as the whole screen writes a PNG of it through
/// `tilo-control snapshot`'s request, and the bar stands in it.
#[test]
fn a_snapshot_through_the_test_control_socket_shows_the_shells_bar() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: Some(ShellLayer::Screen),
            collab: false,
            test_control: Some(socket.clone()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    assert!(
        reached(&mut session, |session| session
            .shell_control("Notifications")
            .is_some_and(|(_, _, w, h)| w > 0.0 && h > 0.0)),
        "the bar's controls: {:?}",
        session.accessible_labels()
    );
    let (_, bar_y, _, bar_h) = session.shell_control("Notifications").unwrap();

    let png = root.path().join("shot.png");
    let answer = ask(
        &mut session,
        &socket,
        &json!({"op": "snapshot", "path": png.to_str().unwrap()}),
    );
    assert_eq!(answer, json!({"ok": {"frame": 0}}), "the snapshot is taken");

    let shot = image::open(&png).expect("a PNG").into_rgba8();
    assert_eq!(
        (shot.width(), shot.height()),
        (
            u32::try_from(WIDTH).unwrap(),
            u32::try_from(HEIGHT).unwrap()
        ),
        "the whole output"
    );
    // The bar's band, top to the bottom of its controls: drawn, not one flat colour.
    let band = (bar_y + bar_h).ceil() as u32;
    let first = *shot.get_pixel(0, 0);
    let mut distinct = std::collections::BTreeSet::new();
    for y in 0..band.min(shot.height()) {
        for x in 0..shot.width() {
            let pixel = shot.get_pixel(x, y);
            if *pixel != first {
                distinct.insert(pixel.0);
            }
        }
    }
    assert!(
        distinct.len() > 8,
        "the bar is drawn in the snapshot: {} colours besides the corner's {first:?} in its band",
        distinct.len()
    );

    // A click through the socket presses the screen's own control there: the preset switch
    // shows the framed canvas, whose entry stands where the strips stood.
    let (x, y, w, h) = session.shell_control("Frames").expect("the preset switch");
    let clicked = ask(
        &mut session,
        &socket,
        &json!({"op": "click_at", "x": x + w / 2.0, "y": y + h / 2.0}),
    );
    assert_eq!(clicked, json!({"ok": {}}), "the click is taken");
    assert!(
        reached(&mut session, |session| session
            .shell_control("Typed")
            .is_some_and(|(_, _, w, h)| w > 0.0 && h > 0.0)),
        "the canvas: {:?}",
        session.accessible_labels()
    );

    // What the socket does not answer, it says so.
    let refused = ask(
        &mut session,
        &socket,
        &json!({"op": "drag", "x1": 1, "y1": 1, "x2": 2, "y2": 2}),
    );
    assert_eq!(refused["err"]["code"], "unsupported", "{refused}");
}

/// The seeded apps publish notes, tasks, chat and mail, each filled by its first scenario: the
/// notes are found by their title, and the inbox is among what the session publishes.
#[test]
fn the_seeded_apps_publish_their_records_to_the_session() {
    let apps = tilo_session::seeded_apps::seeded(PERSON).expect("the apps load and seed");
    let resources = apps.resources();
    let mut kinds = std::collections::BTreeSet::new();
    fn walk(
        resources: &[tilo_actions::ResourceSpec],
        kinds: &mut std::collections::BTreeSet<String>,
    ) {
        for resource in resources {
            kinds.insert(resource.kind.clone());
            walk(&resource.children, kinds);
        }
    }
    walk(&resources, &mut kinds);
    for kind in [
        "tl.so.tilo.lang.notes.Note",
        "tl.so.tilo.lang.tasks.Task",
        "tl.so.tilo.lang.chat.TextChannel",
        "tl.so.tilo.lang.mail.Mailbox",
    ] {
        assert!(kinds.contains(kind), "{kind} is published: {kinds:?}");
    }
}

/// `tilo-session`, started as `dist/dev/shell-run.sh` starts its first pass: its test-control
/// socket mints the throwaway identity the next session opens its desk with. Ended by SIGTERM,
/// it ends as it does on its own: the process exits cleanly, the files people choose for apps,
/// which it mounted, are unmounted, and its socket is gone.
#[test]
fn a_session_ended_by_sigterm_unmounts_what_it_mounted() {
    let runtime = private_home();
    let home = tempfile::tempdir().unwrap();
    let control = runtime.path().join("test-control.sock");
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_tilo-session"))
        .args([
            "--backend",
            "headless",
            "--no-x11",
            "--session-bus",
            "none",
            "--system-bus",
            "none",
            "--test-control",
            control.to_str().unwrap(),
        ])
        .env("XDG_RUNTIME_DIR", runtime.path())
        .env("TILO_IDENTITY_ROOT", home.path().join("identity"))
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_DATA_HOME", home.path().join(".local/share"))
        .env("XDG_STATE_HOME", home.path().join(".local/state"))
        .env("XDG_CACHE_HOME", home.path().join(".cache"))
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("DISPLAY")
        .env_remove("TILO_AGENT_DOOR")
        .stdout(std::process::Stdio::piped())
        .spawn_locked()
        .expect("tilo-session starts");
    // It is up once it printed its control socket, the last of what it prints.
    let mut out = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    while line.trim_end() != format!("TILO_TEST_CONTROL={}", control.display()) {
        line.clear();
        assert!(
            out.read_line(&mut line).unwrap() > 0,
            "the session ended before it listened: {:?}",
            child.wait()
        );
    }
    let documents = runtime.path().join("tilo").join("documents");
    let mounted = |at: &Path| {
        std::fs::read_to_string("/proc/self/mountinfo")
            .unwrap()
            .lines()
            .any(|entry| entry.split(' ').nth(4) == Some(at.to_str().unwrap()))
    };
    if !mounted(&documents) {
        tilo_test_env::capability_unavailable(
            tilo_test_env::Capability::Fuse,
            format_args!("the session mounted nothing at {}", documents.display()),
        );
    }

    // The throwaway identity, as `tilo-control raw` asks for it.
    let mut stream = UnixStream::connect(&control).expect("the test-control socket");
    stream
        .write_all(b"{\"op\":\"provision_test_identity\"}\n")
        .unwrap();
    let mut minted = String::new();
    BufReader::new(stream).read_line(&mut minted).unwrap();
    let minted: Value = serde_json::from_str(&minted).expect("a JSON answer");
    assert_eq!(minted["answer"]["throwaway"], true, "{minted}");

    let signalled = std::process::Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status_locked()
        .expect("kill runs");
    assert!(signalled.success(), "the session is sent SIGTERM");
    let status = child.wait().unwrap();
    assert!(status.success(), "the session ends cleanly: {status:?}");
    assert!(
        !mounted(&documents),
        "the files people choose for apps are unmounted"
    );
    assert!(!control.exists(), "the test-control socket is removed");
}

/// How many pixels of `shot` in `rect` (logical, at scale 1) are as light as text on the
/// dark theme's ground: what is written there.
fn written(shot: &image::RgbaImage, (x, y, w, h): (f64, f64, f64, f64)) -> usize {
    let mut light = 0;
    for py in (y as u32)..((y + h) as u32).min(shot.height()) {
        for px in (x as u32)..((x + w) as u32).min(shot.width()) {
            let [r, g, b, _] = shot.get_pixel(px, py).0;
            if u32::from(r) + u32::from(g) + u32::from(b) > 3 * 128 {
                light += 1;
            }
        }
    }
    light
}

/// A note of the seeded notes app placed on the framed canvas is seen in the snapshot: its
/// frame's title is drawn where the shell lays it out, not cut off by a canvas with no room.
#[test]
fn a_note_placed_on_the_canvas_is_drawn_in_the_snapshot() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("test-control.sock");
    let apps = tilo_session::seeded_apps::seeded(PERSON).expect("the seeded apps");
    let note = apps
        .resources()
        .iter()
        .flat_map(|resource| resource.children.clone())
        .find(|resource| resource.label == "Groceries")
        .expect("the seeded note")
        .id
        .as_str()
        .to_owned();
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            shell: Some(ShellLayer::Screen),
            collab: false,
            services: vec![Box::new(apps)],
            test_control: Some(socket.clone()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    assert!(reached(&mut session, |session| session
        .shell_control("Frames")
        .is_some()));
    let (x, y, w, h) = session.shell_control("Frames").unwrap();
    ask(
        &mut session,
        &socket,
        &json!({"op": "click_at", "x": x + w / 2.0, "y": y + h / 2.0}),
    );
    assert!(reached(&mut session, |session| session
        .shell_control("Typed")
        .is_some()));

    let (workspace, row) = {
        let tree = session.handle().read(Clone::clone);
        let workspace = tree.active_workspace().expect("a workspace is in front");
        (
            workspace.id.0.to_string(),
            workspace.strips.first().expect("a row").id.0,
        )
    };
    let arguments = json!({
        "workspace": workspace,
        "row": row,
        "strip": session.handle().mint_id(),
        "place": session.handle().mint_id(),
        "resource": note,
        "x": 80,
        "y": 60,
        "width": 480,
        "height": 360,
    });
    session
        .invoke("shell:arrangement", "place", arguments.as_object().unwrap())
        .expect("the note is placed");
    assert!(
        reached(&mut session, |session| session.shell_says("Oat milk")),
        "the note's editor: {:?}",
        session.accessible_labels()
    );
    // As it stands once the frames after it settled, not only on the first.
    for _ in 0..50 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    let title = session
        .shell_control("Groceries")
        .expect("the frame's title");

    let png = root.path().join("shot.png");
    let answer = ask(
        &mut session,
        &socket,
        &json!({"op": "snapshot", "path": png.to_str().unwrap()}),
    );
    assert_eq!(answer, json!({"ok": {"frame": 0}}), "the snapshot is taken");
    let shot = image::open(&png).expect("a PNG").into_rgba8();
    assert!(
        written(&shot, title) > 20,
        "the frame's title is drawn where it is laid out, {title:?}"
    );
}
