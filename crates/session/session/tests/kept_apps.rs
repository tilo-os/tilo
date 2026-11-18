//! An app that is not Tilo's runs behind a keeper and stays running while the desktop restarts in
//! place: the session that follows takes it back, and each of its windows returns to the tab it
//! stood in, in the workspace the person had moved it to. A logout ends it, and so does a start that follows
//! no restart, and a restart whose keeper stopped answering: nobody would take such an app back.
//!
//! Everything is real but the launcher: the session's own spawner and restore, a keeper (this
//! test binary, run as one), and the app (this test binary again, a Wayland client of the keeper).

#![cfg(target_os = "linux")]

use std::cell::Cell;
use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tilo_app_launch::Teardown;
use tilo_app_launch::kept::testing;
use tilo_desktop_model::{Layout, Strip, StripId, TabId, TileContent, Workspace, WorkspaceId};
use tilo_session::apps::Apps;
use tilo_session::{AppActions, Options, Session};

use tilo_desktop_surface::surface::window_menu::MOVE_TO_WORKSPACE;

use crate::common::window::{Client, KEY_A, WINDOW_BYTES};
use crate::layout::{Device, tabs, tile_of, until, workspace_of};

/// The kept app, by its desktop-file id and the id its windows announce.
const APP: &str = "org.example.Kept";

/// What makes this test binary the kept app: the directory it reads its windows from and reports
/// in.
const APP_DIR: &str = "TILO_TEST_KEPT_APP";

/// Not a test: the keeper a kept launch starts, as `tilo-app-keeper` would run.
#[test]
fn keeper_process() {
    testing::serve_keeper().expect("the keeper ran");
}

/// Set when the app is asked to close its last window.
static CLOSE: AtomicBool = AtomicBool::new(false);

extern "C" fn close_one(_: libc::c_int) {
    CLOSE.store(true, Ordering::Release);
}

/// Not a test: the app a kept launch starts. It opens one window for each line of `titles` in its
/// directory, writes its pid to `started` as it starts and the number of keys it was delivered to
/// `keys`, and closes its last window on `SIGUSR1`. It ends with its keeper.
#[test]
fn kept_app() {
    let Ok(dir) = std::env::var(APP_DIR) else {
        return;
    };
    let dir = PathBuf::from(dir);
    let titles: Vec<String> = fs::read_to_string(dir.join("titles"))
        .expect("the app's windows")
        .lines()
        .map(str::to_owned)
        .collect();
    let mut started = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("started"))
        .expect("the starts");
    writeln!(started, "{}", std::process::id()).expect("the start recorded");
    // SAFETY: the handler only stores into an atomic.
    unsafe {
        libc::signal(libc::SIGUSR1, close_one as *const () as libc::sighandler_t);
    }
    let socket = std::env::var("WAYLAND_DISPLAY").expect("the keeper's display");
    let mut client = Client::on(Path::new(&socket), APP, &titles[0]);
    for title in &titles[1..] {
        let index = client.open_toplevel_with(APP, |toplevel| toplevel.set_title(title.clone()));
        while !client.seen.ready(index) {
            client.exchange();
            std::thread::sleep(Duration::from_millis(5));
        }
        let (width, height) = client.seen.size_at(index).unwrap_or((320, 200));
        client.show(index, width, height, WINDOW_BYTES);
    }
    let mut open = titles.len();
    let mut keys = 0;
    loop {
        client.exchange();
        if CLOSE.swap(false, Ordering::AcqRel) && open > 1 {
            open -= 1;
            client.close(open);
        }
        let pressed = client.seen.keys.iter().filter(|(_, down)| *down).count();
        if pressed != keys {
            keys = pressed;
            fs::write(dir.join("keys"), keys.to_string()).expect("the keys recorded");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A device whose layout has three workspaces, the launcher that starts the kept app with the
/// windows `titles` names, and the keepers' registry.
struct Rig {
    home: tilo_test_env::TestHome,
    device: Device,
    /// The device's workspaces: the first is in front.
    workspaces: Vec<WorkspaceId>,
    /// The kept app's directory: what it opens, and what it reports.
    app: PathBuf,
    keeper: PathBuf,
    root: PathBuf,
}

/// One run of the desktop: its session, its apps, and how it ends when dropped.
struct Run {
    session: Session,
    apps: Rc<Apps>,
    ending: Rc<Cell<Teardown>>,
}

impl Run {
    /// End the run as `teardown` does.
    fn end(self, teardown: Teardown) {
        self.ending.set(teardown);
        let Self { session, apps, .. } = self;
        drop(session);
        drop(apps);
    }
}

impl Rig {
    fn new(titles: &[&str]) -> Self {
        let mut home = tilo_test_env::TestHome::new().expect("a home");
        let workspaces = [(1, "Home"), (2, "Two"), (3, "Three")].map(|(n, name)| {
            let mut workspace = Workspace::new(WorkspaceId::from_u128(n), name);
            workspace.strips = vec![Strip::new(StripId(u64::try_from(n).unwrap()), name)];
            workspace
        });
        let tree = Layout {
            active: workspaces[0].id(),
            workspaces: workspaces.into(),
            ..Layout::default()
        };
        let (device, workspaces) = Device::with_layout(&mut home, &tree);
        let app = home.home().join("app");
        fs::create_dir_all(&app).unwrap();
        fs::write(app.join("titles"), titles.join("\n")).unwrap();
        let applications = home.data().join("applications");
        fs::create_dir_all(&applications).unwrap();
        fs::write(
            applications.join(format!("{APP}.desktop")),
            "[Desktop Entry]\nType=Application\nName=Kept\nExec=/bin/true\n",
        )
        .unwrap();
        desktop_entry::cache::invalidate();
        let keeper = home.home().join("keeper");
        testing::keeper_program(&keeper, "kept_apps::keeper_process").unwrap();
        let exe = std::env::current_exe().unwrap();
        testing::launcher_program(
            &home.home().join("tilo-launcher"),
            &[
                "/usr/bin/env".to_owned(),
                format!("{APP_DIR}={}", app.display()),
                exe.to_string_lossy().into_owned(),
                "--exact".to_owned(),
                "kept_apps::kept_app".to_owned(),
                "--nocapture".to_owned(),
            ],
        )
        .unwrap();
        let root = home.runtime().join("kept");
        Self {
            home,
            device,
            workspaces,
            app,
            keeper,
            root,
        }
    }

    /// A run of the desktop, taking up what the keepers hold as the session's `main` does:
    /// `following` an in-place restart, or a start of its own.
    fn run(&self, following: bool) -> Run {
        let mut environment = tilo_app_launch::SessionEnvironment::from_process();
        environment.wayland_socket = Some(self.home.runtime().join("wayland-7"));
        let apps = Rc::new(Apps::with_launcher(
            environment,
            None,
            Some(self.home.home().join("tilo-launcher")),
        ));
        let ending = Rc::new(Cell::new(Teardown::Logout));
        let asked = Rc::clone(&ending);
        let mut session = crate::common::headless(Options {
            collab: false,
            apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
            launch: Some(Rc::clone(&apps)),
            state: Some(self.device.state.clone()),
            teardown: Box::new(move || asked.get()),
            ..Options::default()
        });
        let connector = session.compositor().client_connector();
        apps.keep_behind(tilo_app_launch::kept::Keeping {
            keeper: self.keeper.clone(),
            root: self.root.clone(),
            desktop: Arc::new(move |pid| connector.connect_for(pid)),
        });
        apps.take_up_kept(following);
        session.step(Duration::from_millis(1)).expect("step");
        Run {
            session,
            apps,
            ending,
        }
    }

    /// Start the app in the workspace in front.
    fn launch(&self, run: &Run) {
        let workspace = run
            .session
            .handle()
            .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
            .expect("a workspace in front");
        run.apps
            .start(
                &tilo_app_launch::Launch::Host {
                    desktop_id: APP.to_owned(),
                    open: None,
                },
                &run.apps.workspace_named(&workspace),
            )
            .expect("the app starts");
    }

    /// The pids of the app's starts.
    fn starts(&self) -> Vec<u32> {
        fs::read_to_string(self.app.join("started"))
            .unwrap_or_default()
            .lines()
            .filter_map(|line| line.parse().ok())
            .collect()
    }

    /// How many keys the app was delivered.
    fn keys(&self) -> usize {
        fs::read_to_string(self.app.join("keys"))
            .ok()
            .and_then(|keys| keys.parse().ok())
            .unwrap_or_default()
    }

    /// The one keeper's directory and the app it keeps, by pid and start time.
    fn kept(&self) -> (PathBuf, u32, u64) {
        let described = testing::described(&self.root);
        let [(dir, description)] = described.as_slice() else {
            panic!("one keeper answers: {described:?}");
        };
        (
            dir.clone(),
            description.app_pid.expect("the app runs"),
            description.started_at.expect("its start is known"),
        )
    }

    /// The tab each of the app's windows the keeper was last told it stands in, by title.
    fn told(&self) -> Vec<(String, Option<String>)> {
        let mut told: Vec<(String, Option<String>)> = testing::described(&self.root)
            .into_iter()
            .flat_map(|(_, description)| description.windows)
            .map(|window| (window.title.unwrap_or_default(), window.placement))
            .collect();
        told.sort();
        told
    }
}

/// The tab showing the window titled `title`, once one does.
fn tab_showing(session: &mut Session, title: &str) -> Option<TabId> {
    let window = session
        .compositor()
        .windows()
        .into_iter()
        .find(|(info, _)| info.title.as_deref() == Some(title))?
        .0
        .id;
    tabs(session).into_iter().map(|(tab, _)| tab).find(|tab| {
        matches!(
            tile_of(session, *tab),
            Some(TileContent::Foreign { surface_id, .. }) if surface_id == window
        )
    })
}

/// Wait until every window `titles` names stands in a tab, and the keeper was told each one: the
/// tabs by title.
fn shown_and_told(rig: &Rig, run: &mut Run, titles: &[&str]) -> Vec<(String, TabId)> {
    let mut placed = Vec::new();
    crate::common::wait_for(&mut run.session, "every window is in a tab", |session| {
        placed = titles
            .iter()
            .map(|title| Some(((*title).to_owned(), tab_showing(session, title)?)))
            .collect::<Option<Vec<_>>>()
            .unwrap_or_default();
        (placed.len() == titles.len()).then_some(())
    });
    let mut expected: Vec<(String, Option<String>)> = placed
        .iter()
        .map(|(title, tab)| (title.clone(), Some(tab.to_string())))
        .collect();
    expected.sort();
    until(&mut run.session, "the keeper is told each tab", |_| {
        rig.told() == expected
    });
    placed
}

/// Send the window in `tab` to the workspace `to`, as its window menu does, and wait until it
/// stands there.
fn send(session: &mut Session, tab: TabId, to: WorkspaceId) {
    session
        .handle()
        .choose_window_menu(tab, &format!("{MOVE_TO_WORKSPACE}:{to}"), None)
        .expect("the person moves the window");
    until(session, "the window is in the other workspace", |session| {
        workspace_of(session, tab) == Some(to)
    });
}

/// Where each window stands, by title: its tab and the workspace of that tab.
fn places(
    session: &Session,
    shown: &[(String, TabId)],
) -> Vec<(String, TabId, Option<WorkspaceId>)> {
    shown
        .iter()
        .map(|(title, tab)| (title.clone(), *tab, workspace_of(session, *tab)))
        .collect()
}

/// Whether `pid` is the process that started at `at`.
fn running(pid: u32, at: u64) -> bool {
    let Ok(stat) = fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return false;
    };
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().nth(19))
        .and_then(|field| field.parse::<u64>().ok())
        == Some(at)
}

/// The process that started `pid`.
fn parent_of(pid: u32) -> u32 {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).expect("the process runs");
    stat.rsplit_once(')')
        .and_then(|(_, rest)| rest.split_whitespace().nth(1))
        .and_then(|field| field.parse().ok())
        .expect("its parent")
}

fn signal(pid: u32, signal: libc::c_int) {
    // SAFETY: kill takes a pid and a signal number.
    let sent = unsafe { libc::kill(libc::pid_t::try_from(pid).unwrap(), signal) };
    assert_eq!(sent, 0, "the signal reaches {pid}");
}

#[test]
fn a_kept_app_stays_through_an_in_place_restart_in_its_tabs_and_a_logout_ends_it() {
    let _settled = ui::motion::testing::settled();
    let titles = ["First", "Second", "Third"];
    let rig = Rig::new(&titles);
    let mut first = rig.run(false);
    rig.launch(&first);
    let placed = shown_and_told(&rig, &mut first, &titles);
    let (dir, pid, at) = rig.kept();
    // Each window to a workspace of its own, none of which the person goes to: the last one just
    // before the desktop restarts, with no look after it.
    send(&mut first.session, placed[1].1, rig.workspaces[1]);
    assert_eq!(
        shown_and_told(&rig, &mut first, &titles),
        placed,
        "a window keeps its tab as it changes workspace"
    );
    send(&mut first.session, placed[2].1, rig.workspaces[2]);
    let placed = places(&first.session, &placed);
    assert_eq!(
        placed.iter().map(|(_, _, at)| *at).collect::<Vec<_>>(),
        rig.workspaces.iter().copied().map(Some).collect::<Vec<_>>(),
        "a window in each workspace"
    );

    first.end(Teardown::InPlaceRestart);
    assert!(
        running(pid, at),
        "the app outlives the desktop that started it"
    );

    let mut second = rig.run(true);
    let back = shown_and_told(&rig, &mut second, &titles);
    assert_eq!(
        places(&second.session, &back),
        placed,
        "each window is back in the tab and the workspace it stood in"
    );
    assert_eq!(tabs(&second.session).len(), 3, "no other tab opened");
    assert_eq!(rig.kept(), (dir.clone(), pid, at), "it is the same process");
    assert_eq!(rig.starts(), [pid], "the app was not started again");

    // The window in front is the person's again: a key reaches it.
    assert!(second.session.handle().focus_tab(back[0].1));
    crate::common::wait_for(
        &mut second.session,
        "the window has the keyboard",
        |session| {
            session
                .compositor()
                .focused_window()
                .is_some_and(|window| window.title.as_deref() == Some("First"))
                .then_some(())
        },
    );
    crate::common::window::tap_key(&mut second.session, KEY_A);
    until(&mut second.session, "the app hears the key", |_| {
        rig.keys() == 1
    });

    second.end(Teardown::Logout);
    tilo_test_env::exited("the kept app at logout", pid);
    assert!(!dir.exists(), "a logout removes the keeper's directory");
}

/// The command line the session starts an app that is not Tilo's with is one the real launcher
/// takes: asked to plan the launch rather than make it, the launcher names the app the session
/// asked for. A launcher that answers to another command opens no such app at all.
#[test]
fn the_launcher_takes_the_command_line_the_session_starts_an_app_with() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]);
    let mut first = rig.run(false);
    rig.launch(&first);
    let stand_in = rig.home.home().join("tilo-launcher");
    until(&mut first.session, "the launcher was started", |_| {
        !testing::launcher_handed(&stand_in).is_empty()
    });
    let handed = testing::launcher_handed(&stand_in);
    let launcher = tilo_test_env::sibling_binary("launcher", "tilo-launcher");
    let nowhere = rig.home.home().join("no-other-applications");
    fs::create_dir_all(&nowhere).unwrap();
    let planned = std::process::Command::new(&launcher)
        .args(&handed)
        .arg("--dry-run")
        .env("XDG_DATA_HOME", rig.home.data())
        .env("XDG_DATA_DIRS", &nowhere)
        .output()
        .expect("the launcher answers");
    let said = String::from_utf8_lossy(&planned.stdout);
    assert!(
        planned.status.success(),
        "{handed:?}: {}",
        String::from_utf8_lossy(&planned.stderr)
    );
    assert!(said.contains(&format!("desktop-id : {APP}")), "{said}");
    first.end(Teardown::Logout);
}

#[test]
fn a_start_that_follows_no_restart_ends_what_a_restart_left() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]);
    let mut first = rig.run(false);
    rig.launch(&first);
    shown_and_told(&rig, &mut first, &["Only"]);
    let (dir, pid, _) = rig.kept();
    first.end(Teardown::InPlaceRestart);

    let second = rig.run(false);
    tilo_test_env::exited("an app a restart left that no run takes back", pid);
    assert!(!dir.exists(), "and its keeper's directory is gone");
    second.end(Teardown::Logout);
}

#[test]
fn a_window_the_app_closed_while_no_desktop_was_there_closes_its_tab() {
    let _settled = ui::motion::testing::settled();
    let titles = ["Stays", "Closes"];
    let rig = Rig::new(&titles);
    let mut first = rig.run(false);
    rig.launch(&first);
    let placed = shown_and_told(&rig, &mut first, &titles);
    let (_, pid, _) = rig.kept();
    first.end(Teardown::InPlaceRestart);

    signal(pid, libc::SIGUSR1);
    tilo_test_env::wait_until("the app closed its window", || rig.told().len() == 1);

    let mut second = rig.run(true);
    let back = shown_and_told(&rig, &mut second, &["Stays"]);
    assert_eq!(back[0], placed[0], "the window it kept is back in its tab");
    until(
        &mut second.session,
        "the closed window's tab closes",
        |session| tabs(session).len() == 1,
    );
    for _ in 0..100 {
        second.session.step(Duration::from_millis(1)).expect("step");
    }
    assert_eq!(tabs(&second.session).len(), 1, "no tab opens for it again");
    assert_eq!(rig.starts(), [pid], "and no second copy of the app starts");
    second.end(Teardown::Logout);
}

#[test]
fn a_restart_ends_an_app_whose_keeper_stopped_answering() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]);
    let mut first = rig.run(false);
    rig.launch(&first);
    shown_and_told(&rig, &mut first, &["Only"]);
    let (dir, pid, _) = rig.kept();
    let keeper = parent_of(pid);
    signal(keeper, libc::SIGSTOP);

    first.end(Teardown::InPlaceRestart);
    assert!(
        !dir.exists(),
        "a keeper that does not answer is not left for the next run"
    );
    signal(keeper, libc::SIGCONT);
    tilo_test_env::exited("the app of a keeper that stopped answering", pid);
}

#[test]
fn an_app_from_before_an_update_says_so_in_its_tab_and_restart_opens_it_again() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]);
    let mut first = rig.run(false);
    rig.launch(&first);
    let placed = shown_and_told(&rig, &mut first, &["Only"]);
    let (dir, pid, at) = rig.kept();
    first.end(Teardown::InPlaceRestart);
    // The update changed how the desktop speaks to the app's keeper.
    testing::age(&dir, true).expect("aged");

    let mut second = rig.run(true);
    crate::common::wait_for(
        &mut second.session,
        "its tab says it is from before the update",
        |session| crate::common::control_on_screen(session, "Restart").map(|_| ()),
    );
    assert_eq!(tabs(&second.session).len(), 1, "it says so in its own tab");
    assert_eq!(tabs(&second.session)[0].0, placed[0].1, "the tab it was in");
    assert_eq!(rig.starts(), [pid], "no second copy started beside it");
    assert!(running(pid, at), "it goes on with what it holds");

    crate::common::press_control(&mut second.session, "Restart");
    until(
        &mut second.session,
        "the old app ended and a new one started",
        |_| !running(pid, at) && rig.starts().len() == 2,
    );
    let back = shown_and_told(&rig, &mut second, &["Only"]);
    assert_eq!(
        back[0].1,
        placed[0].1,
        "the new one opens in the same tab: {:?}",
        tabs(&second.session)
    );
    assert_eq!(tabs(&second.session).len(), 1, "and nowhere else");
    assert!(!dir.exists(), "the old keeper's directory is gone");
    second.end(Teardown::Logout);
}

#[test]
fn a_logout_ends_an_app_from_before_an_update() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]);
    let mut first = rig.run(false);
    rig.launch(&first);
    shown_and_told(&rig, &mut first, &["Only"]);
    let (dir, pid, _) = rig.kept();
    first.end(Teardown::InPlaceRestart);
    testing::age(&dir, true).expect("aged");
    let mut second = rig.run(true);
    crate::common::wait_for(
        &mut second.session,
        "its tab says it is from before the update",
        |session| crate::common::control_on_screen(session, "Restart").map(|_| ()),
    );
    second.end(Teardown::Logout);
    tilo_test_env::exited("the app from before the update at logout", pid);
    assert!(!dir.exists());
}

#[test]
fn an_app_from_before_an_update_that_left_no_tab_says_so_in_a_tab_of_its_own() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]);
    let mut first = rig.run(false);
    rig.launch(&first);
    shown_and_told(&rig, &mut first, &["Only"]);
    let (dir, pid, at) = rig.kept();
    first.end(Teardown::InPlaceRestart);
    testing::age(&dir, true).expect("aged");
    // Its keeper names an app no saved tab shows: the tab it had is gone.
    let launch = tilo_app_launch::kept::KeptLaunch {
        version: tilo_app_launch::kept::LAUNCH_VERSION,
        app: "org.example.Elsewhere".into(),
        title: "Elsewhere".into(),
        workspace: String::new(),
        asked: None,
        window_identities: Vec::new(),
        service: false,
    };
    fs::write(dir.join("launch"), launch.encode()).expect("the launch rewritten");

    let mut second = rig.run(true);
    crate::common::wait_for(&mut second.session, "a tab of its own says so", |session| {
        crate::common::control_on_screen(session, "Restart").map(|_| ())
    });
    assert!(
        second
            .session
            .accessible_labels()
            .iter()
            .any(|label| label == "Elsewhere is still running from before the update"),
        "{:?}",
        second.session.accessible_labels()
    );
    assert!(running(pid, at), "it goes on with what it holds");
    second.end(Teardown::Logout);
    tilo_test_env::exited("the app from before the update at logout", pid);
}
