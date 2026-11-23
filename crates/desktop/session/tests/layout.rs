//! The layout comes back after a login: its strips, its groups and their widths, the window that was
//! in front — and the windows the person left open, each asked for again into the tab it was in. A
//! window whose app is gone says so in its tab and closes. The layout is the workspaces' layouts, so
//! what a session leaves is what the next one shows.

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
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use tilo_desktop_model::{
    GroupId, Layout, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth, Workspace,
    WorkspaceId,
};
use tilo_desktop_policy::Window;
use tilo_desktop_surface::surface::window_menu::move_to_workspace_id;
use tilo_session::apps::Apps;
use tilo_session::{AppActions, Options, Session};
use tilo_test_env::SpawnUnderEnvLock as _;

/// The app a test installs: a Tilo package whose window is a tab of the layout.
const PROBE: &str = "so.tilo.probe";

/// The app of a saved window that is not on this machine.
const GONE: &str = "so.tilo.gone";

/// An app whose windows are the person's own, never on a workspace's shared desktop.
const SETTINGS: &str = "so.tilo.settings";

/// What a test names a strip it renames.
const RENAMED: &str = "Sketches";

/// A device in a test's home: where it keeps its state, and the salt it numbers what it
/// makes in.
pub(crate) struct Device {
    pub(crate) state: PathBuf,
    salt: u16,
}

impl Device {
    /// The device of `home`, holding its workspaces.
    fn in_home(home: &mut tilo_test_env::TestHome) -> Self {
        let state = crate::common::device_in(home);
        let salt = session_keeping(&state, None)
            .handle()
            .read(|tree| tree.salt);
        Self { state, salt }
    }

    /// The device of `home` with `tree` left as the layout its session ended with
    /// ([`save_layout`]), and the ids that layout gave `tree`'s workspaces.
    pub(crate) fn with_layout(
        home: &mut tilo_test_env::TestHome,
        tree: &Layout,
    ) -> (Self, Vec<WorkspaceId>) {
        let state = crate::common::device_in(home);
        let (salt, workspace_ids) = save_layout(&state, tree);
        (Self { state, salt }, workspace_ids)
    }

    /// What this device numbers `n` as.
    fn id(&self, n: u64) -> u64 {
        tilo_desktop_model::compose_id(self.salt, n)
    }
}

/// `tree` with every strip, group and window numbered as the device whose salt is `salt` numbers
/// what it makes.
fn salted(salt: u16, tree: &Layout) -> Layout {
    let id = |n: u64| tilo_desktop_model::compose_id(salt, n);
    let mut tree = tree.clone();
    for strip in tree
        .workspaces
        .iter_mut()
        .flat_map(|workspace| &mut workspace.strips)
    {
        strip.id = StripId(id(strip.id.0));
        for group in &mut strip.window_groups {
            group.id = id(group.id);
            group.active_tab = id(group.active_tab);
            for tab in &mut group.tabs {
                tab.id = id(tab.id);
            }
        }
    }
    tree.active_strip = tree
        .active_strip
        .iter()
        .map(|(workspace_id, strip)| (*workspace_id, StripId(id(strip.0))))
        .collect();
    tree.active_group = tree
        .active_group
        .iter()
        .map(|(strip, group)| (StripId(id(strip.0)), id(*group)))
        .collect();
    tree
}

/// Leave `tree` as the layout a session of the device keeping its state at `state` ends with, its
/// ids numbered as that device numbers them ([`salted`]): the first of `tree`'s workspaces is the
/// one the session opened, the others are put beside it, each is laid out as `tree` does, and a
/// workspace's layout is written while it is in front. The session then ends as a desktop ends: what
/// it could not write yet of the shell stays on the device, and the next session writes it as the
/// person's steps. The device's salt, as that session reads it, and the ids the layout gave `tree`'s
/// workspaces, in order.
fn save_layout(state: &Path, tree: &Layout) -> (u16, Vec<WorkspaceId>) {
    let mut session = session_keeping(state, None);
    let salt = session.handle().read(|tree| tree.salt);
    let tree = salted(salt, tree);
    session.step(Duration::from_millis(1)).expect("step");
    let mut layout = session.handle().read(Clone::clone);
    for laid in tree.workspaces.iter().skip(layout.workspaces.len()) {
        layout
            .workspaces
            .push(Workspace::new(WorkspaceId::new(), laid.name.clone()));
    }
    let workspace_ids: Vec<WorkspaceId> = layout
        .workspaces
        .iter()
        .map(tilo_desktop_model::Workspace::id)
        .collect();
    for (index, laid) in tree.workspaces.iter().enumerate() {
        let held = &mut layout.workspaces[index];
        held.name.clone_from(&laid.name);
        held.strips.clone_from(&laid.strips);
        if let Some(strip) = tree.active_strip.get(&laid.id()) {
            layout.active_strip.insert(workspace_ids[index], *strip);
        }
        for strip in &laid.strips {
            if let Some(group) = tree.active_group.get(&strip.id) {
                layout.active_group.insert(strip.id, *group);
            }
        }
    }
    for (index, laid) in tree.workspaces.iter().enumerate() {
        if laid.tabs().next().is_none() {
            continue;
        }
        // A workspace's layout is written while it is in front.
        layout.active = workspace_ids[index];
        session.handle().replace_tree(layout);
        session.step(Duration::from_millis(1)).expect("step");
        layout = session.handle().read(Clone::clone);
    }
    let front = tree
        .workspaces
        .iter()
        .position(|held| held.id() == tree.active)
        .unwrap_or_default();
    layout.active = workspace_ids[front];
    session.handle().replace_tree(layout);
    session.step(Duration::from_millis(1)).expect("step");
    // The desktop ends: what it could not write yet stays on the device for the session after.
    drop(session);
    (salt, workspace_ids)
}

/// Run frames until the layout shows a window.
fn laid_out(session: &mut Session) {
    until(session, "the layout is laid out", |session| {
        !tabs(session).is_empty()
    });
}

/// Move the window `tab` to the workspace `to`, as the person does once the window's menu offers
/// the workspace.
fn move_to_workspace(session: &mut Session, tab: u64, to: WorkspaceId) {
    let row = move_to_workspace_id(to);
    until(session, "the window menu offers the workspace", |session| {
        session
            .handle()
            .window_menu(tab)
            .iter()
            .any(|offered| offered.find(&row).is_some())
    });
    session
        .handle()
        .choose_window_menu(tab, &row, None)
        .expect("the person moves the window");
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
        view: String::new(),
        pinned: false,
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
fn on_shell(strips: Vec<Strip>) -> Layout {
    let mut home = Workspace::new(WorkspaceId::from_u128(1), "Home");
    home.strips = strips;
    let mut tree = Layout {
        active: home.id(),
        workspaces: vec![home],
        ..Layout::default()
    };
    tree.active_strip
        .insert(WorkspaceId::from_u128(1), StripId(1));
    tree.set_active_group(StripId(1), 10);
    tree
}

/// A layout with two strips of two groups in one workspace, and a second workspace behind it: each
/// strip and group carrying what the person chose.
fn wide_shell() -> Layout {
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

    let mut away = Workspace::new(WorkspaceId::from_u128(2), "Away");
    away.strips = vec![Strip::new(StripId(3), "Later")];

    let mut tree = on_shell(vec![writing, reference]);
    tree.workspaces.push(away);
    tree
}

/// A layout holding one saved window of `app` , alone in its group.
fn one_window(app: &str, label: &str) -> Layout {
    let mut strip = Strip::new(StripId(1), "Windows");
    strip.window_groups = vec![group(
        10,
        vec![tab(100, label, app)],
        100,
        WindowWidth::Fraction(600),
    )];
    on_shell(vec![strip])
}

/// A session on the shell this device saved, starting `apps` when its windows are asked for. `None`
/// : a session that starts nothing at all. It takes part in no peer network, as a laptop that
/// boots with none: the workspaces it makes are founded on its own workspace service all the same.
fn session_keeping(state: &Path, apps: Option<Rc<Apps>>) -> Session {
    session_on(state, apps, false)
}

/// [`session_keeping`], taking part in the peer network when `network` says so.
fn session_on(state: &Path, apps: Option<Rc<Apps>>, network: bool) -> Session {
    crate::common::headless(Options {
        collab: network,
        apps: apps
            .as_ref()
            .map(|apps| Rc::clone(apps) as Rc<dyn AppActions>),
        launch: apps,
        state: Some(state.to_path_buf()),
        ..Options::default()
    })
}

/// [`session_keeping`] as the machine's own desktop is started: it asks whether its wrapper restarts it
/// in place, and ends its apps only when the login does.
fn session_restarted(state: &Path) -> Session {
    crate::common::headless(Options {
        collab: false,
        state: Some(state.to_path_buf()),
        teardown: Box::new(tilo_session::restart::teardown),
        ..Options::default()
    })
}

/// The apps this session starts, on its own display.
fn apps_at(home: &tilo_test_env::TestHome, launcher: Option<PathBuf>) -> Rc<Apps> {
    let mut environment = tilo_app_launch::SessionEnvironment::from_process();
    environment.wayland_socket = Some(home.runtime().join("wayland-7"));
    Rc::new(Apps::with_launcher(environment, None, launcher))
}

/// Run frames until `done` holds.
pub(crate) fn until(session: &mut Session, what: &str, mut done: impl FnMut(&Session) -> bool) {
    crate::common::wait_for(session, what, |session| done(session).then_some(()));
}

/// Every tab of the layout, with the group it sits in: what a second window or a second tab of a
/// restored one would change.
pub(crate) fn tabs(session: &Session) -> Vec<(TabId, GroupId)> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .flat_map(|workspace| &workspace.strips)
            .flat_map(|strip| &strip.window_groups)
            .flat_map(|group| group.tabs.iter().map(move |tab| (tab.id, group.id)))
            .collect()
    })
}

/// What the tab `id` holds.
pub(crate) fn tile_of(session: &Session, id: u64) -> Option<TileContent> {
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

/// Install [`PROBE`] in `home` 's own app store, and write the launcher the session starts it
/// through: it records the pid of every start and runs, as the app would, until the test is over.
fn installed_probe(home: &mut tilo_test_env::TestHome, started: &Path) -> tilo_test_env::StandIn {
    installed_probe_package(home);
    launcher(home, started)
}

/// The launcher the session starts packages through: it records the pid of every start and runs, as
/// the app would, until the test is over.
fn launcher(home: &tilo_test_env::TestHome, started: &Path) -> tilo_test_env::StandIn {
    tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"$$\" >> '{started}'\nhold\n",
            started = started.display(),
        ),
    )
    .unwrap()
}

/// The same launcher, recording what each start was handed as well.
fn recording_launcher(
    home: &tilo_test_env::TestHome,
    started: &Path,
    arguments: &Path,
) -> tilo_test_env::StandIn {
    tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"$$\" >> '{started}'\n\
             printf '%s\\n' \"$*\" >> '{arguments}'\n\
             hold\n",
            started = started.display(),
            arguments = arguments.display(),
        ),
    )
    .unwrap()
}

/// What the last start the launcher recorded was handed for `flag` : the argument that follows it.
fn handed(arguments: &Path, flag: &str) -> String {
    let recorded = fs::read_to_string(arguments).expect("the launcher recorded its start");
    let arguments: Vec<&str> = recorded
        .lines()
        .next_back()
        .expect("a line per start")
        .split_whitespace()
        .collect();
    let at = arguments
        .iter()
        .position(|argument| *argument == flag)
        .unwrap_or_else(|| panic!("the launch carries {flag}: {arguments:?}"));
    arguments
        .get(at.saturating_add(1))
        .unwrap_or_else(|| panic!("{flag} names a value: {arguments:?}"))
        .to_string()
}

/// Install [`PROBE`] in `home` 's own app store: a package whose entrypoint is the launcher's to
/// start.
fn installed_probe_package(home: &mut tilo_test_env::TestHome) {
    crate::common::probe_app(home, PROBE, "Probe");
}

#[test]
fn a_restart_brings_back_the_strips_groups_widths_and_the_window_in_front() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    // The widths, the maximized group and what is in front are the person's own view of the
    // workspace, a record beside the layout's: the session that leaves them is the one after the
    // device's first, which founded the workspace ([`Device::with_layout`] says why).
    let device = Device::in_home(&mut home);
    let (_, workspace_ids) = save_layout(&device.state, &wide_shell());

    // Nothing is asked of any app: this is the shape of the layout coming back.
    let mut session = session_keeping(&device.state, None);
    laid_out(&mut session);

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
                tilo_session_core::desktop::navigation::active_route(tree).map(|route| route.tab),
                tree.active_strip.get(&workspace_ids[0]).copied(),
            )
        });
    assert_eq!(workspaces, 2, "the workspace behind came back too");
    assert_eq!(names, ["Writing", "Reference"], "the strips, in order");
    assert_eq!(widths, [480, 333], "each group kept the width it had");
    assert!(maximized, "so did the group the person maximized");
    assert_eq!(strip_in_front, Some(StripId(device.id(1))));
    assert_eq!(
        front,
        Some(device.id(101)),
        "the window that was in front is in front again"
    );
    assert_eq!(
        tile_of(&session, device.id(101)),
        Some(TileContent::LocalApp {
            app_id: PROBE.into(),
            label: "Build".into(),
            note: String::new(),
        }),
        "a window is kept as the app that opens it again"
    );
}

/// What a restart brings back is the layout the session itself kept: a strip renamed in one session
/// is that strip in the next, at the width it was left.
#[test]
fn a_restart_brings_back_the_shell_the_session_kept() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let device = Device::in_home(&mut home);
    let state = device.state.clone();
    save_layout(&device.state, &wide_shell());

    let kept = {
        let mut session = session_keeping(&state, None);
        laid_out(&mut session);
        let mut tree = session.handle().read(Clone::clone);
        tree.workspaces[0].strips[0].name = RENAMED.into();
        session.handle().replace_tree(tree);
        session.step(Duration::from_millis(1)).expect("step");
        session
    };
    // A session keeps the layout as it ends.
    drop(kept);

    assert!(
        kept_file(&state, "so.tilo.session-shown").exists(),
        "the device keeps what it shows"
    );

    let mut next = session_keeping(&state, None);
    laid_out(&mut next);
    let (name, width) = next.handle().read(|tree| {
        let strip = &tree.workspaces[0].strips[0];
        (strip.name.clone(), strip.window_groups[0].width.permille())
    });
    assert_eq!(name, RENAMED, "the strip as it was renamed");
    assert_eq!(width, 480, "at the width it was left");
}

/// A shell laid out in the session that founds the device's workspace, ended before that workspace
/// can hold it, is there in the next session and is written as the person's steps.
#[test]
fn a_shell_a_session_ends_before_it_can_be_written_is_kept() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let state = crate::common::device_in(&mut home);
    // The device's first session: the workspace it shows is founded a moment after it starts.
    let kept = {
        let mut session = session_keeping(&state, None);
        let salt = session.handle().read(|tree| tree.salt);
        let tree = salted(salt, &wide_shell());
        let mut layout = session.handle().read(Clone::clone);
        let shown = layout.workspaces[0].id();
        layout.workspaces[0]
            .strips
            .clone_from(&tree.workspaces[0].strips);
        if let Some(strip) = tree.active_strip.get(&tree.workspaces[0].id()) {
            layout.active_strip.insert(shown, *strip);
        }
        for strip in &tree.workspaces[0].strips {
            if let Some(group) = tree.active_group.get(&strip.id) {
                layout.active_group.insert(strip.id, *group);
            }
        }
        session.handle().replace_tree(layout);
        // The look that draws it, and the session ends.
        session.step(Duration::from_millis(1)).expect("step");
        tabs(&session)
    };

    let mut next = session_keeping(&state, None);
    let mut wanted = kept;
    wanted.sort_unstable();
    let arrived = crate::common::reached(&mut next, |session| {
        let mut back = tabs(session);
        back.sort_unstable();
        back == wanted
    });
    if !arrived {
        let mut back = tabs(&next);
        back.sort_unstable();
        panic!("giving up waiting for the shell is back: {wanted:?} wanted, {back:?} back");
    }
    assert!(
        next.layout().borrow_mut().last_step().is_some(),
        "the shell is written as the person's steps"
    );
}

/// A window the person sends to a workspace they are not looking at is in that workspace after a
/// restart: the workspace it left and the one it went to are both written, though the person never
/// went there. A desktop with no peer network keeps it as one on the network does.
#[test]
fn a_window_moved_to_a_workspace_behind_comes_back_there() {
    moved_behind_comes_back(false);
}

/// [`a_window_moved_to_a_workspace_behind_comes_back_there`] on a desktop that takes part in the
/// peer network.
#[test]
fn a_window_moved_to_a_workspace_behind_on_the_network_comes_back_there() {
    moved_behind_comes_back(true);
}

/// A window moved to the workspace behind, by a session taking part in the peer network when
/// `network` says so, is there in the next one.
fn moved_behind_comes_back(network: bool) {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let (device, workspace_ids) = Device::with_layout(&mut home, &wide_shell());
    let mail = device.id(200);

    let mut session = session_on(&device.state, None, network);
    until(&mut session, "the window is back", |session| {
        workspace_of(session, mail).is_some()
    });
    move_to_workspace(&mut session, mail, workspace_ids[1]);
    until(
        &mut session,
        "the window is in the workspace behind",
        |session| workspace_of(session, mail) == Some(workspace_ids[1]),
    );
    assert_eq!(
        session.handle().read(|tree| tree.active),
        workspace_ids[0],
        "the person stays where they are"
    );
    // A look writes what moved, and the session ends as a restart ends it.
    session.step(Duration::from_millis(1)).expect("step");
    drop(session);

    let mut next = session_on(&device.state, None, network);
    until(&mut next, "the window is back", |session| {
        workspace_of(session, mail).is_some()
    });
    assert_eq!(
        workspace_of(&next, mail),
        Some(workspace_ids[1]),
        "the window is in the workspace the person sent it to"
    );
    assert_eq!(
        tile_of(&next, mail),
        Some(TileContent::LocalApp {
            app_id: "so.tilo.mail".into(),
            label: "Mail".into(),
            note: String::new(),
        }),
        "as the window it was"
    );
}

/// Set in the process a scene has: the signal a wrapper sends a desktop ends every later
/// layout write in the process that sees it, so a scene that must see one has a process of its
/// own.
const SIGNALLED_SCENE: &str = "TILO_TEST_SIGNALLED_RESTART";

/// A window moved in the instant before the wrapper signals an in-place restart is in the
/// workspace the person sent it to when the next desktop opens: the signal does not end the save.
#[test]
fn a_window_moved_just_before_the_restart_signal_comes_back_there() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let (device, workspace_ids) = Device::with_layout(&mut home, &wide_shell());
    let mail = device.id(200);
    // The desktop that sees the signal has a process of its own; this one opens the desktop
    // that follows, which a signal has to reach no more than the one before it.
    let scene = std::process::Command::new(std::env::current_exe().expect("this test program"))
        .args(["--exact", "layout::signalled_restart_scene", "--nocapture"])
        .env(SIGNALLED_SCENE, "1")
        .env("TILO_TEST_SCENE_STATE", &device.state)
        .env("TILO_TEST_SCENE_WORKSPACE", workspace_ids[1].to_string())
        .env("TILO_TEST_SCENE_TAB", mail.to_string())
        .output_locked()
        .expect("the scene starts");
    assert!(
        scene.status.success(),
        "the scene failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&scene.stdout),
        String::from_utf8_lossy(&scene.stderr)
    );

    let mut next = session_keeping(&device.state, None);
    until(&mut next, "the window is back", |session| {
        workspace_of(session, mail).is_some()
    });
    assert_eq!(
        workspace_of(&next, mail),
        Some(workspace_ids[1]),
        "the window moved in the instant before the signal is where the person sent it"
    );
}

/// Not a test: the desktop [`a_window_moved_just_before_the_restart_signal_comes_back_there`]
/// ends with the wrapper's signal, which would end every later write in this process too.
#[test]
fn signalled_restart_scene() {
    if std::env::var_os(SIGNALLED_SCENE).is_none() {
        return;
    }
    let state =
        PathBuf::from(std::env::var_os("TILO_TEST_SCENE_STATE").expect("the scene's state"));
    let workspace: WorkspaceId = std::env::var("TILO_TEST_SCENE_WORKSPACE")
        .expect("the scene's workspace")
        .parse()
        .expect("a workspace id");
    let mail: u64 = std::env::var("TILO_TEST_SCENE_TAB")
        .expect("the scene's window")
        .parse()
        .expect("a tab id");
    let mut session = session_restarted(&state);
    until(&mut session, "the window is back", |session| {
        workspace_of(session, mail).is_some()
    });
    move_to_workspace(&mut session, mail, workspace);
    until(
        &mut session,
        "the window is in the workspace behind",
        |session| workspace_of(session, mail) == Some(workspace),
    );
    let private = signalled_restart_directory();
    // The wrapper named its private directory in the environment: this process sets it for
    // its one scene.
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set(
        "TILO_SESSION_LOGOUT_REQUEST",
        private.path().join("logout-request"),
    );
    // The wrapper's signal, in the instant no look follows.
    tilo_session::termination::install(tilo_service::LoopWake::default().handle())
        .expect("a signal ends this desktop");
    // SAFETY: the handler is installed and only stores a flag.
    unsafe { libc::raise(libc::SIGTERM) };
    assert!(
        tilo_session::termination::asked(),
        "the signal reached the desktop"
    );
    drop(session);
    drop(private);
}

/// The login's own directory with the wrapper's restart marker in it, as it is before an
/// in-place restart. The `TempDir` is held for as long as the marker matters.
fn signalled_restart_directory() -> tempfile::TempDir {
    let private = tempfile::tempdir().expect("the login's directory");
    fs::set_permissions(private.path(), fs::Permissions::from_mode(0o700)).expect("private");
    let marker = private.path().join("restart-request");
    fs::write(&marker, b"restart\n").expect("the wrapper's marker");
    fs::set_permissions(&marker, fs::Permissions::from_mode(0o600)).expect("private");
    private
}

#[test]
fn a_running_app_is_asked_again_and_its_window_takes_the_tab_it_was_in() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("started");
    let launcher = installed_probe(&mut home, &started);
    let (device, _) = Device::with_layout(&mut home, &one_window(PROBE, "Probe"));

    let mut session = session_keeping(
        &device.state,
        Some(apps_at(&home, Some(launcher.path().to_owned()))),
    );
    laid_out(&mut session);
    let before = tabs(&session);
    assert_eq!(
        before,
        [(device.id(100), device.id(10))],
        "the layout as it was left"
    );

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
            tile_of(session, device.id(100)),
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
}

/// The app a saved window's tab opens again into is told which tab it is: the instance its window
/// is, which what it reports of the window names.
#[test]
fn a_window_that_opens_again_is_told_which_tab_it_is_in() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("started");
    let instances = home.home().join("instances");
    installed_probe_package(&mut home);
    let launcher = tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"$$\" >> '{started}'\n\
             printf '%s\\n' \"${{TILO_CONTINUITY_INSTANCE:-}}\" >> '{instances}'\n\
             hold\n",
            started = started.display(),
            instances = instances.display(),
        ),
    )
    .unwrap();
    let (device, _) = Device::with_layout(&mut home, &one_window(PROBE, "Probe"));

    let mut session = session_keeping(
        &device.state,
        Some(apps_at(&home, Some(launcher.path().to_owned()))),
    );
    until(&mut session, "the app is asked for its window", |_| {
        instances.exists()
            && fs::read_to_string(&instances).is_ok_and(|said| !said.trim().is_empty())
    });
    assert_eq!(
        fs::read_to_string(&instances)
            .expect("the launcher recorded its start")
            .trim(),
        device.id(100).to_string(),
        "the app is told the tab its window takes"
    );
}

/// A window another app opens on what it shows — Files opening a file in an app, here a link — is
/// told the tab it lands in from its start, as a window the layout opens is: what it shows is kept for
/// that tab, so the restart that brings the tab back finds it.
#[test]
fn a_window_opened_on_content_is_told_the_tab_it_lands_in() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("started");
    let instances = home.home().join("instances");
    installed_probe_package(&mut home);
    let launcher = tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"$$\" >> '{started}'\n\
             printf '%s\\n' \"${{TILO_CONTINUITY_INSTANCE:-}}\" >> '{instances}'\n\
             hold\n",
            started = started.display(),
            instances = instances.display(),
        ),
    )
    .unwrap();
    let state = Device::in_home(&mut home).state;

    let apps = apps_at(&home, Some(launcher.path().to_owned()));
    let mut session = session_keeping(&state, Some(Rc::clone(&apps)));
    let workspace = session
        .handle()
        .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
        .expect("a workspace is in front");
    apps.start(
        &tilo_app_launch::Launch::Package {
            app: PROBE.into(),
            open: Some(tilo_app_launch::Open::Uri(
                "https://example.org/budget-for-friday".into(),
            )),
        },
        &apps.workspace_named(&workspace),
    )
    .expect("the launch is taken");
    until(&mut session, "the app is started", |_| {
        starts(&started) == 1
    });
    let instance: u64 = fs::read_to_string(&instances)
        .expect("the launcher recorded its start")
        .trim()
        .parse()
        .expect("the app is told the tab its window takes");
    session
        .desktop()
        .with_policy(|policy| policy.map(window(11, PROBE)))
        .expect("the window maps");
    until(
        &mut session,
        "the window takes the tab it was told",
        |session| {
            matches!(
                tile_of(session, instance),
                Some(TileContent::Foreign { surface_id: 11, .. })
            )
        },
    );
    assert_eq!(tabs(&session).len(), 1, "one tab, the window's");
}

/// How many times the launcher wrote a start down: one line per app the session started.
fn starts(started: &Path) -> usize {
    fs::read_to_string(started).map_or(0, |written| {
        written
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count()
    })
}

/// A window the person opened in one session opens again in the next: the layout keeps a running
/// window as the app that opened it, and the next session opens that app, in the tab it was in.
#[test]
fn a_window_the_person_opened_opens_again_after_a_restart() {
    opens_again_after_a_restart(PROBE);
}

/// A window of the person's own (Settings) is never in the workspace's layout, and opens again
/// after a restart all the same: the device's view keeps it, and the next session asks its app.
#[test]
fn a_window_of_the_persons_own_opens_again_after_a_restart() {
    opens_again_after_a_restart(SETTINGS);
}

/// The person opens `app`, its window maps into the tab the launch made, the session restarts, and
/// the next session asks `app` for its window again, into that tab.
fn opens_again_after_a_restart(app: &str) {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("started");
    crate::common::probe_app(&mut home, app, "Probe");
    let launcher = launcher(&home, &started);
    let state = Device::in_home(&mut home).state;

    // The person opens the app from the layout, the compositor maps its window into the tab it made,
    // and the session stops with that window open.
    let before = {
        let apps = apps_at(&home, Some(launcher.path().to_owned()));
        let mut session = session_keeping(&state, Some(Rc::clone(&apps)));
        let workspace = session
            .handle()
            .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
            .expect("a workspace is in front");
        apps.start(
            &tilo_app_launch::Launch::Package {
                app: app.into(),
                open: None,
            },
            &apps.workspace_named(&workspace),
        )
        .expect("the layout starts the app");
        until(&mut session, "the app is started", |_| {
            starts(&started) == 1
        });
        session
            .desktop()
            .with_policy(|policy| policy.map(window(11, app)))
            .expect("the window maps");
        until(&mut session, "the window is in its tab", |session| {
            tabs(session).len() == 1
                && matches!(
                    tile_of(session, tabs(session)[0].0),
                    Some(TileContent::Foreign { surface_id: 11, .. })
                )
        });
        tabs(&session)
    };

    // The next session starts on what this one kept, and asks the app for its window again, into
    // the tab it was in.
    let mut next = session_keeping(
        &state,
        Some(apps_at(&home, Some(launcher.path().to_owned()))),
    );
    until(&mut next, "the app is asked for its window again", |_| {
        starts(&started) == 2
    });
    assert_eq!(
        tabs(&next),
        before,
        "the tab it was in is kept for it while the app starts"
    );
    next.desktop()
        .with_policy(|policy| policy.map(window(12, app)))
        .expect("the window maps");
    until(&mut next, "the app's new window takes its tab", |session| {
        matches!(
            tile_of(session, before[0].0),
            Some(TileContent::Foreign { surface_id: 12, .. })
        )
    });
    assert_eq!(
        tabs(&next),
        before,
        "no second tab opens beside the saved one"
    );
}

/// The person closes a window of their own (Settings) and goes on to open another app: the closed
/// window stays closed, its app is not asked for it again, and opening Settings later makes a new
/// window rather than bringing back the closed one. The view this device last wrote still names the
/// window until the person's view rests and is written again, and the look between is no reason to
/// put it back.
#[test]
fn a_window_of_the_persons_own_stays_closed() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("started");
    crate::common::probe_app(&mut home, SETTINGS, "Settings");
    crate::common::probe_app(&mut home, PROBE, "Probe");
    let launcher = launcher(&home, &started);
    let state = Device::in_home(&mut home).state;
    let apps = apps_at(&home, Some(launcher.path().to_owned()));
    let mut session = session_keeping(&state, Some(Rc::clone(&apps)));
    let workspace = session
        .handle()
        .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
        .expect("a workspace is in front");
    let open = |app: &str| {
        apps.start(
            &tilo_app_launch::Launch::Package {
                app: app.into(),
                open: None,
            },
            &apps.workspace_named(&workspace),
        )
        .expect("the person opens the app");
    };
    let shown_in = |session: &Session, surface: u64| {
        tabs(session).into_iter().find(|(tab, _)| {
            matches!(
                tile_of(session, *tab),
                Some(TileContent::Foreign { surface_id, .. }) if surface_id == surface
            )
        })
    };

    open(SETTINGS);
    until(&mut session, "Settings is started", |_| {
        starts(&started) == 1
    });
    session
        .desktop()
        .with_policy(|policy| policy.map(window(11, SETTINGS)))
        .expect("the window maps");
    until(&mut session, "Settings' window is in its tab", |session| {
        shown_in(session, 11).is_some()
    });
    let (closed, _) = shown_in(&session, 11).expect("Settings' window has a tab");
    // The person's view rests with the window in it, and this device writes it.
    let _ = crate::common::steps_within(&mut session, Duration::from_secs(1), |_| None::<()>);

    assert!(
        session
            .desktop()
            .with_policy(|policy| policy.close_tab_of(SETTINGS)),
        "the person closes Settings' window"
    );
    session.desktop().with_policy(|policy| policy.unmap(11));
    until(&mut session, "the window is closed", |session| {
        tabs(session).is_empty()
    });

    // The person goes on to another app: the look that takes its window in is no reason to put
    // Settings back.
    open(PROBE);
    until(&mut session, "the other app is started", |_| {
        starts(&started) == 2
    });
    session
        .desktop()
        .with_policy(|policy| policy.map(window(12, PROBE)))
        .expect("the other app's window maps");
    until(
        &mut session,
        "the other app's window is in its tab",
        |session| shown_in(session, 12).is_some(),
    );
    let came_back = crate::common::steps_within(&mut session, Duration::from_secs(2), |session| {
        (tabs(session).len() != 1 || starts(&started) != 2)
            .then(|| (tabs(session), starts(&started)))
    });
    assert_eq!(
        came_back, None,
        "the closed window stays closed and its app is not asked for it (tabs, starts)"
    );

    // Opening Settings again makes a window of its own, not the closed one. Its app still runs, so
    // the new window comes from it rather than from a second start.
    open(SETTINGS);
    until(&mut session, "Settings' new window has a tab", |session| {
        tabs(session).len() == 2
    });
    session
        .desktop()
        .with_policy(|policy| policy.map(window(13, SETTINGS)))
        .expect("the new window maps");
    until(
        &mut session,
        "Settings' new window is in its tab",
        |session| shown_in(session, 13).is_some(),
    );
    let (reopened, _) = shown_in(&session, 13).expect("the new window has a tab");
    assert_ne!(reopened, closed, "a new window, not the closed one back");
    assert_eq!(
        tabs(&session).len(),
        2,
        "the other app's window and Settings' new one"
    );
}

/// The person opens an app before the session first laid out the workspace's layout, and the
/// layout holds no window of it: the place the launch made is the person's, not one an earlier
/// session left unfilled. It stays, and the window takes it.
#[test]
fn a_launch_before_the_first_layout_keeps_its_place() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("started");
    let launcher = installed_probe(&mut home, &started);
    let state = Device::with_layout(&mut home, &one_window(GONE, "Gone"))
        .0
        .state;

    let apps = apps_at(&home, Some(launcher.path().to_owned()));
    let mut session = session_keeping(&state, Some(Rc::clone(&apps)));
    let workspace = session
        .handle()
        .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
        .expect("a workspace is in front");
    apps.start(
        &tilo_app_launch::Launch::Package {
            app: PROBE.into(),
            open: None,
        },
        &apps.workspace_named(&workspace),
    )
    .expect("the person opens the app");
    until(&mut session, "the app is started", |_| {
        starts(&started) == 1
    });
    until(&mut session, "the saved layout is laid out", |session| {
        tabs(session).iter().any(|(tab, _)| {
            matches!(
                tile_of(session, *tab),
                Some(TileContent::LocalApp { ref app_id, .. }) if app_id == GONE
            )
        })
    });
    for _ in 0..100 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    assert_eq!(
        tabs(&session).len(),
        2,
        "the launch keeps its place beside the saved window"
    );

    session
        .desktop()
        .with_policy(|policy| policy.map(window(11, PROBE)))
        .expect("the window maps");
    until(
        &mut session,
        "the window takes the launch's place",
        |session| {
            tabs(session).iter().any(|(tab, _)| {
                matches!(
                    tile_of(session, *tab),
                    Some(TileContent::Foreign { surface_id: 11, .. })
                )
            })
        },
    );
    let probes = tabs(&session)
        .iter()
        .filter(|(tab, _)| {
            matches!(
                tile_of(&session, *tab),
                Some(TileContent::Foreign { ref app_id, .. } | TileContent::LocalApp { ref app_id, .. })
                    if app_id == PROBE
            )
        })
        .count();
    assert_eq!(probes, 1, "one place of the app, holding its window");
    assert_eq!(starts(&started), 1, "the app is started once");
}

#[test]
fn a_launch_before_the_restore_asks_starts_the_app_into_the_saved_tab() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("started");
    let launcher = installed_probe(&mut home, &started);
    let (device, _) = Device::with_layout(&mut home, &one_window(PROBE, "Probe"));
    let state = device.state.clone();

    // The session starts on the shell, and the person — or an action reaching the app from somewhere
    // else — opens it before the restore has asked for its saved window: that launch starts the
    // app, into the tab the window was left in.
    let apps = apps_at(&home, Some(launcher.path().to_owned()));
    let mut session = session_keeping(&state, Some(Rc::clone(&apps)));
    let workspace = session
        .handle()
        .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
        .expect("a workspace is in front");
    let launch = tilo_app_launch::Launch::Package {
        app: PROBE.into(),
        open: None,
    };
    apps.start(&launch, &apps.workspace_named(&workspace))
        .expect("the launch is taken");
    until(&mut session, "the app is started", |_| starts(&started) > 0);

    // Nothing asks for it again, and the window it opens fills the tab the person left it in.
    for _ in 0..100 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    assert_eq!(starts(&started), 1, "the app started once");
    session
        .desktop()
        .with_policy(|policy| policy.map(window(11, PROBE)))
        .expect("the window maps");
    until(&mut session, "the window takes the saved tab", |session| {
        matches!(
            tile_of(session, device.id(100)),
            Some(TileContent::Foreign { surface_id: 11, .. })
        )
    });
    assert_eq!(
        tabs(&session),
        [(device.id(100), device.id(10))],
        "one tab, not two"
    );
}

#[test]
fn a_launch_while_the_saved_window_is_coming_does_not_start_the_app_again() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("started");
    let launcher = installed_probe(&mut home, &started);
    let (device, _) = Device::with_layout(&mut home, &one_window(PROBE, "Probe"));
    let state = device.state.clone();

    // The restore asks for the saved window first, and the same app is asked for again while its
    // window is coming: a record names the process that already has the app's records, so no second
    // one starts.
    let apps = apps_at(&home, Some(launcher.path().to_owned()));
    let mut session = session_keeping(&state, Some(Rc::clone(&apps)));
    until(&mut session, "the restore asks for the window", |_| {
        starts(&started) == 1
    });
    let workspace = session
        .handle()
        .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
        .expect("a workspace is in front");
    let record =
        tilo_app_launch::Launch::parse("record:so.tilo.probe:notes").expect("a record launch");
    apps.start(&record, &apps.workspace_named(&workspace))
        .expect("the launch is taken");
    for _ in 0..100 {
        session.step(Duration::from_millis(1)).expect("step");
    }
    assert_eq!(starts(&started), 1, "the app is not started a second time");

    session
        .desktop()
        .with_policy(|policy| policy.map(window(11, PROBE)))
        .expect("the window maps");
    until(&mut session, "the window takes the saved tab", |session| {
        matches!(
            tile_of(session, device.id(100)),
            Some(TileContent::Foreign { surface_id: 11, .. })
        )
    });
    assert_eq!(
        tabs(&session),
        [(device.id(100), device.id(10))],
        "one tab, not two"
    );
}

#[test]
fn a_window_whose_app_is_gone_says_so_in_its_tab_and_that_tab_closes() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let device = Device::in_home(&mut home);
    save_layout(&device.state, &one_window(GONE, "Gone"));

    let mut session = session_keeping(&device.state, Some(apps_at(&home, None)));
    until(&mut session, "the tab says why it is empty", |session| {
        note_of(session, device.id(100)).is_some_and(|note| !note.is_empty())
    });
    let note = note_of(&session, device.id(100)).expect("the tab holds the app it was");
    assert!(
        note.contains("Gone"),
        "the tab names the app it holds: {note}"
    );
    assert!(
        note.len() > "Gone".len(),
        "and says why it is not here: {note}"
    );
    // The layout is read off the loop, so the tab it lays in is drawn on the frame after.
    until(
        &mut session,
        "the tab names the app where its window would be",
        |session| {
            session
                .accessible_labels()
                .iter()
                .any(|label| label == "Gone")
        },
    );
    assert!(
        session
            .desktop()
            .with_policy(|policy| policy.app_tabs_in_front().is_empty()),
        "and no window stands in its place"
    );
    assert_eq!(
        tabs(&session),
        [(device.id(100), device.id(10))],
        "nothing else on the layout changed"
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

fn two_workspaces() -> Layout {
    let mut away = Workspace::new(WorkspaceId::from_u128(2), "Away");
    away.strips = vec![Strip::new(StripId(3), "Later")];
    let mut tree = on_shell(vec![Strip::new(StripId(1), "Main")]);
    tree.workspaces.push(away);
    tree
}

/// The workspace the tab `id` stands in.
pub(crate) fn workspace_of(session: &Session, id: TabId) -> Option<WorkspaceId> {
    session.handle().read(|tree| {
        tree.workspaces
            .iter()
            .find(|workspace| {
                workspace.strips.iter().any(|strip| {
                    strip
                        .window_groups
                        .iter()
                        .any(|group| group.tabs.iter().any(|tab| tab.id == id))
                })
            })
            .map(tilo_desktop_model::Workspace::id)
    })
}

/// The app's window on the shell of the workspace in front, mapped as the compositor would have it:
/// what the person is looking at.
fn showing(session: &mut Session, apps: &Apps, started: &Path) -> TabId {
    let workspace = session
        .handle()
        .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
        .expect("a workspace in front");
    apps.start(
        &tilo_app_launch::Launch::Package {
            app: PROBE.into(),
            open: None,
        },
        &apps.workspace_named(&workspace),
    )
    .expect("the layout starts the app");
    until(session, "the app is started", |_| starts(started) == 1);
    session
        .desktop()
        .with_policy(|policy| policy.map(window(11, PROBE)))
        .expect("the window maps");
    until(session, "the window is in its tab", |session| {
        tabs(session).len() == 1
            && matches!(
                tile_of(session, tabs(session)[0].0),
                Some(TileContent::Foreign { surface_id: 11, .. })
            )
    });
    tabs(session)[0].0
}

/// A window the person moves to another workspace runs its app there: the app is started again in
/// the workspace they carried it to, under that workspace's grants, and the window it opens takes
/// the tab they moved.
#[test]
fn a_window_moved_to_another_workspace_runs_its_app_there() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("started");
    let arguments = home.home().join("arguments");
    installed_probe_package(&mut home);
    let launcher = recording_launcher(&home, &started, &arguments);
    let (device, workspace_ids) = Device::with_layout(&mut home, &two_workspaces());
    let state = device.state;

    let apps = apps_at(&home, Some(launcher.path().to_owned()));
    let mut session = session_keeping(&state, Some(Rc::clone(&apps)));
    let window_tab = showing(&mut session, &apps, &started);
    let (away, away_id) = session
        .handle()
        .read(|tree| {
            let away = tree
                .workspaces
                .iter()
                .find(|held| held.id() != workspace_ids[0])?;
            Some((away.id(), away.id().to_string()))
        })
        .expect("a workspace to move it to");

    session
        .handle()
        .choose_window_menu(window_tab, &move_to_workspace_id(away), None)
        .expect("the person moves the window");

    until(&mut session, "the app runs in the other workspace", |_| {
        starts(&started) == 2
    });
    assert_eq!(
        handed(&arguments, "--workspace-id"),
        away_id,
        "the app runs under the grants of the workspace the person moved it to"
    );
    assert_eq!(
        workspace_of(&session, window_tab),
        Some(away),
        "the tab is where the person put it"
    );
    // The window the app opens there takes the tab the person moved.
    session
        .desktop()
        .with_policy(|policy| policy.map(window(12, PROBE)))
        .expect("the window of the app in the other workspace maps");
    until(&mut session, "the new window takes the tab", |session| {
        matches!(
            tile_of(session, window_tab),
            Some(TileContent::Foreign { surface_id: 12, .. })
        )
    });
    assert!(
        session.notifications().feed().banners.is_empty(),
        "nobody else is in either workspace, so nobody is asked"
    );
}

/// Moving a window into a workspace other people are in changes what they can see: the person is
/// asked first, nothing moves until they answer, and the app runs in the shared workspace only once
/// they say so.
#[test]
fn moving_a_window_into_a_shared_workspace_asks_first() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("started");
    let arguments = home.home().join("arguments");
    installed_probe_package(&mut home);
    let launcher = recording_launcher(&home, &started, &arguments);
    let device = Device::in_home(&mut home);
    let apps = apps_at(&home, Some(launcher.path().to_owned()));
    let mut session = session_keeping(&device.state, Some(Rc::clone(&apps)));
    session.step(Duration::from_millis(1)).expect("step");
    // Someone else is in the workspace the person moves the window to: it is another person's, and
    // this device holds no governance of it.
    let mut away = Workspace::new(WorkspaceId::new(), "Away");
    away.set_owner("bb".repeat(32));
    let away_id = away.id().to_string();
    let workspace_ids = [session.handle().read(|tree| tree.active), away.id()];
    let mut layout = session.handle().read(Clone::clone);
    layout.workspaces.push(away);
    session.handle().replace_tree(layout);
    let window_tab = showing(&mut session, &apps, &started);

    session
        .handle()
        .choose_window_menu(window_tab, &move_to_workspace_id(workspace_ids[1]), None)
        .expect("the person moves the window");
    until(&mut session, "the person is asked", |session| {
        session.notifications().feed().banners.iter().any(|banner| {
            banner
                .actions
                .iter()
                .any(|(key, _)| key == "window-move-carry")
        })
    });
    let ask = session
        .notifications()
        .feed()
        .banners
        .into_iter()
        .find(|banner| {
            banner
                .actions
                .iter()
                .any(|(key, _)| key == "window-move-carry")
        })
        .expect("the notice asking about the move");
    assert!(
        ask.actions.iter().any(|(key, _)| key == "window-move-stay"),
        "the person may leave the window where it is: {:?}",
        ask.actions
    );
    assert!(
        ask.notification.title.contains("Away"),
        "the notice names the workspace: {:?}",
        ask.notification.title
    );
    assert_eq!(
        workspace_of(&session, window_tab),
        Some(workspace_ids[0]),
        "nothing moves until they answer"
    );
    assert_eq!(starts(&started), 1, "and nothing starts");

    session
        .notifications()
        .invoke_action(ask.notification.id, "window-move-carry");
    until(&mut session, "the app runs in the shared workspace", |_| {
        starts(&started) == 2
    });
    assert_eq!(handed(&arguments, "--workspace-id"), away_id);
    assert_eq!(workspace_of(&session, window_tab), Some(workspace_ids[1]));
}

/// The file the device's store keeps the session's document `name` in under `state`, once what the
/// session handed the store is written.
fn kept_file(state: &std::path::Path, name: &str) -> std::path::PathBuf {
    use durable_store::DurableStore as _;
    let root = state.join("tilo").join("documents");
    app_state::shared_store(&root)
        .flush(None)
        .expect("what the session keeps is written");
    root.join(name).join(app_state::DOCUMENT_FILE)
}

/// The launcher that records what each start is handed as well: its pid, the tab the window is
/// told it takes, and the item it opens on.
fn item_launcher(
    home: &tilo_test_env::TestHome,
    started: &Path,
    instances: &Path,
    reveals: &Path,
) -> tilo_test_env::StandIn {
    tilo_test_env::stand_in(
        home.home().join("tilo-launcher"),
        &format!(
            "printf '%s\\n' \"$$\" >> '{started}'\n\
             printf '%s\\n' \"${{TILO_CONTINUITY_INSTANCE:-}}\" >> '{instances}'\n\
             printf '%s\\n' \"${{{reveal}:-}}\" >> '{reveals}'\n\
             hold\n",
            started = started.display(),
            instances = instances.display(),
            reveals = reveals.display(),
            reveal = ui::window_link::REVEAL_ENV,
        ),
    )
    .unwrap()
}

/// A window a send opened on an item (a conversation, a record) is a window, not a place of a
/// view: after a restart its app is asked for it again, and it opens on that same item.
#[test]
fn a_window_a_send_opened_on_an_item_opens_there_again_after_a_restart() {
    const ITEM: &str = "probe:conversation/7";
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("started");
    let instances = home.home().join("instances");
    let reveals = home.home().join("reveals");
    installed_probe_package(&mut home);
    let launcher = item_launcher(&home, &started, &instances, &reveals);
    let state = Device::in_home(&mut home).state;

    // The send opens a window of the app on the conversation, beside the window that sent it.
    let tab = {
        let apps = apps_at(&home, Some(launcher.path().to_owned()));
        let mut session = session_keeping(&state, Some(Rc::clone(&apps)));
        assert!(
            apps.ask_window(
                &tilo_app_launch::Launch::Package {
                    app: PROBE.into(),
                    open: None,
                },
                None,
                false,
                true,
                Some(ITEM),
            ),
            "the shell holds the tab the conversation's window takes"
        );
        until(&mut session, "the app is started", |_| {
            starts(&started) == 1
        });
        let tab = tabs(&session).first().map(|(tab, _)| *tab).expect("a tab");
        session
            .desktop()
            .with_policy(|policy| policy.map(window(11, PROBE)))
            .expect("the window maps");
        until(&mut session, "the window is in its tab", |session| {
            matches!(
                tile_of(session, tab),
                Some(TileContent::Foreign { surface_id: 11, .. })
            )
        });
        tab
    };

    // The next session asks the app for that window again, and on the item it opened.
    let mut next = session_keeping(
        &state,
        Some(apps_at(&home, Some(launcher.path().to_owned()))),
    );
    until(&mut next, "the app is asked for its window again", |_| {
        starts(&started) == 2
    });
    let told: Vec<String> = fs::read_to_string(&instances)
        .expect("the launcher recorded its starts")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        told.last().map(String::as_str),
        Some(tab.to_string().as_str()),
        "the window takes the tab it was in"
    );
    let shown: Vec<String> = fs::read_to_string(&reveals)
        .expect("the launcher recorded the items")
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        shown.last().map(String::as_str),
        Some(ITEM),
        "the window opens on the item it showed"
    );
}
