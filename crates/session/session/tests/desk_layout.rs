//! The desk comes back after a login: its strips, its groups and their
//! widths, the window that was in front — and the windows the person left
//! open, each asked for again into the tab it was in. A window whose app is
//! gone says so in its tab and closes; a desk this build cannot read starts
//! empty and says where the person's own desk still is.

#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::significant_drop_tightening,
    reason = "test fixtures: a failed step is the assertion, and the session \
              outlives every call made into it"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use tilo_compositor::OutputConfig;
use tilo_desktop_model::{
    DesktopTree, GroupId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth,
    Workspace, WorkspaceId,
};
use tilo_desktop_policy::Window;
use tilo_desktop_surface::notifications::NotificationSeverity;
use tilo_session::mount::apps::Apps;
use tilo_session::{AppActions, Options, Session};

const WIDTH: i32 = 1280;
const HEIGHT: i32 = 800;

/// The app a test installs: a Tilo package whose window is a tab of the desk.
const PROBE: &str = "so.tilo.probe";

/// The app of a saved window that is not on this machine.
const GONE: &str = "so.tilo.gone";

/// What a test names a strip it renames.
const RENAMED: &str = "Sketches";

/// Where this device keeps what outlives a session.
fn state_of(home: &tilo_test_env::TestHome) -> PathBuf {
    home.state().to_path_buf()
}

/// Where the session reads the desk this device left.
fn desk_file(state: &Path) -> PathBuf {
    state.join("tilo").join("desk.json")
}

/// Write `tree` as the desk this device left, where the next session reads it.
fn save_desk(state: &Path, tree: &DesktopTree) {
    let path = desk_file(state);
    fs::create_dir_all(path.parent().expect("a tilo directory")).expect("the state directory");
    fs::write(
        &path,
        serde_json::to_string(tree).expect("the desk is written"),
    )
    .expect("the desk is saved");
}

fn tab(id: u64, label: &str, app: &str) -> Tab {
    Tab {
        id,
        title: label.into(),
        tile: TileContent::LocalApp {
            app_id: app.into(),
            label: label.into(),
            note: String::new(),
        },
        app_title: None,
    }
}

fn group(id: u64, tabs: Vec<Tab>, front: u64, width: WindowWidth) -> WindowGroup {
    WindowGroup {
        id,
        tabs,
        active_tab: front,
        width,
        maximized: false,
        focus_history: Vec::new(),
    }
}

/// The one workspace `strips` make up, in front.
fn on_desk(strips: Vec<Strip>) -> DesktopTree {
    let mut home = Workspace::new(WorkspaceId(1), "Home");
    home.strips = strips;
    let mut tree = DesktopTree {
        active: home.id,
        workspaces: vec![home],
        ..DesktopTree::default()
    };
    tree.active_strip.insert(WorkspaceId(1), StripId(1));
    tree.set_active_group(StripId(1), 10);
    tree
}

/// A desk with two strips of two groups in one workspace, and a second
/// workspace behind it: each strip and group carrying what the person chose.
fn wide_desk() -> DesktopTree {
    let mut writing = Strip::new(StripId(1), "Writing");
    writing.window_groups = vec![
        group(
            10,
            vec![tab(100, "Notes", "so.tilo.notes"), tab(101, "Build", PROBE)],
            101,
            WindowWidth::Fraction(480),
        ),
        group(
            11,
            vec![tab(110, "Files", "so.tilo.files")],
            110,
            WindowWidth::Narrow,
        ),
    ];
    writing.window_groups[1].maximized = true;
    let mut reference = Strip::new(StripId(2), "Reference");
    reference.window_groups = vec![group(
        20,
        vec![tab(200, "Mail", "so.tilo.mail")],
        200,
        WindowWidth::Normal,
    )];

    let mut away = Workspace::new(WorkspaceId(2), "Away");
    away.strips = vec![Strip::new(StripId(3), "Later")];

    let mut tree = on_desk(vec![writing, reference]);
    tree.workspaces.push(away);
    tree
}

/// A desk holding one saved window of `app`, alone in its group.
fn one_window(app: &str, label: &str) -> DesktopTree {
    let mut strip = Strip::new(StripId(1), "Windows");
    strip.window_groups = vec![group(
        10,
        vec![tab(100, label, app)],
        100,
        WindowWidth::Fraction(600),
    )];
    on_desk(vec![strip])
}

/// A session on the desk this device saved, starting `apps` when its windows
/// are asked for. `None`: a session that starts nothing at all.
fn session_keeping(state: &Path, apps: Option<Rc<Apps>>) -> Session {
    Session::headless(
        Options {
            presentation: ui::headless::fixture_runner_inputs,
            collab: false,
            apps: apps
                .as_ref()
                .map(|apps| Rc::clone(apps) as Rc<dyn AppActions>),
            launch: apps,
            state: Some(state.to_path_buf()),
            ..Options::default()
        },
        &[OutputConfig::new("HEADLESS-1", WIDTH, HEIGHT)],
    )
    .expect("session")
}

/// The apps this session starts, on its own display.
fn apps_at(home: &tilo_test_env::TestHome, launcher: Option<PathBuf>) -> Rc<Apps> {
    let mut environment = tilo_app_launch::SessionEnvironment::from_process();
    environment.wayland_socket = Some(home.runtime().join("wayland-7"));
    Rc::new(Apps::with_launcher(environment, None, launcher))
}

/// Run frames until `done` holds.
fn until(session: &mut Session, what: &str, mut done: impl FnMut(&Session) -> bool) {
    for _ in 0..400 {
        session.step(Duration::from_millis(1)).expect("step");
        if done(session) {
            return;
        }
    }
    panic!("{what} did not happen");
}

/// Every tab of the desk, with the group it sits in: what a second window or
/// a second tab of a restored one would change.
fn tabs(session: &Session) -> Vec<(TabId, GroupId)> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| group.tabs.iter().map(move |tab| (tab.id, group.id)))
            .collect()
    })
}

/// Every tab of the desk that stands for an app.
fn app_tabs(session: &Session) -> Vec<TabId> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .filter(|tab| matches!(tab.tile, TileContent::LocalApp { .. }))
            .map(|tab| tab.id)
            .collect()
    })
}

/// What the tab `id` holds.
fn tile_of(session: &Session, id: u64) -> Option<TileContent> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| &group.tabs)
            .find(|tab| tab.id == id)
            .map(|tab| tab.tile.clone())
    })
}

/// The note the tab `id` shows in place of its window.
fn note_of(session: &Session, id: u64) -> Option<String> {
    match tile_of(session, id)? {
        TileContent::LocalApp { note, .. } => Some(note),
        _ => None,
    }
}

/// A window of `app` the compositor mapped.
fn window(id: u64, app: &str) -> Window {
    Window {
        id,
        app_id: Some(app.into()),
        title: Some(app.into()),
        ..Window::default()
    }
}

/// Install [`PROBE`] in `home`'s own app store, and write the launcher the
/// session starts it through: it records the pid of every start and runs until
/// the test creates `stop`.
fn installed_probe(home: &mut tilo_test_env::TestHome, started: &Path, stop: &Path) -> PathBuf {
    let store = home.data().join("tilo-apps");
    let payload = store.join("payload").join(PROBE);
    fs::create_dir_all(payload.join("bin")).unwrap();
    let manifest = payload.join("manifest.toml");
    fs::write(
        &manifest,
        format!(
            "[package]\n\
             id = \"{PROBE}\"\n\
             version = \"1.0.0\"\n\
             kind = \"app\"\n\
             name = \"Probe\"\n\
             \n\
             [publisher]\n\
             identity = \"ed25519:{key}\"\n\
             signature = \"\"\n\
             \n\
             [sandbox]\n\
             namespaces = [\"user\"]\n\
             seccomp = \"tilo-default\"\n\
             landlock = \"strict\"\n\
             \n\
             [sandbox.net]\n\
             mode = \"off\"\n\
             \n\
             [entrypoint]\n\
             bin = \"bin/app\"\n\
             type = \"gui\"\n\
             \n\
             [providers]\n\
             schemes = [\"probe\"]\n",
            key = "11".repeat(32),
        ),
    )
    .unwrap();
    fs::write(
        store.join("registry.toml"),
        format!(
            "[[entry]]\nid = \"{PROBE}\"\nversion = \"1.0.0\"\n\
             payload_root = \"{payload}\"\nmanifest_path = \"{manifest}\"\n",
            payload = payload.display(),
            manifest = manifest.display(),
        ),
    )
    .unwrap();
    home.env().set("TILO_APPS_DATA_DIR", &store);
    let launcher = home.home().join("tilo-launcher");
    tilo_test_env::script(
        &launcher,
        &format!(
            "#!/bin/sh\nprintf '%s\\n' \"$$\" >> '{started}'\n\
             while [ ! -e '{stop}' ]; do sleep 0.05; done\n",
            started = started.display(),
            stop = stop.display(),
        ),
    )
    .unwrap();
    launcher
}

#[test]
fn a_restart_brings_back_the_strips_groups_widths_and_the_window_in_front() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    save_desk(&state_of(&home), &wide_desk());

    // Nothing is asked of any app: this is the shape of the desk coming back.
    let mut session = session_keeping(&state_of(&home), None);
    session.step(Duration::from_millis(1)).expect("step");

    let (workspaces, names, widths, maximized, front, strip_in_front) =
        session.handle().read(|tree| {
            let home = &tree.workspaces[0];
            (
                tree.workspaces.len(),
                home.strips
                    .iter()
                    .map(|strip| strip.name.clone())
                    .collect::<Vec<_>>(),
                home.strips[0]
                    .window_groups
                    .iter()
                    .map(|group| group.width.permille())
                    .collect::<Vec<_>>(),
                home.strips[0].window_groups[1].maximized,
                tilo_desktop_surface::navigation::active_route(tree).map(|route| route.tab),
                tree.active_strip.get(&WorkspaceId(1)).copied(),
            )
        });
    assert_eq!(workspaces, 2, "the workspace behind came back too");
    assert_eq!(names, ["Writing", "Reference"], "the strips, in order");
    assert_eq!(widths, [480, 333], "each group kept the width it had");
    assert!(maximized, "so did the group the person maximized");
    assert_eq!(strip_in_front, Some(StripId(1)));
    assert_eq!(
        front,
        Some(101u64),
        "the window that was in front is in front again"
    );
    assert_eq!(
        tile_of(&session, 101),
        Some(TileContent::LocalApp {
            app_id: PROBE.into(),
            label: "Build".into(),
            note: String::new(),
        }),
        "a window is kept as the app that opens it again"
    );
}

/// What a restart brings back is the desk the session itself kept, with no
/// tree file beside it: a strip renamed in one session is that strip in the
/// next, at the width it was left.
#[test]
fn a_restart_brings_back_the_desk_the_session_kept() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    let state = state_of(&home);
    save_desk(&state, &wide_desk());

    let kept = {
        let mut session = session_keeping(&state, None);
        session.step(Duration::from_millis(1)).expect("step");
        let mut tree = session.handle().read(Clone::clone);
        tree.workspaces[0].strips[0].name = RENAMED.into();
        session.handle().replace_tree(tree);
        session.step(Duration::from_millis(1)).expect("step");
        session
    };
    // A session keeps the desk as it ends.
    drop(kept);

    assert!(
        !desk_file(&state).exists(),
        "the desk is no longer kept as a tree file"
    );
    assert!(
        state.join("tilo").join("desk.automerge").exists(),
        "it is kept as its one document"
    );

    let mut next = session_keeping(&state, None);
    next.step(Duration::from_millis(1)).expect("step");
    let (name, width) = next.handle().read(|tree| {
        let strip = &tree.workspaces[0].strips[0];
        (strip.name.clone(), strip.window_groups[0].width.permille())
    });
    assert_eq!(name, RENAMED, "the strip as it was renamed");
    assert_eq!(width, 480, "at the width it was left");
}

#[test]
fn a_running_app_is_asked_again_and_its_window_takes_the_tab_it_was_in() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("started");
    let stop = home.home().join("stop");
    let launcher = installed_probe(&mut home, &started, &stop);
    save_desk(&state_of(&home), &one_window(PROBE, "Probe"));

    let mut session = session_keeping(&state_of(&home), Some(apps_at(&home, Some(launcher))));
    let before = tabs(&session);
    assert_eq!(before, [(100u64, 10u64)], "the desk as it was left");

    until(&mut session, "the app is asked for its window", |_| {
        started.exists()
    });
    // The window the app opened, as the compositor hands it over.
    session
        .desktop()
        .with_policy(|policy| policy.map(window(11, PROBE)))
        .expect("the window maps");
    until(&mut session, "the window is in its tab", |session| {
        matches!(
            tile_of(session, 100),
            Some(TileContent::Foreign { surface_id: 11, .. })
        )
    });

    assert_eq!(
        tabs(&session),
        before,
        "the window took the tab it was in: no second tab, no second group"
    );
    assert_eq!(
        session.desktop().with_policy(|policy| policy.focused()),
        Some(11),
        "and it is the window the person sees"
    );
    fs::write(&stop, "").unwrap();
}

#[test]
fn a_window_whose_app_is_gone_says_so_in_its_tab_and_that_tab_closes() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    save_desk(&state_of(&home), &one_window(GONE, "Gone"));

    let mut session = session_keeping(&state_of(&home), Some(apps_at(&home, None)));
    until(&mut session, "the tab says why it is empty", |session| {
        note_of(session, 100).is_some_and(|note| !note.is_empty())
    });
    let note = note_of(&session, 100).expect("the tab holds the app it was");
    assert!(
        note.contains("Gone"),
        "the tab names the app it holds: {note}"
    );
    assert!(
        note.len() > "Gone".len(),
        "and says why it is not here: {note}"
    );
    let labels = session.accessible_labels();
    assert!(
        labels.iter().any(|label| label == "Gone"),
        "the tab names the app where its window would be: {labels:?}"
    );
    assert!(
        session
            .desktop()
            .with_policy(|policy| policy.windows_in_front().is_empty()),
        "and no window stands in its place"
    );
    assert_eq!(
        tabs(&session),
        [(100u64, 10u64)],
        "nothing else on the desk changed"
    );

    assert!(
        session
            .desktop()
            .with_policy(|policy| policy.close_tab_of(GONE)),
        "the tab is the person's to close"
    );
    until(&mut session, "the tab is closed", |session| {
        tabs(session).is_empty()
    });
}

#[test]
fn a_desk_that_cannot_be_read_starts_empty_and_says_the_desk_is_still_there() {
    let _settled = ui::motion::testing::settled();
    let home = tilo_test_env::TestHome::new().unwrap();
    let state = state_of(&home);
    let path = desk_file(&state);
    fs::create_dir_all(path.parent().expect("a tilo directory")).expect("the state directory");
    fs::write(&path, "{ not a desk").expect("a desk file that cannot be read");

    let mut session = session_keeping(&state, Some(apps_at(&home, None)));
    session.step(Duration::from_millis(1)).expect("step");

    assert!(
        app_tabs(&session).is_empty(),
        "the session starts on an empty desk: {:?}",
        app_tabs(&session)
    );
    let shown = session.notifications().log().active().to_vec();
    let about = shown
        .iter()
        .find(|notice| notice.body.contains(&path.display().to_string()))
        .unwrap_or_else(|| {
            panic!(
                "the person is told where their desk is: {:?}",
                shown
                    .iter()
                    .map(|notice| (notice.title.clone(), notice.body.clone()))
                    .collect::<Vec<_>>()
            )
        });
    assert!(
        !about.title.trim().is_empty(),
        "the notice says what happened"
    );
    assert_eq!(
        about.severity,
        NotificationSeverity::Error,
        "and is not one that fades before it is read"
    );
    assert!(
        path.with_extension("json.not-opened").exists(),
        "the reason is left beside the desk, for anyone not looking at the screen"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("the desk is still there"),
        "{ not a desk",
        "nothing is written over a desk this session did not open"
    );
}
