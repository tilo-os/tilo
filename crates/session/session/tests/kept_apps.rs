//! An app that is not Tilo's runs behind a keeper and stays running while the desktop restarts in
//! place: the session that follows takes it back, and each of its windows returns to the tab it
//! stood in, in the workspace the person had moved it to. A logout ends it, and so does a start that follows
//! no restart, and a restart whose keeper is gone or cannot be reached: nobody would take such an app back.
//!
//! Everything is real but the launcher: the session's own spawner and restore, a keeper (this
//! test binary, run as one), and the app (this test binary again, a Wayland client of the keeper).

#![cfg(target_os = "linux")]

use std::cell::Cell;
use std::fmt::Write as _;
use std::fs;
use std::io::Write as _;
use std::os::fd::AsRawFd as _;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use tilo_app_launch::Teardown;
use tilo_app_launch::kept::testing;
use tilo_desktop_model::{Layout, Strip, StripId, TabId, TileContent, Workspace, WorkspaceId};
use tilo_session::apps::Apps;
use tilo_session::{AppActions, Options, Session};
use tilo_test_env::SpawnUnderEnvLock as _;

use tilo_desktop_surface::surface::window_menu::move_to_workspace_id;

use crate::common::window::{Client, KEY_A, WINDOW_BYTES};
use crate::layout::{Device, tabs, tile_of, until, workspace_of};

/// The kept app, by its desktop-file id and the id its windows announce.
const APP: &str = "org.example.Kept";

/// What makes this test binary the kept app: the directory it reads its windows from and reports
/// in.
const APP_DIR: &str = "TILO_TEST_KEPT_APP";

/// A relay nobody answers at: an address of the block kept for documentation, which routes
/// nowhere.
const SILENT_RELAY: &str = "https://192.0.2.1/";

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
/// `keys` (only those its open windows were delivered), and closes its last window on `SIGUSR1`. It ends with its keeper.
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
    // What this process holds beyond its own standard streams: a launch gets only the descriptors
    // it is handed, so a test reads this and looks for what the desktop holds.
    let mut fds = String::new();
    let mut listed: Vec<(i32, String)> = fs::read_dir("/proc/self/fd")
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let fd: i32 = entry.file_name().to_string_lossy().parse().ok()?;
            let target = fs::read_link(entry.path()).ok()?;
            Some((fd, target.to_string_lossy().into_owned()))
        })
        .collect();
    listed.sort();
    for (fd, target) in listed {
        let _ = writeln!(fds, "{fd} {target}");
    }
    fs::write(dir.join("fds"), fds).expect("the open descriptors");
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
        // A key for a window the app closed is lost, as a Windows program drops one for a window
        // it hid: only the keys its open windows were delivered count.
        let open_surfaces: Vec<_> = (0..open).map(|index| client.surface_id(index)).collect();
        let pressed = client
            .seen
            .keys
            .iter()
            .zip(&client.seen.keys_on)
            .filter(|((_, down), on)| {
                *down && on.as_ref().is_some_and(|on| open_surfaces.contains(on))
            })
            .count();
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
    /// The session serves its workspaces X11 displays, so a launch is given one and its keeper
    /// serves the app a display of its own.
    x11: bool,
    /// The desktop entry the app is launched by.
    entry: String,
    /// The relays the desktop's peer network is given, when a test gives it any.
    relays: Option<tilo_endpoint_reachability::EndpointRelays>,
}

/// One run of the desktop: its session, its apps, and how it ends when dropped.
struct Pass {
    session: Session,
    apps: Rc<Apps>,
    ending: Rc<Cell<Teardown>>,
}

impl Pass {
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
        Self::with_x11(titles, false)
    }

    /// A rig whose session serves X11, so each launch is given a display and its keeper asks for
    /// one of its own.
    fn keeping_x11(titles: &[&str]) -> Self {
        Self::with_x11(titles, true)
    }

    fn with_x11(titles: &[&str], x11: bool) -> Self {
        Self::launched_by(titles, x11, APP)
    }

    /// A rig whose app is launched by the desktop entry `entry`: its windows say they are `APP`.
    fn launched_by(titles: &[&str], x11: bool, entry: &str) -> Self {
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
            applications.join(format!("{entry}.desktop")),
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
            x11,
            entry: entry.to_owned(),
            relays: None,
        }
    }

    /// This device's peer network given a relay that never answers: what a desktop away from its
    /// home network, on a captive portal or offline, is given.
    fn on_a_silent_relay(mut self) -> Self {
        let silent: iroh::RelayUrl = SILENT_RELAY.parse().expect("a relay address");
        self.relays = Some(tilo_endpoint_reachability::EndpointRelays {
            relays: iroh::RelayMap::from(silent),
            n0_dns: false,
        });
        self
    }

    /// A run of the desktop, taking up what the keepers hold as the session's `main` does:
    /// `following` an in-place restart, or a start of its own.
    fn desktop(&self, following: bool) -> Pass {
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
            apps: Some(Rc::clone(&apps) as Rc<dyn AppActions>),
            launch: Some(Rc::clone(&apps)),
            state: Some(self.device.state.clone()),
            relays: self.relays.clone(),
            teardown: Box::new(move || asked.get()),
            ..Options::default()
        });
        let x11 = self
            .x11
            .then(|| session.compositor().x11_displays())
            .flatten();
        apps.set_displays(self.home.runtime().join("wayland-7"), x11, None);
        let connector = session.compositor().client_connector();
        apps.keep_behind(tilo_app_launch::kept::Keeping {
            keeper: self.keeper.clone(),
            root: self.root.clone(),
            desktop: Arc::new(move |pid| connector.connect_for(pid)),
        });
        apps.take_up_kept(following);
        session.step(Duration::from_millis(1)).expect("step");
        Pass {
            session,
            apps,
            ending,
        }
    }

    /// Start the app in the workspace in front.
    fn launch(&self, pass: &Pass) {
        let workspace = pass
            .session
            .handle()
            .read(|tree| tree.active_workspace().map(|held| held.id().to_string()))
            .expect("a workspace in front");
        pass.apps
            .start(
                &tilo_app_launch::Launch::Host {
                    desktop_id: self.entry.clone(),
                    open: None,
                },
                &pass.apps.workspace_named(&workspace),
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

/// Wait until the keeper was told the tab each of the app's windows `titles` names stands in, and
/// answer those tabs: what the keeper says of its windows is where the launch stands, not a tab
/// that showed a window of the same name before this launch's own came.
fn shown_and_told(rig: &Rig, pass: &mut Pass, titles: &[&str]) -> Vec<(String, TabId)> {
    let mut placed = Vec::new();
    until(
        &mut pass.session,
        "the keeper is told each tab",
        |session| {
            let told = rig.told();
            let found: Option<Vec<(String, TabId)>> = titles
                .iter()
                .map(|title| {
                    let (_, placement) = told
                        .iter()
                        .find(|(told, placement)| told == title && placement.is_some())?;
                    let tab: TabId = placement.as_deref()?.parse().ok()?;
                    matches!(tile_of(session, tab), Some(TileContent::Foreign { .. }))
                        .then(|| ((*title).to_owned(), tab))
                })
                .collect();
            let Some(found) = found else {
                return false;
            };
            placed = found;
            true
        },
    );
    placed
}

/// Send the window in `tab` to the workspace `to`, as its window menu does, and wait until it
/// stands there.
fn send(session: &mut Session, tab: TabId, to: WorkspaceId) {
    session
        .handle()
        .choose_window_menu(tab, &move_to_workspace_id(to), None)
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

/// Whether `pid` is the process that started at `at`, still alive: one that exited and is not
/// reaped yet is not.
fn running(pid: u32, at: u64) -> bool {
    tilo_process::start_time(pid) == Some(at)
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
    let mut first = rig.desktop(false);
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

    let mut second = rig.desktop(true);
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

/// A desktop given a relay that never answers restarts in place and shows its kept windows at
/// once: what it waits for is the layouts its own store holds, never the network.
#[test]
fn kept_windows_show_without_waiting_for_a_relay_that_never_answers() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]).on_a_silent_relay();
    let mut first = rig.desktop(false);
    rig.launch(&first);
    let placed = shown_and_told(&rig, &mut first, &["Only"]);
    first.end(Teardown::InPlaceRestart);

    let started = Instant::now();
    let mut second = rig.desktop(true);
    let back = shown_and_told(&rig, &mut second, &["Only"]);
    let waited = started.elapsed();
    eprintln!(
        "the kept window was shown {waited:?} after the desktop started, with no relay answering"
    );
    assert_eq!(back, placed, "the window is back in its saved tab");
    assert!(
        waited < tilo_endpoint_reachability::RELAY_WAIT,
        "the kept window came after {waited:?}: it waited on the network"
    );
    second.end(Teardown::Logout);
}

/// A launch gets only the descriptors it is handed: a file the desktop holds without close-on-exec
/// — a GPU node a driver opened, a saved document — does not reach the app behind its keeper.
#[test]
fn a_launched_app_inherits_no_descriptor_it_was_not_handed() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]);
    // The desktop's own descriptor, left without close-on-exec as a driver's own is.
    let leaked = rig.app.join("the-desktops-own");
    let held = fs::File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(&leaked)
        .expect("a file the desktop holds");
    // SAFETY: the descriptor is this process's, open until `held` is dropped.
    unsafe {
        assert_eq!(libc::fcntl(held.as_raw_fd(), libc::F_SETFD, 0), 0);
    }
    let mut pass = rig.desktop(false);
    rig.launch(&pass);
    shown_and_told(&rig, &mut pass, &["Only"]);
    let descriptors = fs::read_to_string(rig.app.join("fds")).expect("the app's descriptors");
    assert!(
        !descriptors.lines().any(|line| line
            .split_once(' ')
            .is_some_and(|(_, target)| target == leaked.to_string_lossy())),
        "the app inherited a descriptor it was not handed:\n{descriptors}"
    );
    pass.end(Teardown::Logout);
}

/// A kept program's second window closes while it has the keyboard, the way Wine closes a dialog
/// (its role goes, its surface stays): the keyboard goes back to the program's window that is still
/// open, and the next key reaches it.
#[test]
fn a_kept_apps_closed_window_hands_the_keyboard_to_its_window_still_open() {
    let _settled = ui::motion::testing::settled();
    let titles = ["Untitled", "Open"];
    let rig = Rig::new(&titles);
    let mut pass = rig.desktop(false);
    rig.launch(&pass);
    let placed = shown_and_told(&rig, &mut pass, &titles);
    let focused = |session: &mut Session| {
        session
            .compositor()
            .focused_window()
            .and_then(|window| window.title)
    };
    assert!(pass.session.handle().focus_tab(placed[1].1));
    crate::common::wait_for(
        &mut pass.session,
        "the second window has the keyboard",
        |session| (focused(session).as_deref() == Some("Open")).then_some(()),
    );
    let (_, pid, _) = rig.kept();
    signal(pid, libc::SIGUSR1);
    crate::common::wait_for(
        &mut pass.session,
        "the window that stays has the keyboard",
        |session| (focused(session).as_deref() == Some("Untitled")).then_some(()),
    );
    crate::common::window::tap_key(&mut pass.session, KEY_A);
    until(
        &mut pass.session,
        "the window still open hears the key",
        |_| rig.keys() == 1,
    );
    pass.end(Teardown::Logout);
}

/// The command line the session starts an app that is not Tilo's with is one the real launcher
/// takes: asked to plan the launch rather than make it, the launcher names the app the session
/// asked for. A launcher that answers to another command opens no such app at all.
/// A keeper killed takes its app with it: the person is told the app stopped working, in the tab
/// its window was in, and nothing else opens.
#[test]
fn an_app_whose_keeper_stops_says_so_in_its_own_tab() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]);
    let mut pass = rig.desktop(false);
    rig.launch(&pass);
    let placed = shown_and_told(&rig, &mut pass, &["Only"]);
    let tab = placed[0].1;
    let (_, pid, _) = rig.kept();
    signal(parent_of(pid), libc::SIGKILL);
    tilo_test_env::exited("the app of a keeper that is gone", pid);
    until(&mut pass.session, "its tab says it stopped", |session| {
        matches!(
            tile_of(session, tab),
            Some(TileContent::AppPlaceholder { .. })
        )
    });
    for _ in 0..100 {
        pass.session.step(Duration::from_millis(1)).expect("step");
    }
    assert_eq!(
        tabs(&pass.session)
            .into_iter()
            .map(|(other, _)| other)
            .collect::<Vec<_>>(),
        [tab],
        "the notice stands where the window was, and no other tab opens"
    );
    // The window went with its keeper: what the switcher walks and the overview pictures is the
    // windows, and a card saying the app stopped is the tab's, not one of them.
    let key = tab.to_string();
    let windows = crate::common::windows(&pass.session);
    assert!(
        !windows.iter().any(|window| window.key == key),
        "the window of the app that stopped is still one to walk to"
    );
    pass.end(Teardown::Logout);
}

/// A keeper killed with no word of how its app ended, while no desktop was there, took the app
/// with it: the person is told it stopped working in the tab its window was in, and nothing else
/// opens.
#[test]
fn a_keeper_killed_while_no_desktop_was_there_says_the_app_stopped_in_its_tab() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]);
    let mut first = rig.desktop(false);
    rig.launch(&first);
    let placed = shown_and_told(&rig, &mut first, &["Only"]);
    let tab = placed[0].1;
    let (dir, pid, _) = rig.kept();
    first.end(Teardown::InPlaceRestart);
    signal(parent_of(pid), libc::SIGKILL);
    tilo_test_env::exited("the app of a keeper killed while away", pid);
    assert!(
        !dir.join("ended").exists(),
        "its keeper never said how it ended"
    );

    let mut second = rig.desktop(true);
    until(&mut second.session, "its tab says it stopped", |session| {
        matches!(
            tile_of(session, tab),
            Some(TileContent::AppPlaceholder { .. })
        )
    });
    for _ in 0..100 {
        second.session.step(Duration::from_millis(1)).expect("step");
    }
    assert_eq!(
        tabs(&second.session)
            .into_iter()
            .map(|(other, _)| other)
            .collect::<Vec<_>>(),
        [tab],
        "the notice stands where the window was, and no other tab opens"
    );
    assert_eq!(rig.starts(), [pid], "and the app is not started again");
    second.end(Teardown::Logout);
}

/// A program whose window names its app otherwise than the desktop entry it was launched by takes
/// the tab the shell held for that launch: the window comes on the keeper connection the session
/// handed over for exactly that launch.
#[test]
fn a_kept_window_whose_id_differs_from_its_launch_takes_the_launchs_tab() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::launched_by(&["Only"], false, "org.example.Launched");
    let mut pass = rig.desktop(false);
    // As a press on the app's tile does: the shell holds the tab the window takes.
    let launch = tilo_app_launch::Launch::Host {
        desktop_id: "org.example.Launched".to_owned(),
        open: None,
    };
    pass.session
        .handle()
        .dispatch(
            tilo_session_core::desktop::navigation::ChromeEffect::OpenApp {
                app: launch.id(),
                beside: None,
                background: None,
                float: None,
            },
        )
        .expect("the person opens the app");
    let placed = shown_and_told(&rig, &mut pass, &["Only"]);
    for _ in 0..100 {
        pass.session.step(Duration::from_millis(1)).expect("step");
    }
    assert_eq!(
        tabs(&pass.session)
            .into_iter()
            .map(|(tab, _)| tab)
            .collect::<Vec<_>>(),
        [placed[0].1],
        "the window is in the tab its launch held, and no tab waits empty"
    );
    pass.end(Teardown::Logout);
}

#[test]
fn the_launcher_takes_the_command_line_the_session_starts_an_app_with() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]);
    let mut first = rig.desktop(false);
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
        .output_locked()
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
    let mut first = rig.desktop(false);
    rig.launch(&first);
    shown_and_told(&rig, &mut first, &["Only"]);
    let (dir, pid, _) = rig.kept();
    first.end(Teardown::InPlaceRestart);

    let second = rig.desktop(false);
    tilo_test_env::exited("an app a restart left that no run takes back", pid);
    assert!(!dir.exists(), "and its keeper's directory is gone");
    second.end(Teardown::Logout);
}

#[test]
fn a_window_the_app_closed_while_no_desktop_was_there_closes_its_tab() {
    let _settled = ui::motion::testing::settled();
    let titles = ["Stays", "Closes"];
    let rig = Rig::new(&titles);
    let mut first = rig.desktop(false);
    rig.launch(&first);
    let placed = shown_and_told(&rig, &mut first, &titles);
    let (_, pid, _) = rig.kept();
    first.end(Teardown::InPlaceRestart);

    signal(pid, libc::SIGUSR1);
    tilo_test_env::wait_until("the app closed its window", || rig.told().len() == 1);

    let mut second = rig.desktop(true);
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

/// Whether the session tells that the app stopped, with Reopen, in `tab`, the one tab it shows:
/// the notice put where its window was.
fn tells_stop(session: &Session, tab: TabId) -> bool {
    let labels = session.accessible_labels();
    labels.iter().any(|label| label == "Kept stopped working")
        && labels.iter().any(|label| label == "Reopen")
        && tabs(session).iter().map(|(shown, _)| *shown).eq([tab])
}

/// The kept app ends (a signal, so it stopped rather than closed) after the desktop went away
/// for an in-place restart, and its keeper notes how: the app's one tab, by id.
fn ended_while_away(rig: &Rig) -> TabId {
    let mut first = rig.desktop(false);
    rig.launch(&first);
    let placed = shown_and_told(rig, &mut first, &["Only"]);
    let (dir, pid, _) = rig.kept();
    first.end(Teardown::InPlaceRestart);
    signal(pid, libc::SIGTERM);
    tilo_test_env::exited("the app, while no desktop is there", pid);
    tilo_test_env::wait_until("its keeper notes the end", || dir.join("ended").exists());
    placed[0].1
}

#[test]
fn an_app_that_ended_while_no_desktop_was_there_says_it_stopped_in_its_tab() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]);
    let tab = ended_while_away(&rig);
    let [pid] = rig.starts()[..] else {
        panic!("one start: {:?}", rig.starts());
    };

    let mut second = rig.desktop(true);
    // Its end is told in the tab it stood in, where the person looks for it, with Reopen.
    until(
        &mut second.session,
        "the stop is told in its tab",
        |session| tells_stop(session, tab),
    );
    for _ in 0..100 {
        second.session.step(Duration::from_millis(1)).expect("step");
    }
    let shown: Vec<(TabId, Option<TileContent>)> = tabs(&second.session)
        .into_iter()
        .map(|(other, _)| (other, tile_of(&second.session, other)))
        .collect();
    assert!(
        tells_stop(&second.session, tab),
        "only the notice that it stopped, in its own tab, and nothing else opened: {shown:?}"
    );
    assert_eq!(rig.starts(), [pid], "and the app is not started again");
    second.end(Teardown::Logout);
}

/// The tab an app that ended while the desktop was away stood in tells that it stopped; the
/// notice is that session's. The tab is still the window the person left there, so the next login
/// opens the app in it again, once, as a start after a reboot does, and the notice is gone.
#[test]
fn the_tab_of_an_app_that_ended_while_away_opens_it_again_at_the_next_login() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]);
    let tab = ended_while_away(&rig);
    let mut second = rig.desktop(true);
    until(
        &mut second.session,
        "the stop is told in its tab",
        |session| tells_stop(session, tab),
    );
    second.end(Teardown::Logout);

    let mut third = rig.desktop(false);
    crate::common::wait_for(&mut third.session, "the app opens in its tab", |session| {
        (tab_showing(session, "Only") == Some(tab)).then_some(())
    });
    assert_eq!(rig.starts().len(), 2, "the app started once more");
    assert!(
        !third
            .session
            .accessible_labels()
            .iter()
            .any(|label| label == "Kept stopped working"),
        "the last session's notice is not shown again"
    );
    assert_eq!(tabs(&third.session).len(), 1, "and nothing opens beside it");
    third.end(Teardown::Logout);
}

#[test]
fn a_start_after_a_reboot_opens_the_tab_of_an_app_that_ended_again() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]);
    let tab = ended_while_away(&rig);

    // A start that follows no in-place restart has nothing running to keep: the saved tab opens
    // its app again, in that tab.
    let mut second = rig.desktop(false);
    crate::common::wait_for(&mut second.session, "the app opens in its tab", |session| {
        (tab_showing(session, "Only") == Some(tab)).then_some(())
    });
    assert_eq!(rig.starts().len(), 2, "the app started once more");
    second.end(Teardown::Logout);
}

/// A restart keeps an app whose keeper is merely slow to run: the update it applies is itself
/// load, and a delay in scheduling a healthy keeper must not cost the person what the app holds.
#[test]
fn a_restart_keeps_an_app_whose_keeper_is_slow() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]);
    let mut first = rig.desktop(false);
    rig.launch(&first);
    let placed = shown_and_told(&rig, &mut first, &["Only"]);
    let (dir, pid, at) = rig.kept();
    let keeper = parent_of(pid);
    signal(keeper, libc::SIGSTOP);

    first.end(Teardown::InPlaceRestart);
    assert!(
        dir.exists(),
        "a keeper that is slow keeps its app for the next run"
    );
    assert!(running(pid, at), "and the app goes on with what it holds");

    signal(keeper, libc::SIGCONT);
    let mut second = rig.desktop(true);
    let back = shown_and_told(&rig, &mut second, &["Only"]);
    assert_eq!(
        back[0].1, placed[0].1,
        "the next desktop takes it back where it stood"
    );
    assert_eq!(rig.starts(), [pid], "the same app, not a second copy");
    second.end(Teardown::Logout);
    tilo_test_env::exited("the kept app at logout", pid);
}

/// A restart ends an app whose keeper no later desktop could take back: its socket refuses, as a
/// keeper that ended does.
#[test]
fn a_restart_ends_an_app_whose_keepers_socket_refuses() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]);
    let mut first = rig.desktop(false);
    rig.launch(&first);
    shown_and_told(&rig, &mut first, &["Only"]);
    let (dir, pid, _) = rig.kept();
    let keeper = parent_of(pid);
    // The keeper stays up, but nothing can reach it again: neither this desktop nor the next.
    fs::remove_file(dir.join("control")).expect("the socket goes");

    first.end(Teardown::InPlaceRestart);
    assert!(
        !dir.exists(),
        "a keeper that cannot be reached is not left for the next run"
    );
    tilo_test_env::exited("the app of a keeper that cannot be reached", pid);
    // It ends once its app did; a keeper already gone is no failure.
    // SAFETY: kill takes a pid and a signal number.
    let _ = unsafe { libc::kill(libc::pid_t::try_from(keeper).unwrap(), libc::SIGKILL) };
}

/// A restart ends an app whose keeper process is gone: nothing would hold it, and no later
/// desktop could take it back.
#[test]
fn a_restart_ends_an_app_whose_keeper_is_gone() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]);
    let mut first = rig.desktop(false);
    rig.launch(&first);
    shown_and_told(&rig, &mut first, &["Only"]);
    let (dir, pid, _) = rig.kept();
    let keeper = parent_of(pid);
    signal(keeper, libc::SIGKILL);
    tilo_test_env::wait_until("the keeper's socket refuses", || {
        std::os::unix::net::UnixStream::connect(dir.join("control")).is_err()
    });

    first.end(Teardown::InPlaceRestart);
    assert!(
        !dir.exists(),
        "a keeper that is gone leaves nothing for the next run"
    );
    tilo_test_env::exited("the app of a keeper that is gone", pid);
}

#[test]
fn an_app_from_before_an_update_says_so_in_its_tab_and_restart_opens_it_again() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Only"]);
    let mut first = rig.desktop(false);
    rig.launch(&first);
    let placed = shown_and_told(&rig, &mut first, &["Only"]);
    let (dir, pid, at) = rig.kept();
    first.end(Teardown::InPlaceRestart);
    // The update changed how the desktop speaks to the app's keeper.
    testing::age(&dir, true).expect("aged");

    let mut second = rig.desktop(true);
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
    let mut first = rig.desktop(false);
    rig.launch(&first);
    shown_and_told(&rig, &mut first, &["Only"]);
    let (dir, pid, _) = rig.kept();
    first.end(Teardown::InPlaceRestart);
    testing::age(&dir, true).expect("aged");
    let mut second = rig.desktop(true);
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
    let mut first = rig.desktop(false);
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

    let mut second = rig.desktop(true);
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

/// A kept launch that is given an X11 display has one of its keeper's own, and the session serves
/// it to the composing input method once its server is up: the cookie that admits a client is in
/// the engine's authority file while the launch lives, the session that follows an in-place
/// restart serves it again, and it leaves the file once the keeper ends.
#[test]
fn a_kept_launchs_own_x11_display_is_served_to_the_input_method() {
    tilo_test_env::require_capability!(XServer, x_servers(), "the keeper's X server");
    let _settled = ui::motion::testing::settled();
    let rig = Rig::keeping_x11(&["Only"]);
    let merged = || {
        rig.home
            .runtime()
            .join("tilo-session")
            .join("input-method")
            .join("Xauthority")
    };
    let cookie = |dir: &Path| fs::read(dir.join("Xauthority")).unwrap_or_default();
    let mut first = rig.desktop(false);
    rig.launch(&first);
    let handed = crate::common::wait_for(
        &mut first.session,
        "the launcher is handed the launch",
        |_| {
            let handed = testing::launcher_handed(&rig.home.home().join("tilo-launcher"));
            (!handed.is_empty()).then_some(handed)
        },
    );
    assert!(
        handed.iter().any(|arg| arg == "--x11-display"),
        "the launch is given a display: {handed:?}"
    );
    // The keeper binds its display as it starts and names it in its directory.
    let dir = crate::common::wait_for(&mut first.session, "the keeper names its display", |_| {
        testing::described(&rig.root)
            .into_iter()
            .find_map(|(dir, _)| fs::read_to_string(dir.join("x11")).ok().map(|_| dir))
    });
    let display = fs::read_to_string(dir.join("x11")).expect("the display's name");
    assert!(display.starts_with(':'), "{display:?} names an X11 display");
    // A display whose server is not up yet is not served: a client would start one the app
    // never asked for.
    assert!(
        !merged().exists() || fs::read(merged()).unwrap_or_default() != cookie(&dir),
        "nothing is served before the launch starts an X program"
    );
    // The app's first X program starts the server; from then on the session serves the display.
    let x11 = connect_to_kept_display(&dir);
    crate::common::wait_for(
        &mut first.session,
        "the engine is given the kept display's cookie",
        |_| (fs::read(merged()).unwrap_or_default() == cookie(&dir)).then_some(()),
    );

    // A logout ends the launch, its X server with it.
    let (_, pid, at) = rig.kept();
    first.end(Teardown::Logout);
    tilo_test_env::exited("the kept app at logout", pid);
    assert!(!running(pid, at), "the app ended with the run");
    let _x11 = x11;
}

/// The lock and socket a kept launch's X11 display holds in `/tmp`, by the display's name in the
/// keeper's directory `dir`.
fn display_files(dir: &Path) -> [PathBuf; 2] {
    let display = fs::read_to_string(dir.join("x11")).expect("the display's name");
    let number = display.trim().trim_start_matches(':').to_owned();
    [
        PathBuf::from(format!("/tmp/.X{number}-lock")),
        PathBuf::from(format!("/tmp/.X11-unix/X{number}")),
    ]
}

/// The directory of the keeper `which` once its display is named there.
fn named_display(rig: &Rig, pass: &mut Pass, which: &str) -> PathBuf {
    crate::common::wait_for(
        &mut pass.session,
        format!("the {which} keeper names its display"),
        |_| {
            testing::described(&rig.root)
                .into_iter()
                .find_map(|(dir, _)| fs::read_to_string(dir.join("x11")).ok().map(|_| dir))
        },
    )
}

/// A kept launch's X11 display leaves nothing in `/tmp` however its keeper ends: killed while the
/// desktop is up, when it has no moment to remove its own lock, or at the logout that ends it. The
/// same app starts again in that session while the killed keeper's files are still being cleared:
/// its new keeper names a display of its own, an X client reaches it, and the app starts again.
#[test]
fn a_kept_launchs_x11_display_leaves_nothing_in_tmp_however_its_keeper_ends() {
    tilo_test_env::require_capability!(XServer, x_servers(), "the keeper's X11 display");
    let _settled = ui::motion::testing::settled();
    let rig = Rig::keeping_x11(&["Only"]);
    let mut pass = rig.desktop(false);

    // Killed while the desktop is up.
    rig.launch(&pass);
    shown_and_told(&rig, &mut pass, &["Only"]);
    let first = display_files(&named_display(&rig, &mut pass, "first"));
    assert!(
        first.iter().all(|file| file.exists()),
        "the keeper holds its display: {first:?}"
    );
    let (first_dir, pid, _) = rig.kept();
    signal(parent_of(pid), libc::SIGKILL);
    tilo_test_env::exited("the app of a keeper that is gone", pid);

    // Started again in the same session, at once: its new keeper names a display of its own, an X
    // client reaches it, and the app starts a second time.
    rig.launch(&pass);
    shown_and_told(&rig, &mut pass, &["Only"]);
    assert_eq!(rig.starts().len(), 2, "the app started again");
    let dir = named_display(&rig, &mut pass, "second");
    assert_ne!(dir, first_dir, "a keeper of its own");
    let files = display_files(&dir);
    assert!(
        files.iter().all(|file| file.exists()),
        "the keeper holds its display: {files:?}"
    );
    let handed = testing::launcher_handed(&rig.home.home().join("tilo-launcher"));
    assert!(
        handed.iter().any(|arg| arg == "--x11-display"),
        "the second launch is given a display: {handed:?}"
    );
    let x11 = connect_to_kept_display(&dir);
    // The killed keeper's files go, and clearing them leaves the new keeper's alone.
    if first != files {
        tilo_test_env::wait_until("the killed keeper's display files go", || {
            first.iter().all(|file| !file.exists())
        });
    }
    assert!(
        files.iter().all(|file| file.exists()),
        "the new keeper's display files stay: {files:?}"
    );

    // Ended by the logout, its X server with it: the display leaves nothing behind.
    pass.end(Teardown::Logout);
    tilo_test_env::wait_until("the logout's keeper leaves no display files", || {
        files.iter().all(|file| !file.exists())
    });
    let _x11 = x11;
}

/// The X program behind a kept launch can lose its server, as Xwayland does when its programs are
/// killed: its windows leave the desktop, and the session goes on stepping.
#[test]
fn a_kept_x_programs_server_ending_closes_its_windows_and_the_session_goes_on() {
    tilo_test_env::require_capability!(XServer, x_servers(), "the keeper's X server");
    let _settled = ui::motion::testing::settled();
    let rig = Rig::keeping_x11(&["Only"]);
    let mut first = rig.desktop(false);
    rig.launch(&first);
    shown_and_told(&rig, &mut first, &["Only"]);
    let dir = crate::common::wait_for(&mut first.session, "the keeper names its display", |_| {
        testing::described(&rig.root)
            .into_iter()
            .find_map(|(dir, _)| fs::read_to_string(dir.join("x11")).ok().map(|_| dir))
    });
    // An X program of the launch: its client starts the keeper's server, and maps a window.
    let x11 = connect_to_kept_display(&dir);
    x_window(&x11, "Dying");
    // The X program's window, as the desktop shows it: by its class, which the keeper passes on as
    // the window's app id.
    let shows_the_x_window = |session: &mut tilo_session::Session| {
        session.compositor().windows().iter().any(|(info, _)| {
            info.app_id
                .as_deref()
                .is_some_and(|id| id.eq_ignore_ascii_case("dying"))
                || info
                    .title
                    .as_deref()
                    .is_some_and(|title| title.contains("Dying"))
        })
    };
    crate::common::wait_for(
        &mut first.session,
        "the X program's window shows",
        |session| shows_the_x_window(session).then_some(()),
    );
    let number = fs::read_to_string(dir.join("x11")).expect("the display");
    let number: u32 = number
        .trim()
        .trim_start_matches(':')
        .parse()
        .expect("a number");
    let pid = x11_server(number).expect("the server is up");
    signal(pid, libc::SIGKILL);
    crate::common::wait_for(
        &mut first.session,
        "the X program's window leaves",
        |session| (!shows_the_x_window(session)).then_some(()),
    );
    // The launch outlives both its server and the desktop: a restart takes it back, and its
    // program's next client finds a fresh server on the same display.
    first.end(Teardown::InPlaceRestart);
    let mut second = rig.desktop(true);
    shown_and_told(&rig, &mut second, &["Only"]);
    let next = connect_to_kept_display(&dir);
    x_window(&next, "Dying");
    crate::common::wait_for(
        &mut second.session,
        "the X program's window shows again",
        |session| shows_the_x_window(session).then_some(()),
    );
    let pid = x11_server(number).expect("the fresh server is up");
    signal(pid, libc::SIGKILL);
    crate::common::wait_for(
        &mut second.session,
        "the X program's window leaves again",
        |session| (!shows_the_x_window(session)).then_some(()),
    );
    second.end(Teardown::Logout);
    drop((x11, next));
}

/// An X11 program of `x11`'s client: a 200×150 window of the class `class`, mapped.
fn x_window(x11: &x11rb::rust_connection::RustConnection, class: &str) {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ConnectionExt as _, CreateWindowAux, WindowClass};
    use x11rb::wrapper::ConnectionExt as _;

    let root = x11.setup().roots[0].root;
    let window = x11.generate_id().expect("a window id");
    x11.create_window(
        x11rb::COPY_DEPTH_FROM_PARENT,
        window,
        root,
        0,
        0,
        200,
        150,
        0,
        WindowClass::INPUT_OUTPUT,
        x11rb::COPY_FROM_PARENT,
        &CreateWindowAux::new().background_pixel(0x0000_00ff),
    )
    .expect("the window");
    x11.change_property8(
        x11rb::protocol::xproto::PropMode::REPLACE,
        window,
        x11rb::protocol::xproto::AtomEnum::WM_CLASS,
        x11rb::protocol::xproto::AtomEnum::STRING,
        format!("{}\0{class}\0", class.to_lowercase()).as_bytes(),
    )
    .expect("its class");
    x11.map_window(window).expect("map");
    x11.flush().expect("flush");
}

/// The pid of the server on display `number`: `xwayland-satellite :N`.
fn x11_server(number: u32) -> Option<u32> {
    let wanted = format!(":{number}");
    for entry in fs::read_dir("/proc").ok()? {
        let entry = entry.ok()?;
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = fs::read(entry.path().join("cmdline")) else {
            continue;
        };
        let args: Vec<&[u8]> = cmdline.split(|byte| *byte == 0).collect();
        if args
            .first()
            .is_some_and(|arg| arg.ends_with(b"xwayland-satellite"))
            && args.get(1).is_some_and(|arg| *arg == wanted.as_bytes())
        {
            return Some(pid);
        }
    }
    None
}

/// The X servers a keeper can start here, so a machine without them skips the test.
fn x_servers() -> Result<(), String> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    for program in ["xwayland-satellite", "Xwayland"] {
        if !std::env::split_paths(&path).any(|dir| dir.join(program).is_file()) {
            return Err(format!("{program} is not on PATH"));
        }
    }
    Ok(())
}

/// A client of the display a keeper bound in `dir`: connecting starts its server, as the launch's
/// own X program would.
fn connect_to_kept_display(dir: &Path) -> x11rb::rust_connection::RustConnection {
    use x11rb::rust_connection::{DefaultStream, RustConnection};

    let display = fs::read_to_string(dir.join("x11")).expect("the display's name");
    let display = display.trim().trim_start_matches(':');
    let authority = fs::read(dir.join("Xauthority")).expect("the display's cookie");
    let cookie = authority[authority.len() - 16..].to_vec();
    let stream = std::os::unix::net::UnixStream::connect(format!("/tmp/.X11-unix/X{display}"))
        .expect("the display listens");
    let (stream, _) = DefaultStream::from_unix_stream(stream).expect("the display's socket");
    RustConnection::connect_to_stream_with_auth_info(
        stream,
        0,
        b"MIT-MAGIC-COOKIE-1".to_vec(),
        cookie,
    )
    .expect("the cookie admits the client")
}

/// A keeper a service started (the Linux VM's window forwarder), with the app in it: the session
/// finds it in the registry by itself.
fn start_service_keeper(rig: &Rig) -> std::process::Child {
    use std::os::unix::process::CommandExt as _;
    let launch = tilo_app_launch::kept::KeptLaunch {
        version: tilo_app_launch::kept::LAUNCH_VERSION,
        app: "vm.linux".into(),
        title: "Linux".into(),
        workspace: String::new(),
        asked: None,
        window_identities: Vec::new(),
        service: true,
    };
    let dir = testing::new_dir(&rig.root, &launch).expect("the keeper's directory");
    let exe = std::env::current_exe().unwrap();
    std::process::Command::new(&rig.keeper)
        .arg("--dir")
        .arg(&dir)
        .arg("--launch")
        .arg(launch.encode())
        .arg("--app-id-prefix")
        .arg("vm.linux.")
        .arg("--")
        .arg("/usr/bin/env")
        .arg(format!("{APP_DIR}={}", rig.app.display()))
        .arg(exe)
        .args(["--exact", "kept_apps::kept_app", "--nocapture"])
        .process_group(0)
        .spawn_locked()
        .expect("the service's keeper starts")
}

#[test]
fn a_services_window_takes_the_keyboard_on_a_click_and_hears_the_keys() {
    let _settled = ui::motion::testing::settled();
    let rig = Rig::new(&["Guest"]);
    let mut desktop = rig.desktop(false);
    let mut keeper = start_service_keeper(&rig);
    let window = crate::common::wait_for(
        &mut desktop.session,
        "the guest's window shows",
        |session| {
            let (info, _) = session
                .compositor()
                .windows()
                .into_iter()
                .find(|(info, _)| info.title.as_deref() == Some("Guest"))?;
            session
                .compositor()
                .window_rect(info.id)
                .map(|rect| (info.id, rect))
        },
    );
    let (_, (x, y, width, height)) = window;
    crate::common::window::click(
        &mut desktop.session,
        f64::from(x + width / 2),
        f64::from(y + height / 2),
    );
    crate::common::wait_for(
        &mut desktop.session,
        "the window has the keyboard",
        |session| {
            session
                .compositor()
                .focused_window()
                .is_some_and(|window| window.title.as_deref() == Some("Guest"))
                .then_some(())
        },
    );
    crate::common::window::tap_key(&mut desktop.session, KEY_A);
    until(&mut desktop.session, "the app hears the key", |_| {
        rig.keys() == 1
    });
    let _ = keeper.kill();
    let _ = keeper.wait();
    desktop.end(Teardown::Logout);
}
