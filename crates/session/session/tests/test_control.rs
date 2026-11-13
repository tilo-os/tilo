//! The development session as `dist/dev/nested-session.sh` runs it: seen through the test-control
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
use std::os::unix::process::{CommandExt as _, ExitStatusExt as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::{Value, json};
use tilo_compositor::KeyRepeat;
use tilo_desktop::desk_verbs::{DESK_RESOURCE, SEARCH_RESOURCE};
use tilo_desktop_model::{
    DesktopTree, Strip, StripId, Tab, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceSlot,
};
use tilo_service::Service as _;
use tilo_session::shell::state::WINDOW_MENU;
use tilo_session::{Options, Session};
use tilo_test_env::SpawnUnderEnvLock as _;

use crate::common::{CANVAS_ENTRY, HEIGHT, PERSON, WIDTH, control as ask, private_home, reached};

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

/// The desk's preset menu, once the shell stands on the output: the bar names the preset the
/// desk shows, and which one that is depends on the desk the session starts from.
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
        .expect("the desk's preset menu")
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
        shell: true,
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
    assert_eq!(
        answer,
        json!({"ok": {"frame": 0, "locked": false}}),
        "the snapshot is taken, of a session that is not locked"
    );

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
            .shell_control(CANVAS_ENTRY)
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

/// Whether a filesystem is mounted at `at`.
fn mounted(at: &Path) -> bool {
    std::fs::read_to_string("/proc/self/mountinfo")
        .unwrap()
        .lines()
        .any(|entry| entry.split(' ').nth(4) == Some(at.to_str().unwrap()))
}

/// A headless `tilo-session`, started as `dist/dev/nested-session.sh` starts its first pass, in a
/// runtime directory and a process group of its own.
struct StartedSession {
    child: std::process::Child,
    /// The directory of its sockets and of the mount, kept until the test ends.
    runtime: tempfile::TempDir,
    /// The home it reads, kept until the test ends.
    _home: tempfile::TempDir,
    control: PathBuf,
    /// Where the files people choose for apps are mounted.
    documents: PathBuf,
}

impl StartedSession {
    /// The session, up once it printed its test-control socket, the last of what it prints.
    fn start() -> Self {
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
            // A stop of the whole group, as a session is stopped in a terminal or
            // by a script, reaches it through this group.
            .process_group(0)
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
            documents: runtime.path().join("tilo").join("documents"),
            child,
            runtime,
            _home: home,
            control,
        }
    }

    /// The session and its mount, when this machine can mount FUSE.
    fn started() -> Option<Self> {
        let session = Self::start();
        if mounted(&session.documents) {
            return Some(session);
        }
        tilo_test_env::capability_unavailable(
            tilo_test_env::Capability::Fuse,
            format_args!(
                "the session mounted nothing at {}",
                session.documents.display()
            ),
        );
        None
    }

    /// The throwaway identity, as `tilo-control raw` asks for it.
    fn mint_identity(&self) {
        let mut stream = UnixStream::connect(&self.control).expect("the test-control socket");
        stream
            .write_all(b"{\"op\":\"provision_test_identity\"}\n")
            .unwrap();
        let mut minted = String::new();
        BufReader::new(stream).read_line(&mut minted).unwrap();
        let minted: Value = serde_json::from_str(&minted).expect("a JSON answer");
        assert_eq!(minted["answer"]["throwaway"], true, "{minted}");
    }

    /// The group the session leads: `kill(2)` reads a negative pid as every process in that
    /// process group.
    fn group(&self) -> libc::pid_t {
        libc::pid_t::try_from(self.child.id())
            .expect("a pid fits a pid_t")
            .wrapping_neg()
    }

    /// Sends `signal` to the session's process group: `kill(2)` reads only the pid and the
    /// signal number.
    fn signal_group(&self, signal: libc::c_int) {
        let group = self.group();
        // SAFETY: a pid and a signal number are all kill(2) reads.
        let sent = unsafe { libc::kill(group, signal) };
        assert_eq!(
            sent,
            0,
            "signal {signal} to the session's process group: {}",
            std::io::Error::last_os_error()
        );
    }
}

impl Drop for StartedSession {
    /// A test that failed before it ended the session still leaves none behind.
    fn drop(&mut self) {
        if self.child.try_wait().is_ok_and(|ended| ended.is_none()) {
            let group = self.group();
            // SAFETY: a pid and a signal number are all kill(2) reads.
            let _ = unsafe { libc::kill(group, libc::SIGKILL) };
            let _ = self.child.wait();
        }
    }
}

/// A stop of the session's whole process group signals every member, and a wrapper that was
/// signalled with the group passes the signal on: the session is asked twice, and the second
/// must not end it before it has put away what it mounted.
#[test]
fn a_session_stopped_with_its_whole_group_ends_cleanly_and_unmounts_what_it_mounted() {
    let Some(mut session) = StartedSession::started() else {
        return;
    };
    session.mint_identity();
    session.signal_group(libc::SIGTERM);
    // SAFETY: a pid and a signal number are all kill(2) reads.
    let _ = unsafe {
        libc::kill(
            libc::pid_t::try_from(session.child.id()).expect("a pid fits a pid_t"),
            libc::SIGTERM,
        )
    };
    let status = session.child.wait().unwrap();
    assert!(status.success(), "the session ends cleanly: {status:?}");
    assert!(
        !mounted(&session.documents),
        "the files people choose for apps are unmounted"
    );
    assert!(
        !session.control.exists(),
        "the test-control socket is removed"
    );
}

/// The person pressing interrupt again while the session ends means it: the signal's own action
/// ends the process then, cleanup or not.
#[test]
fn a_second_sigint_ends_the_session_at_once() {
    let Some(mut session) = StartedSession::started() else {
        return;
    };
    session.mint_identity();
    // Standard signals do not queue, so one sent before the session has read
    // the last would arrive as the same one: interrupt it until it is gone.
    let group = session.group();
    // SAFETY: a pid and a signal number are all kill(2) reads.
    while session.child.try_wait().unwrap().is_none() {
        unsafe { libc::kill(group, libc::SIGINT) };
    }
    let status = session.child.wait().unwrap();
    assert_eq!(status.signal(), Some(libc::SIGINT), "{status:?}");
    tilo_test_env::wait_until("the killed session's mount to be removed", || {
        !mounted(&session.documents)
    });
}

/// A session killed outright leaves nothing of its mount behind: the process that watched it
/// unmounts, and the next session can mount, and remove, the directory it used.
#[test]
fn a_session_killed_outright_leaves_no_mount_behind() {
    let Some(mut session) = StartedSession::started() else {
        return;
    };
    session.mint_identity();
    session.signal_group(libc::SIGKILL);
    let status = session.child.wait().unwrap();
    assert_eq!(status.signal(), Some(libc::SIGKILL), "{status:?}");
    tilo_test_env::wait_until("the killed session's mount to be removed", || {
        !mounted(&session.documents)
    });
    std::fs::remove_dir_all(session.runtime.path())
        .expect("the runtime directory the killed session left is removed");
    // The next session mounts there as any session does.
    let again = tilo_documents::mount::mount(tilo_documents::Documents::new(), &session.documents)
        .expect("a session mounts the documents where the killed one had them");
    drop(again);
    assert!(
        !mounted(&session.documents),
        "the documents are unmounted when their session drops them"
    );
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

/// Text typed through the socket into the canvas's entry reaches it: a click on the entry puts
/// the caret there, and what is typed stands in the field as the person's own typing does.
#[test]
fn text_typed_through_the_socket_reaches_the_canvas_entry() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("test-control.sock");
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        shell: true,
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
            .shell_control(CANVAS_ENTRY)
            .is_some()),
        "the canvas: {:?}",
        session.accessible_labels()
    );

    // The press a person's pointer makes on the field, waited out as a hand's is (the frame it
    // lands on is the one that shows it), then text as the socket writes it.
    let (x, y, w, h) = session.shell_control(CANVAS_ENTRY).expect("the entry");
    let (cx, cy) = (x + w / 2.0, y + h / 2.0);
    assert_eq!(
        ask(
            &mut session,
            &socket,
            &json!({"op": "click_at", "x": cx, "y": cy})
        ),
        json!({"ok": {}}),
        "the click on the entry is taken"
    );
    assert!(
        session.compositor().host_takes_keys(),
        "the press put the caret in the entry: {:?}",
        session.accessible_labels()
    );
    let typed = ask(
        &mut session,
        &socket,
        &json!({"op": "type", "text": "groceries"}),
    );
    assert_eq!(typed, json!({"ok": {}}), "the text is typed");
    assert!(
        reached(&mut session, |session| session.shell_says("groceries")),
        "the entry shows what was typed: {:?}; the shell refused {:?}",
        session.accessible_labels(),
        session.shell_refusals()
    );
}

/// A note of the seeded notes app placed on the framed canvas is seen in the snapshot: its
/// frame's title is drawn where the shell lays it out, not cut off by a canvas with no room.
#[test]
fn a_note_placed_on_the_canvas_is_drawn_in_the_snapshot() {
    let _settled = ui::motion::testing::settled();
    let root = private_home();
    let socket = root.path().join("test-control.sock");
    let apps = crate::common::seeded_apps();
    let note = apps
        .resources()
        .iter()
        .flat_map(|resource| resource.children.clone())
        .find(|resource| resource.label == "Groceries")
        .expect("the seeded note")
        .id
        .as_str()
        .to_owned();
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        shell: true,
        collab: false,
        services: vec![Box::new(apps)],
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
        .shell_control(CANVAS_ENTRY)
        .is_some()));

    let (workspace, row) = {
        let tree = session.handle().read(Clone::clone);
        let workspace = tree.active_workspace().expect("a workspace is in front");
        (
            workspace.slot.0.to_string(),
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
    // The frame names its window the way every surface does: the window's own title and its
    // app's own name, resolved once by the session.
    let shown = session
        .arrangement()
        .borrow()
        .reading()
        .places
        .iter()
        .find(|place| place.title == "Groceries")
        .map(|place| place.name.clone())
        .expect("the desk holds the note's place");
    let title = session.shell_control(&shown).expect("the frame's title");

    let png = root.path().join("shot.png");
    let answer = ask(
        &mut session,
        &socket,
        &json!({"op": "snapshot", "path": png.to_str().unwrap()}),
    );
    assert_eq!(
        answer,
        json!({"ok": {"frame": 0, "locked": false}}),
        "the snapshot is taken, of a session that is not locked"
    );
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
        view: String::new(),
        pinned: false,
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
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: groups,
    }];
    let mut tree = DesktopTree {
        active: workspace.slot,
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip.insert(WorkspaceSlot(1), StripId(2));
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
    let mut workspace = Workspace::new(WorkspaceSlot(1), "Work");
    workspace.strips = vec![Strip {
        id: StripId(2),
        name: "Main".into(),
        window_groups: vec![group],
    }];
    let mut tree = DesktopTree {
        active: workspace.slot,
        workspaces: vec![workspace],
        ..Default::default()
    };
    tree.active_strip.insert(WorkspaceSlot(1), StripId(2));
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
    let mut session = crate::common::headless(Options {
        person: PERSON.to_owned(),
        collab: false,
        test_control: Some(socket.to_path_buf()),
        ..Options::default()
    });
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
/// control door drives the desk's gestures the way a touchpad does.
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
    let mut session = session_showing(desk(&windows), &socket);
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
