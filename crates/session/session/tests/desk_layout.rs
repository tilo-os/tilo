//! The desk comes back after a login: its strips, its groups and their widths, the window that was
//! in front — and the windows the person left open, each asked for again into the tab it was in. A
//! window whose app is gone says so in its tab and closes; a desk this build cannot read starts
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

use tilo_action_policy::Role;
use tilo_desktop_model::{
    DesktopTree, GroupId, Strip, StripId, Tab, TabId, TileContent, WindowGroup, WindowWidth,
    Workspace, WorkspaceSlot,
};
use tilo_desktop_policy::Window;
use tilo_desktop_surface::notifications::NotificationSeverity;
use tilo_desktop_surface::surface::window_menu::MOVE_TO_WORKSPACE;
use tilo_session::apps::Apps;
use tilo_session::{AppActions, Options, Session};

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

/// The person at this device, whose desk its sessions open.
const PERSON: &str = "ada";

/// Write `tree` as the desk this device left, where the next session reads it: the document a
/// session that ends keeps, written the way that session writes it.
fn save_desk(state: &Path, tree: &DesktopTree) {
    let desk = tilo_session_core::desk::Desk::new(automerge::AutoCommit::new(), PERSON, None);
    desk.record_now(tree, tilo_clock::unix_now_i64(), &[], None);
    let path = tilo_session_core::desk::path(state);
    fs::create_dir_all(path.parent().expect("a tilo directory")).expect("the state directory");
    fs::write(path, desk.document().save()).expect("the desk this device left");
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
fn on_desk(strips: Vec<Strip>) -> DesktopTree {
    let mut home = Workspace::new(WorkspaceSlot(1), "Home");
    home.strips = strips;
    let mut tree = DesktopTree {
        active: home.slot,
        workspaces: vec![home],
        ..DesktopTree::default()
    };
    tree.active_strip.insert(WorkspaceSlot(1), StripId(1));
    tree.set_active_group(StripId(1), 10);
    tree
}

/// A desk with two strips of two groups in one workspace, and a second workspace behind it: each
/// strip and group carrying what the person chose.
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

    let mut away = Workspace::new(WorkspaceSlot(2), "Away");
    away.strips = vec![Strip::new(StripId(3), "Later")];

    let mut tree = on_desk(vec![writing, reference]);
    tree.workspaces.push(away);
    tree
}

/// A desk holding one saved window of `app` , alone in its group.
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

/// A session on the desk this device saved, starting `apps` when its windows are asked for. `None`
/// : a session that starts nothing at all.
fn session_keeping(state: &Path, apps: Option<Rc<Apps>>) -> Session {
    crate::common::headless(Options {
        collab: false,
        apps: apps
            .as_ref()
            .map(|apps| Rc::clone(apps) as Rc<dyn AppActions>),
        launch: apps,
        state: Some(state.to_path_buf()),
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
fn until(session: &mut Session, what: &str, mut done: impl FnMut(&Session) -> bool) {
    crate::common::wait_for(session, what, |session| done(session).then_some(()));
}

/// Every tab of the desk, with the group it sits in: what a second window or a second tab of a
/// restored one would change.
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
                tree.active_strip.get(&WorkspaceSlot(1)).copied(),
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

/// What a restart brings back is the desk the session itself kept: a strip renamed in one session
/// is that strip in the next, at the width it was left.
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
        tilo_session_core::desk::path(&state).exists(),
        "the desk is kept as its one document"
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
    let launcher = installed_probe(&mut home, &started);
    save_desk(&state_of(&home), &one_window(PROBE, "Probe"));

    let mut session = session_keeping(
        &state_of(&home),
        Some(apps_at(&home, Some(launcher.path().to_owned()))),
    );
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
    save_desk(&state_of(&home), &one_window(PROBE, "Probe"));

    let mut session = session_keeping(
        &state_of(&home),
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
        "100",
        "the app is told the tab its window takes"
    );
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

/// A window the person opened in one session opens again in the next: the desk keeps a running
/// window as the app that opened it, and the next session opens that app, in the tab it was in.
#[test]
fn a_window_the_person_opened_opens_again_after_a_restart() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("started");
    let launcher = installed_probe(&mut home, &started);
    let state = state_of(&home);

    // The person opens the app from the desk, the compositor maps its window into the tab it made,
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
                app: PROBE.into(),
                open: None,
            },
            &apps.workspace_named(&workspace),
        )
        .expect("the desk starts the app");
        until(&mut session, "the app is started", |_| {
            starts(&started) == 1
        });
        session
            .desktop()
            .with_policy(|policy| policy.map(window(11, PROBE)))
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
        .with_policy(|policy| policy.map(window(12, PROBE)))
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

#[test]
fn a_launch_before_the_restore_asks_starts_the_app_into_the_saved_tab() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("started");
    let launcher = installed_probe(&mut home, &started);
    let state = state_of(&home);
    save_desk(&state, &one_window(PROBE, "Probe"));

    // The session starts on the desk, and the person — or an action reaching the app from somewhere
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
    until(&mut session, "the app is started", |_| {
        starts(&started) == 1
    });

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
            tile_of(session, 100),
            Some(TileContent::Foreign { surface_id: 11, .. })
        )
    });
    assert_eq!(tabs(&session), [(100, 10)], "one tab, not two");
}

#[test]
fn a_launch_while_the_saved_window_is_coming_does_not_start_the_app_again() {
    let _settled = ui::motion::testing::settled();
    let mut home = tilo_test_env::TestHome::new().unwrap();
    let started = home.home().join("started");
    let launcher = installed_probe(&mut home, &started);
    let state = state_of(&home);
    save_desk(&state, &one_window(PROBE, "Probe"));

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
            tile_of(session, 100),
            Some(TileContent::Foreign { surface_id: 11, .. })
        )
    });
    assert_eq!(tabs(&session), [(100, 10)], "one tab, not two");
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
            .with_policy(|policy| policy.app_tabs_in_front().is_empty()),
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
    let path = tilo_session_core::desk::path(&state);
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
        .find(|notice| notice.title == "Your desk could not be read")
        .unwrap_or_else(|| {
            panic!(
                "the person is told their desk could not be read: {:?}",
                shown
                    .iter()
                    .map(|notice| (notice.title.clone(), notice.body.clone()))
                    .collect::<Vec<_>>()
            )
        });
    assert!(
        about.body.contains("unchanged"),
        "and that it is still there as it was: {}",
        about.body
    );
    assert_eq!(
        about.severity,
        NotificationSeverity::Error,
        "and is not one that fades before it is read"
    );
    assert!(
        PathBuf::from(format!("{}.not-opened", path.display())).exists(),
        "the reason is left beside the desk, for anyone not looking at the screen"
    );
    assert_eq!(
        fs::read_to_string(&path).expect("the desk is still there"),
        "{ not a desk",
        "nothing is written over a desk this session did not open"
    );
}

/// A desk with one empty strip in Home, and Away behind it.
fn two_workspaces() -> DesktopTree {
    let mut away = Workspace::new(WorkspaceSlot(2), "Away");
    away.strips = vec![Strip::new(StripId(3), "Later")];
    let mut tree = on_desk(vec![Strip::new(StripId(1), "Main")]);
    tree.workspaces.push(away);
    tree
}

/// The workspace the tab `id` stands in.
fn workspace_of(session: &Session, id: TabId) -> Option<WorkspaceSlot> {
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
            .map(|workspace| workspace.slot)
    })
}

/// The app's window on the desk of the workspace in front, mapped as the compositor would have it:
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
    .expect("the desk starts the app");
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
    let state = state_of(&home);
    save_desk(&state, &two_workspaces());

    let apps = apps_at(&home, Some(launcher.path().to_owned()));
    let mut session = session_keeping(&state, Some(Rc::clone(&apps)));
    let window_tab = showing(&mut session, &apps, &started);
    let (away, away_id) = session
        .handle()
        .read(|tree| {
            let away = tree
                .workspaces
                .iter()
                .find(|held| held.slot != WorkspaceSlot(1))?;
            Some((away.slot, away.id().to_string()))
        })
        .expect("a workspace to move it to");

    session
        .handle()
        .choose_window_menu(window_tab, &format!("{MOVE_TO_WORKSPACE}:{}", away.0), None)
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
    let state = state_of(&home);
    let mut desk = two_workspaces();
    let away = desk
        .workspaces
        .iter_mut()
        .find(|held| held.slot != WorkspaceSlot(1))
        .expect("the workspace behind");
    let away_id = away.id().to_string();
    save_desk(&state, &desk);

    let apps = apps_at(&home, Some(launcher.path().to_owned()));
    let mut session = session_keeping(&state, Some(Rc::clone(&apps)));
    // Someone else is in the workspace the person moves the window to: a workspace's people are its
    // governance's, which this session reads onto its workspaces once it holds them, and the desk
    // it starts from does not carry them.
    let mut desk = session.handle().read(Clone::clone);
    desk.workspaces
        .iter_mut()
        .find(|held| held.slot == WorkspaceSlot(2))
        .expect("the workspace behind")
        .admit("bb".repeat(32), Role::Editor);
    session.handle().replace_tree(desk);
    let window_tab = showing(&mut session, &apps, &started);

    session
        .handle()
        .choose_window_menu(
            window_tab,
            &format!("{MOVE_TO_WORKSPACE}:{}", WorkspaceSlot(2).0),
            None,
        )
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
        Some(WorkspaceSlot(1)),
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
    assert_eq!(workspace_of(&session, window_tab), Some(WorkspaceSlot(2)));
}
