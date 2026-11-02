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
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a rectangle the test measured in the output it drew is within that output"
)]

use std::io::{BufRead as _, BufReader, Write as _};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use tilo_compositor::{KeyRepeat, OutputConfig};
use tilo_desktop::desk_verbs::{DESK_RESOURCE, SEARCH_RESOURCE};
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace, WorkspaceId,
};
use tilo_service::Service as _;
use tilo_session::mount::shell::WINDOW_MENU;
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
    let refused = ask(&mut session, &socket, &json!({"op": "nonsense"}));
    assert_eq!(refused["err"]["code"], "unsupported", "{refused}");
}

/// The seeded apps publish notes, tasks, chat and mail, each filled by its first scenario: the
/// notes are found by their title, and the inbox is among what the session publishes.
#[test]
fn the_seeded_apps_publish_their_records_to_the_session() {
    fn walk(
        resources: &[tilo_actions::ResourceSpec],
        kinds: &mut std::collections::BTreeSet<String>,
    ) {
        for resource in resources {
            kinds.insert(resource.kind.clone());
            walk(&resource.children, kinds);
        }
    }
    let apps = tilo_session::seeded_apps::seeded(PERSON).expect("the apps load and seed");
    let resources = apps.resources();
    let mut kinds = std::collections::BTreeSet::new();
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
fn written(shot: &image::RgbaImage, rect: (f64, f64, f64, f64)) -> usize {
    let (left, top, width, height) = rect;
    let mut light = 0usize;
    for row in (top as u32)..((top + height) as u32).min(shot.height()) {
        for column in (left as u32)..((left + width) as u32).min(shot.width()) {
            let [red, green, blue, _] = shot.get_pixel(column, row).0;
            let glow = u32::from(red)
                .saturating_add(u32::from(green))
                .saturating_add(u32::from(blue));
            if glow > 384 {
                light = light.saturating_add(1);
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

/// A window on the desk: one tab of an app, named by its title.
fn window(id: u64, title: &str, app: &str) -> Tab {
    Tab {
        id,
        title: title.into(),
        tile: TileContent::LocalApp {
            app_id: app.into(),
            label: title.into(),
            note: String::new(),
        },
        app_title: None,
    }
}

/// A desk of one strip holding `windows`, each in a group of its own, the last in front.
fn desk(windows: &[(u64, &str)]) -> DesktopTree {
    let groups: Vec<WindowGroup> = windows
        .iter()
        .enumerate()
        .map(|(at, (id, title))| WindowGroup {
            id: u64::try_from(at).map_or(u64::MAX, |at| at.saturating_add(100)),
            active_tab: *id,
            tabs: vec![window(*id, title, "so.tilo.notes")],
            width: WindowWidth::Normal,
            maximized: false,
            focus_history: Vec::new(),
        })
        .collect();
    let front = groups.last().expect("a window").id;
    let mut workspace = Workspace::new(WorkspaceId(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: groups,
    }];
    let mut tree = DesktopTree {
        active: workspace.id,
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip.insert(WorkspaceId(1), StripId(2));
    tree.active_group.insert(StripId(2), front);
    tree
}

/// A desk of one window holding `tabs`, the last of them in front.
fn tabbed(tabs: &[(u64, &str)]) -> DesktopTree {
    let front = tabs.last().map(|(id, _)| *id).expect("a tab");
    let group = WindowGroup {
        id: 100,
        active_tab: front,
        tabs: tabs
            .iter()
            .map(|(id, title)| window(*id, title, "so.tilo.notes"))
            .collect(),
        width: WindowWidth::Normal,
        maximized: false,
        focus_history: Vec::new(),
    };
    let mut workspace = Workspace::new(WorkspaceId(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![group],
    }];
    let mut tree = DesktopTree {
        active: workspace.id,
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip.insert(WorkspaceId(1), StripId(2));
    tree.active_group.insert(StripId(2), 100);
    tree
}

/// The tab the window `group` shows.
fn active_tab(session: &Session, group: u64) -> Option<u64> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .find(|candidate| candidate.id == group)
            .map(|candidate| candidate.active_tab)
    })
}

/// A headless session that shows `tree` and answers on the test-control `socket`.
fn session_showing(tree: DesktopTree, socket: &Path) -> Session {
    let mut session = Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            person: PERSON.to_owned(),
            collab: false,
            test_control: Some(socket.to_path_buf()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session");
    session.handle().replace_tree(tree);
    session
        .desktop()
        .with_widgets(tilo_surface_host::WidgetHost::relayout);
    session.step(Duration::from_millis(1)).expect("step");
    session
}

/// Every group of the desk's first strip, with the tabs it holds, in order.
fn groups(session: &Session) -> Vec<(u64, Vec<u64>)> {
    session.handle().read(|tree| {
        tree.workspaces
            .first()
            .and_then(|workspace| workspace.strips.first())
            .map(|strip| {
                strip
                    .window_groups
                    .iter()
                    .map(|group| (group.id, group.tabs.iter().map(|tab| tab.id).collect()))
                    .collect()
            })
            .unwrap_or_default()
    })
}

/// Where the control `name` stands, as `list_targets` reports it, in logical coordinates.
fn target_rect(answer: &Value, name: &str) -> Option<(f64, f64, f64, f64)> {
    let rect = answer["targets"]
        .as_array()?
        .iter()
        .find(|target| target["name"] == name)?
        .get("rect")?
        .as_array()?;
    Some((
        rect.first()?.as_f64()?,
        rect.get(1)?.as_f64()?,
        rect.get(2)?.as_f64()?,
        rect.get(3)?.as_f64()?,
    ))
}

/// The middle of a rectangle.
fn center((x, y, width, height): (f64, f64, f64, f64)) -> (f64, f64) {
    (x + width / 2.0, y + height / 2.0)
}

/// The open search, as the desk publishes it.
fn search(session: &Session) -> Option<tilo_actions::ResourceSpec> {
    fn find(
        resources: &[tilo_actions::ResourceSpec],
        id: &str,
    ) -> Option<tilo_actions::ResourceSpec> {
        resources.iter().find_map(|resource| {
            if resource.id.as_str() == id {
                Some(resource.clone())
            } else {
                find(&resource.children, id)
            }
        })
    }
    find(&session.resources(), SEARCH_RESOURCE)
}

/// Letters typed through the socket land in the field the desk is listening through: the open
/// search's query, not the window behind it.
#[test]
fn text_typed_through_the_socket_reaches_the_open_search() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = session_showing(desk(&[(20, "Alpha")]), &socket);
    // A step of a headless session can take longer than a repeat waits; the text is typed once.
    session.compositor().set_key_repeat(KeyRepeat {
        delay: Duration::from_secs(60),
        rate: 0,
    });
    session
        .invoke(DESK_RESOURCE, "search", &serde_json::Map::new())
        .expect("the search opens");
    assert!(
        reached(&mut session, |session| search(session).is_some()),
        "the search shows: {:?}",
        session.accessible_labels()
    );

    let typed = ask(&mut session, &socket, &json!({"op": "type", "text": "cal"}));
    assert_eq!(typed, json!({"ok": {}}), "the text is typed");
    assert!(
        reached(&mut session, |session| search(session)
            .is_some_and(|search| search.label == "cal")),
        "the query reads what was typed: {:?}",
        search(&session).map(|search| search.label)
    );
}

/// A tab dragged through the socket from one window's bar to another's is the other window's,
/// as the person's own drag makes it.
#[test]
fn a_drag_through_the_socket_moves_a_tab_into_another_group() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = session_showing(desk(&[(20, "Alpha"), (21, "Beta")]), &socket);
    let targets = ask(&mut session, &socket, &json!({"op": "list_targets"}));
    let beta = target_rect(&targets, "Beta")
        .unwrap_or_else(|| panic!("the Beta pill stands somewhere: {targets}"));
    let (from_x, from_y) = center(beta);
    // The other window stands to the left, past the display's edge: the part of
    // its bar that shows still takes the drop.
    let dragged = ask(
        &mut session,
        &socket,
        &json!({
            "op": "drag",
            "x_from": from_x,
            "y_from": from_y,
            "x_to": 100.0,
            "y_to": from_y,
        }),
    );
    assert_eq!(dragged, json!({"ok": {}}), "the drag is taken");
    assert!(
        reached(&mut session, |session| groups(session)
            .iter()
            .any(|(_, tabs)| tabs.contains(&20) && tabs.contains(&21))),
        "the dragged tab shares the other window's group: {:?}",
        groups(&session)
    );
}

/// A secondary click through the socket on a window's bar opens the window's menu, as the
/// person's own right click does.
#[test]
fn a_secondary_click_through_the_socket_opens_the_window_menu() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = session_showing(desk(&[(20, "Alpha")]), &socket);
    let targets = ask(&mut session, &socket, &json!({"op": "list_targets"}));
    let notes = target_rect(&targets, "Alpha")
        .unwrap_or_else(|| panic!("the Alpha pill stands somewhere: {targets}"));
    let (x, y) = center(notes);
    assert_eq!(
        ask(
            &mut session,
            &socket,
            &json!({"op": "pointer_move", "x": x, "y": y})
        ),
        json!({"ok": {}}),
        "the pointer stands on the pill"
    );
    assert_eq!(
        ask(
            &mut session,
            &socket,
            &json!({"op": "pointer_button", "button": "right", "pressed": true})
        ),
        json!({"ok": {}}),
        "the right button goes down"
    );
    assert_eq!(
        ask(
            &mut session,
            &socket,
            &json!({"op": "pointer_button", "button": "right", "pressed": false})
        ),
        json!({"ok": {}}),
        "and comes back up"
    );
    assert!(
        reached(&mut session, |session| session
            .read(WINDOW_MENU)
            .is_some_and(|menu| menu["tab"] == json!("20"))),
        "the window's menu stands open: {:?}",
        session.read(WINDOW_MENU)
    );
}

/// A control invoked by the name the semantic tree gives it acts, without a point being aimed
/// at it: the other tab's pill brings that tab to the front.
#[test]
fn a_named_control_is_invoked_through_the_socket() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = session_showing(tabbed(&[(20, "Alpha"), (21, "Beta")]), &socket);
    assert_eq!(
        active_tab(&session, 100),
        Some(21),
        "the last tab is in front"
    );
    let targets = ask(&mut session, &socket, &json!({"op": "list_targets"}));
    assert!(
        target_rect(&targets, "Alpha").is_some(),
        "the pill is named: {targets}"
    );
    assert_eq!(
        ask(
            &mut session,
            &socket,
            &json!({"op": "click_named", "name": "Alpha"})
        ),
        json!({"ok": {}}),
        "the pill is pressed by its name"
    );
    assert!(
        reached(&mut session, |session| active_tab(session, 100) == Some(20)),
        "the tab it names is the one in front: {:?}",
        active_tab(&session, 100)
    );
}
