//! The development session as `dist/dev/nested-session.sh` runs it: seen through the test-control
//! socket `tilo-control snapshot` writes a PNG over, with the tilolang apps seeded, and ending on
//! SIGTERM as it ends on its own.

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

use std::io::{BufRead as _, BufReader};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tilo_compositor::KeyRepeat;
use tilo_desktop_model::{
    Layout, Strip, StripId, WindowGroup, WindowWidth, Workspace, WorkspaceId,
};
use tilo_session::shell::state::WINDOW_MENU;
use tilo_session::{Options, Session};
use tilo_session_core::desktop::verbs::{SEARCH_RESOURCE, SHELL_RESOURCE};
use tilo_test_env::DiesWithTheTest as _;
use tilo_test_env::SpawnUnderEnvLock as _;

use crate::common::tree::window;
use crate::common::{CANVAS, HEIGHT, PERSON, WIDTH, control as ask, private_home, reached};

/// Click the shell's control `name` through the test-control socket, as the person does: the
/// socket takes the click, and what it presses is what stands there.
fn click(session: &mut Session, socket: &Path, name: &str) {
    let (x, y, w, h) = session
        .shell_control(name)
        .unwrap_or_else(|| panic!("the shell shows {name}: {:?}", session.accessible_labels()));
    let clicked = ask(
        session,
        socket,
        &json!({"op": "click_at", "x": x + w / 2.0, "y": y + h / 2.0}),
    );
    assert_eq!(clicked, json!({"ok": {}}), "the click on {name} is taken");
}

/// The shell's preset menu, once the shell stands on the output: the bar names the preset the
/// shell shows, and which one that is depends on the shell the session starts from.
fn preset_menu(session: &mut Session) -> String {
    assert!(
        reached(session, |session| session
            .shell_control("Notifications")
            .is_some()),
        "the shell stands on the output: {:?}",
        session.accessible_labels()
    );
    session
        .accessible_labels()
        .into_iter()
        .find(|label| label.starts_with("Showing ") && session.shell_control(label).is_some())
        .expect("the shell's preset menu")
}

/// A headless session with the shell as the whole screen writes a PNG of it through
/// `tilo-control snapshot`'s request, and the bar stands in it.
#[test]
fn a_snapshot_through_the_test_control_socket_shows_the_shells_bar() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        collab: false,
        test_control: Some(socket.clone()),
        ..Options::default()
    });
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

    // A click through the socket presses the screen's own control there: the bar's preset menu
    // opens, and the framed canvas it offers stands with its entry where the strips stood.
    let menu = preset_menu(&mut session);
    click(&mut session, &socket, &menu);
    assert!(
        reached(&mut session, |session| session
            .shell_control("Frames")
            .is_some()),
        "the preset menu offers Frames: {:?}",
        session.accessible_labels()
    );
    click(&mut session, &socket, "Frames");
    assert!(
        reached(&mut session, |session| session
            .shell_control(CANVAS)
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
    let apps = crate::common::seeded_apps();
    let resources = crate::common::published(&apps);
    let mut kinds = std::collections::BTreeSet::new();
    walk(&resources, &mut kinds);
    for kind in [
        "tl.so.tilo.notes.Note",
        "tl.so.tilo.tasks.Task",
        "tl.so.tilo.lang.chat.TextChannel",
        "tl.so.tilo.mail.Mailbox",
    ] {
        assert!(kinds.contains(kind), "{kind} is published: {kinds:?}");
    }
}

/// A headless `tilo-session`, started as `dist/dev/nested-session.sh` starts its first pass, in a
/// runtime directory and a process group of its own.
struct StartedSession {
    child: std::process::Child,
    /// This account's daemon, which serves the session: a session is one of a computer's, and
    /// its identity is made through that computer's daemon.
    daemon: std::process::Child,
    /// The directory of its sockets, kept until the test ends.
    runtime: tempfile::TempDir,
    /// The home it reads, kept until the test ends.
    _home: tempfile::TempDir,
}

impl StartedSession {
    /// The session, up once it printed its test-control socket, the last of what it prints, with
    /// `env` set in its environment besides.
    fn start(env: &[(&str, &str)]) -> Self {
        let runtime = private_home();
        let home = tempfile::tempdir().unwrap();
        let control = runtime.path().join("test-control.sock");
        std::fs::create_dir_all(home.path().join("identity")).unwrap();
        // The daemon and the session read one environment: the same home, identity store and
        // runtime directory, so the daemon the test starts is the computer the session finds.
        let configure = |command: &mut std::process::Command| {
            command
                .env("XDG_RUNTIME_DIR", runtime.path())
                .env("TILO_IDENTITY_ROOT", home.path().join("identity"))
                .env("HOME", home.path())
                .env("XDG_CONFIG_HOME", home.path().join(".config"))
                .env("XDG_DATA_HOME", home.path().join(".local/share"))
                .env("XDG_STATE_HOME", home.path().join(".local/state"))
                .env("XDG_CACHE_HOME", home.path().join(".cache"))
                .env_remove("WAYLAND_DISPLAY")
                .env_remove("DISPLAY");
        };
        // A session is one of a computer's: this account's daemon serves it, and makes the
        // account's identity through `devices#found`.
        let mut daemon = std::process::Command::new(tilo_test_env::sibling_binary(
            "tilo-daemon",
            "tilo-runtime",
        ));
        configure(&mut daemon);
        let daemon = daemon
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .dies_with_the_test()
            .spawn_locked()
            .expect("this account's daemon starts");
        let socket = runtime.path().join(tilo_paths::machine::DAEMON_NATIVE);
        let bound = std::time::Instant::now();
        while std::os::unix::net::UnixStream::connect(&socket).is_err() {
            assert!(
                bound.elapsed() < tilo_test_env::hang_bound(),
                "this account's daemon does not answer at {}",
                socket.display()
            );
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let mut session =
            std::process::Command::new(tilo_test_env::package_binary!("tilo-session"));
        configure(&mut session);
        let mut child = session
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
            .envs(env.iter().copied())
            // A stop of the whole group, as a session is stopped in a terminal or
            // by a script, reaches it through this group; and one of the test itself ends it
            // where the test ends without running a destructor.
            .process_group(0)
            .dies_with_the_test()
            .stdout(std::process::Stdio::piped())
            .spawn_locked()
            .expect("tilo-session starts");
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
        Self {
            child,
            runtime,
            _home: home,
            daemon,
        }
    }

    /// The group the session leads: `kill(2)` reads a negative pid as every process in that
    /// process group.
    fn group(&self) -> libc::pid_t {
        libc::pid_t::try_from(self.child.id())
            .expect("a pid fits a pid_t")
            .wrapping_neg()
    }
}

impl Drop for StartedSession {
    /// A test that failed before it ended the session still leaves none behind, nor the daemon
    /// that served it.
    fn drop(&mut self) {
        if self.child.try_wait().is_ok_and(|ended| ended.is_none()) {
            let group = self.group();
            // SAFETY: a pid and a signal number are all kill(2) reads.
            let _ = unsafe { libc::kill(group, libc::SIGKILL) };
            let _ = self.child.wait();
        }
        if self.daemon.try_wait().is_ok_and(|ended| ended.is_none()) {
            let _ = self.daemon.kill();
            let _ = self.daemon.wait();
        }
    }
}

/// A session started as `dist/dev/nested-session.sh` starts it, beside another desktop
/// (`TILO_MACHINE=hosted`), serves its apps on the socket its launcher hands them: the apps, windows
/// and agents it starts find the collaboration broker there. It once served them on a socket of its
/// own that no launcher knew, so every app it started ran without one and its agents exited.
#[test]
fn a_hosted_session_serves_its_apps_where_its_launcher_hands_them() {
    let session = StartedSession::start(&[(tilo_paths::machine::VAR, "hosted")]);
    let collab = session.runtime.path().join("tilo/services/collab.sock");
    assert!(
        collab.exists(),
        "the session serves its apps at {}: {:?}",
        collab.display(),
        std::fs::read_dir(session.runtime.path().join("tilo/services")).map(|entries| entries
            .flatten()
            .map(|entry| entry.file_name())
            .collect::<Vec<_>>())
    );
}

/// A session started as `dist/dev/nested-session.sh` starts it (`TILO_MACHINE=hosted`, no agent
/// key) serves the person's agents at the login's own path. It once served none, so no agent could
/// reach a nested session.
#[test]
fn a_hosted_session_serves_its_persons_agents() {
    let session = StartedSession::start(&[(tilo_paths::machine::VAR, "hosted")]);
    let server = session
        .runtime
        .path()
        .join("tilo-session")
        .join(tilo_paths::login::AGENT_SOCKET_FILE);
    UnixStream::connect(&server)
        .unwrap_or_else(|error| panic!("an agent reaches {}: {error}", server.display()));
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

/// A sentence typed through the socket at the canvas's caret stands there in the order it was
/// typed: every key of one `type` reaches the caret after the one before it, a letter that recurs
/// ("summarise") included.
#[test]
fn a_sentence_typed_through_the_socket_reaches_the_canvas_caret_in_order() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        collab: false,
        test_control: Some(socket.clone()),
        ..Options::default()
    });
    let menu = preset_menu(&mut session);
    click(&mut session, &socket, &menu);
    assert!(
        reached(&mut session, |session| session
            .shell_control("Frames")
            .is_some()),
        "the preset menu offers Frames: {:?}",
        session.accessible_labels()
    );
    click(&mut session, &socket, "Frames");
    assert!(
        reached(&mut session, |session| session
            .shell_control(CANVAS)
            .is_some()),
        "the canvas: {:?}",
        session.accessible_labels()
    );
    let (x, y, w, h) = session.shell_control(CANVAS).expect("the canvas");
    assert_eq!(
        ask(
            &mut session,
            &socket,
            &json!({"op": "click_at", "x": x + w / 2.0, "y": y + h / 2.0})
        ),
        json!({"ok": {}}),
        "the click on the canvas is taken"
    );
    // The caret the press puts there stands the frame after, holds the keyboard, and the keys go
    // to it.
    assert!(
        reached(&mut session, |session| {
            session.shell_control(crate::common::CANVAS_CARET).is_some()
        }),
        "the press put the caret on the canvas: {:?}",
        session.accessible_labels()
    );
    assert!(
        session.compositor().surface_takes_keys(),
        "the caret holds the keyboard: {:?}",
        session.accessible_labels()
    );
    let sentence = "summarise what changed in my notes this week";
    let typed = ask(
        &mut session,
        &socket,
        &json!({"op": "type", "text": sentence}),
    );
    assert_eq!(typed, json!({"ok": {}}), "the text is typed");
    assert!(
        reached(&mut session, |session| session.shell_says(sentence)),
        "the caret shows the sentence as typed; of its words it shows {:?}",
        ["summarise", "summaries", "summari", "what", "week"]
            .into_iter()
            .filter(|word| session.shell_says(word))
            .collect::<Vec<_>>()
    );
}

/// A note of the seeded notes app placed on the framed canvas is seen in the snapshot: its
/// title is drawn where the shell lays it out, not cut off by a canvas with no room.
#[test]
fn a_note_placed_on_the_canvas_is_drawn_in_the_snapshot() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("test-control.sock");
    let apps = crate::common::seeded_apps();
    let note = crate::common::published(&apps)
        .iter()
        .flat_map(|resource| resource.children.clone())
        .find(|resource| resource.label == "Groceries")
        .expect("the seeded note")
        .id
        .as_str()
        .to_owned();
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        collab: false,
        services: apps,
        test_control: Some(socket.clone()),
        ..Options::default()
    });
    let menu = preset_menu(&mut session);
    click(&mut session, &socket, &menu);
    assert!(reached(&mut session, |session| session
        .shell_control("Frames")
        .is_some()));
    click(&mut session, &socket, "Frames");
    assert!(reached(&mut session, |session| session
        .shell_control(CANVAS)
        .is_some()));

    let (workspace, row) = {
        let tree = session.handle().read(Clone::clone);
        let workspace = tree.active_workspace().expect("a workspace is in front");
        (
            workspace.id().to_string(),
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
        .invoke("shell:layout", "place", arguments.as_object().unwrap())
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
    // A place drawn as content goes by its own title on the canvas: it is not a window, so its
    // app's name is not joined to it.
    let title = session
        .shell_control("Groceries")
        .expect("the place's title");

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
        "the place's title is drawn where it is laid out, {title:?}"
    );
}

/// A shell of one strip holding `windows`, each in a group of its own, the last in front.
fn shell(windows: &[(u64, &str)]) -> Layout {
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
    let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: groups,
    }];
    let mut tree = Layout {
        active: workspace.id(),
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip
        .insert(WorkspaceId::from_u128(1), StripId(2));
    tree.active_group.insert(StripId(2), front);
    tree
}

/// A shell of one window holding `tabs`, the last of them in front.
fn tabbed(tabs: &[(u64, &str)]) -> Layout {
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
    let mut workspace = Workspace::new(WorkspaceId::from_u128(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![group],
    }];
    let mut tree = Layout {
        active: workspace.id(),
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip
        .insert(WorkspaceId::from_u128(1), StripId(2));
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
fn session_showing(tree: Layout, socket: &Path) -> Session {
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        collab: false,
        test_control: Some(socket.to_path_buf()),
        ..Options::default()
    });
    session.handle().replace_tree(tree);
    session.desktop().pass();
    session.step(Duration::from_millis(1)).expect("step");
    session
}

/// Every group of the shell's first strip, with the tabs it holds, in order.
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

/// Each window's tab is named by the group that holds it, so a harness or an agent finds the
/// window it sees even where two windows carry one title.
#[test]
fn each_windows_tab_is_named_by_the_group_that_holds_it() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = session_showing(shell(&[(20, "Alpha"), (21, "Beta")]), &socket);
    let answer = ask(&mut session, &socket, &json!({"op": "list_targets"}));
    let names: Vec<&str> = answer["targets"]
        .as_array()
        .expect("the targets are a list")
        .iter()
        .filter_map(|target| target["name"].as_str())
        .collect();
    for group in groups(&session).into_iter().map(|(group, _)| group) {
        let tagged = format!("[{group}]");
        assert!(
            names
                .iter()
                .any(|name| name.starts_with("tab:") && name.ends_with(&tagged)),
            "the window in group {group} names its tab: {names:?}"
        );
    }
}

/// The open search, as the shell publishes it.
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

tilo_test_env::scenario! {
    fn the_test_control_socket_drives_the_session_as_a_person_does;

    phase text_typed_through_the_socket_reaches_the_open_search;
    // Letters typed through the socket land in the field the shell is listening through: the open
    // search's query, not the window behind it.
    {
        let _settled = ui::motion::testing::settled();
        let root = private_home();
        let socket = root.path().join("test-control.sock");
        let mut session = session_showing(shell(&[(20, "Alpha")]), &socket);
        // A step of a headless session can take longer than a repeat waits; the text is typed once.
        session.compositor().set_key_repeat(KeyRepeat {
            delay: Duration::from_secs(60),
            rate: 0,
        });
        session
            .invoke(SHELL_RESOURCE, "search", &serde_json::Map::new())
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

    phase a_drag_through_the_socket_moves_a_tab_into_another_group;
    // A tab dragged through the socket from one window's bar to another's is the other window's,
    // as the person's own drag makes it.
    {
        let _settled = ui::motion::testing::settled();
        let root = private_home();
        let socket = root.path().join("test-control.sock");
        let mut session = session_showing(shell(&[(20, "Alpha"), (21, "Beta")]), &socket);
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
}

/// Where the window titled `title` stands on the screen, as `list_targets` reports it, and the
/// picture the session shows, written to `png`.
fn swipe_frame(session: &mut Session, socket: &Path, title: &str, png: &Path) -> f64 {
    let shot = ask(
        session,
        socket,
        &json!({"op": "snapshot", "path": png.to_str().unwrap()}),
    );
    assert!(shot.get("ok").is_some(), "the snapshot is written: {shot}");
    let targets = ask(session, socket, &json!({"op": "list_targets"}));
    target_rect(&targets, title)
        .unwrap_or_else(|| panic!("{title} stands somewhere: {targets}"))
        .0
}

/// Three fingers through the socket carry a strip wider than the screen while they are down, each
/// held swipe going on with the same gesture, and lifted while moving the strip coasts on: the
/// test control drives the shell's gestures the way a touchpad does.
#[test]
fn a_held_swipe_through_the_socket_carries_the_strip_with_the_fingers() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("test-control.sock");
    let shots = std::env::var_os("STRIP_SWIPE_SHOTS")
        .map_or_else(|| root.path().to_path_buf(), PathBuf::from);
    let windows = [
        (20, "Alpha"),
        (21, "Beta"),
        (22, "Gamma"),
        (23, "Delta"),
        (24, "Epsilon"),
        (25, "Zeta"),
    ];
    let mut session = session_showing(shell(&windows), &socket);
    let before = swipe_frame(
        &mut session,
        &socket,
        "Epsilon",
        &shots.join("swipe-0-before.png"),
    );

    let swipe = |session: &mut Session, dx: f64, release: bool| {
        let answer = ask(
            session,
            &socket,
            &json!({"op": "swipe", "fingers": 3, "dx": dx, "dy": 0.0, "steps": 12,
                    "release": release}),
        );
        assert_eq!(answer, json!({"ok": {}}), "the swipe is taken");
    };
    // Rightward under natural scrolling, the default: the windows go with the fingers.
    swipe(&mut session, 150.0, false);
    let first = swipe_frame(
        &mut session,
        &socket,
        "Epsilon",
        &shots.join("swipe-1-held.png"),
    );
    swipe(&mut session, 150.0, false);
    let second = swipe_frame(
        &mut session,
        &socket,
        "Epsilon",
        &shots.join("swipe-2-held.png"),
    );
    assert!(
        (first - before - 150.0).abs() <= 2.0 && (second - first - 150.0).abs() <= 2.0,
        "the strip is where the fingers holding it took it: {before} then {first} then {second}"
    );
    // Lifted while still moving, the strip coasts on the way they went and rests there.
    swipe(&mut session, 150.0, true);
    for _ in 0..30 {
        session.step(Duration::from_millis(16)).expect("step");
    }
    let rested = swipe_frame(
        &mut session,
        &socket,
        "Epsilon",
        &shots.join("swipe-3-lifted.png"),
    );
    assert!(
        rested > second + 150.0,
        "the strip coasts past where the fingers lifted: {second} then {rested}"
    );
}

/// A secondary click through the socket on a window's bar opens the window's menu, as the
/// person's own right click does.
#[test]
fn a_secondary_click_through_the_socket_opens_the_window_menu() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = session_showing(shell(&[(20, "Alpha")]), &socket);
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
