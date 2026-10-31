//! Opening an app: the launches the desktop asks for, planned into the
//! process that starts them and started in the session's own environment.
//!
//! The desktop surface knows an app by name — a package id, `host:<desktop
//! id>`, a window on its way, a record or some content — and opens a tab for
//! one of its own windows. What it cannot do is start a process: it queues
//! the launch for the session ([`tilo_app_launch`]), which plans it here
//! against the workspace in front and the displays this session bound, and
//! starts it without waiting for it.
//!
//! The same entry point serves everything else that asks for an app by name:
//! a search row, a file or a link the person opens
//! ([`crate::search::AppActions`]).
//!
//! When an app stops without choosing to, the session offers it again in the
//! place its window had — a notice with Reopen — instead of leaving the
//! person with nothing.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use tilo_actions::DoorError;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, RemoteActionTarget,
    workspace_provider_room,
};
use tilo_app_launch::routing;
use tilo_app_launch::{
    Answer, Instance, Launch, PlanContext, Planned, Room, SessionEnvironment, Shown, Spawner,
};
use tilo_desktop_search::providers::PublishedActions;
use tilo_desktop_search::providers::jobs::JobEntry;
use tilo_surface_host::{DesktopHost, WidgetHost};

use crate::search::AppActions;

/// The apps this session starts, and everything that asks it for one.
pub struct Apps {
    /// The desktop the launches come from, once the session has mounted it:
    /// what a launch reads its room from, and where a stopped app's notice
    /// goes.
    desktop: RefCell<Option<DesktopHost>>,
    state: RefCell<State>,
}

/// What starting an app needs besides the launch itself.
struct State {
    environment: SessionEnvironment,
    spawner: Spawner,
    /// The launches running now, by the id the spawner gave each: what a
    /// crash notice's Reopen asks for again.
    running: BTreeMap<u64, String>,
    /// The `tilo-launcher` packages start through; `None` when none is
    /// installed, and then only the machine's own apps can start.
    launcher: Option<PathBuf>,
    /// The identity record this workspace's apps are given, when the session
    /// resolved one.
    identity: Option<PathBuf>,
    /// The GPU the compositor renders on, for an app to render on the same.
    render_node: Option<PathBuf>,
    /// The session's launch records, once it has lent them: what its broker
    /// admits a connection by, and so what each launch it makes is written
    /// into.
    launches: Option<std::sync::Arc<crate::Launches>>,
}

impl Apps {
    /// The apps `environment` starts, waking `wake`'s loop when one ends.
    #[must_use]
    pub fn new(environment: SessionEnvironment, wake: Option<tilo_service::Wake>) -> Self {
        let launcher = PlanContext::find_launcher();
        if launcher.is_none() {
            tracing::warn!("no tilo-launcher is installed: Tilo apps cannot start here");
        }
        Self::with_launcher(environment, wake, launcher)
    }

    /// As [`Self::new`], starting packages through `launcher`: a session
    /// built beside its own launcher (a test's) knows where it is.
    #[must_use]
    pub fn with_launcher(
        environment: SessionEnvironment,
        wake: Option<tilo_service::Wake>,
        launcher: Option<PathBuf>,
    ) -> Self {
        Self {
            desktop: RefCell::new(None),
            state: RefCell::new(State {
                environment,
                spawner: Spawner::new(wake),
                running: BTreeMap::new(),
                launcher,
                identity: None,
                render_node: None,
                launches: None,
            }),
        }
    }

    /// The compositor bound its Wayland socket `socket` and, when Xwayland
    /// started, the X11 display `x11`: apps started from now on connect to
    /// these, whatever the session itself was started with.
    pub fn set_displays(
        &self,
        socket: PathBuf,
        x11: Option<(String, Option<PathBuf>)>,
        session_bus: Option<String>,
    ) {
        let mut state = self.state.borrow_mut();
        state.environment.wayland_socket = Some(socket);
        state.environment.x11_display = x11.as_ref().map(|(display, _)| display.clone());
        state.environment.x11_authority = x11.and_then(|(_, authority)| authority);
        state.environment.session_bus = session_bus;
    }

    /// The session's launch records: each app started from here on is
    /// written into them, so its connection to the session's services is
    /// admitted for the launch that made it.
    pub fn lend_launches(&self, launches: std::sync::Arc<crate::Launches>) {
        self.state.borrow_mut().launches = Some(launches);
    }

    /// The desktop the person works on, which the launches come from.
    pub fn attach(&self, desktop: DesktopHost) {
        *self.desktop.borrow_mut() = Some(desktop);
    }

    /// Start what the desktop queued since the last pass, and show what
    /// stopped: called once per pass of the session's loop.
    pub fn follow(&self) {
        self.start_asked();
        self.follow_ended();
    }

    /// The launches the desktop queued: each named app is planned against
    /// the workspace in front and started.
    fn start_asked(&self) {
        let Some(desktop) = self.desktop.borrow().clone() else {
            return;
        };
        let asked = desktop.take_launches();
        if asked.is_empty() {
            return;
        }
        let room = self.active_room();
        for asked in asked {
            let Some(launch) = Launch::parse(&asked) else {
                tracing::warn!(%asked, "not a launch this session knows");
                continue;
            };
            // A window is on its way to its own tab, and a notice starts
            // nothing: neither asks for a process.
            if matches!(launch, Launch::Window { .. } | Launch::Crashed { .. }) {
                continue;
            }
            if let Err(error) = self.start(&launch, &room) {
                tracing::warn!(%asked, %error, "an app did not start");
            }
        }
    }

    /// The apps that ended since the last pass: one that chose to end leaves
    /// its window behind and is gone; one that crashed gets a notice in the
    /// place it had, offering it again.
    fn follow_ended(&self) {
        let ended = self.state.borrow().spawner.events();
        for event in ended {
            if !event.end.crashed() {
                self.state.borrow_mut().running.remove(&event.id);
                tracing::info!(app = %event.app, "an app closed");
                continue;
            }
            tracing::warn!(app = %event.app, end = %event.end, "an app stopped");
            // Reopen asks for the app the way it was asked for this time: a
            // host app by its desktop id, a Tilo app by its package id.
            let asked = self.state.borrow_mut().running.remove(&event.id);
            if asked.is_none() {
                tracing::warn!(app = %event.app, "a stopped app this session did not start");
            }
            if let Some(desktop) = self.desktop.borrow().clone() {
                let notice = desktop.with_policy(|policy| {
                    policy.crashed(asked.as_deref().unwrap_or(&event.app), &event.title)
                });
                if let Err(error) = notice {
                    tracing::warn!(%error, app = %event.app, "no notice for the app");
                }
                // The notice is a desktop command like any other: the desk
                // carries it out, and lays its tab out, on its next pass.
                desktop.with_widgets(WidgetHost::relayout);
            }
        }
    }

    /// What a launch does about the app maybe already showing in `room`:
    /// `Some(new_window)` to start it, `None` when the window it has was
    /// raised instead, and nothing starts.
    ///
    /// A launch is decided against the windows on the workspace in front,
    /// which is the one it belongs to. A launch into another room starts its
    /// own process, as it always has: that room's windows are not showing.
    fn starting(&self, launch: &Launch, room: &Room) -> Option<bool> {
        let Some(desktop) = self.desktop.borrow().clone() else {
            return Some(false);
        };
        if room.is_workspace() && self.active_room().id != room.id {
            return Some(false);
        }
        let shown = Shown::new(desktop.with_policy(|policy| policy.windows_in_front()));
        if shown.is_empty() {
            // Nothing of this app can be showing on a desk with no windows.
            return Some(false);
        }
        let Some(app) = Instance::of(launch, &desktop_entry::cache::cached_scan()) else {
            return Some(false);
        };
        let window = match app.answer(launch, &shown) {
            Answer::Start { new_window } => return Some(new_window),
            Answer::Raise { window } => window,
        };
        if !desktop.with_policy(|policy| policy.focus(window)) {
            // The window went as it was asked for: the app starts again.
            return Some(false);
        }
        desktop.with_widgets(WidgetHost::relayout);
        tracing::info!(
            app = %app.id,
            window,
            "raised the window the app already has"
        );
        None
    }

    /// The room a launch from the desktop belongs to: the workspace in
    /// front, with what the person granted apps in it.
    fn active_room(&self) -> Room {
        let Some(desktop) = self.desktop.borrow().clone() else {
            return Room::under(&runtime_dir(), String::new());
        };
        let handle = desktop.with_policy(|policy| policy.handle().clone());
        handle.read(|tree| match tree.active_workspace() {
            Some(workspace) => {
                let mut room = Room::under(&runtime_dir(), workspace.room().to_string());
                room.grants = workspace.launch_grants(unix_seconds());
                room
            }
            None => Room::under(&runtime_dir(), String::new()),
        })
    }

    /// The room named `room`, with its grants when the desktop holds it and
    /// none when it does not: a launch into a room this device does not show
    /// gets the app's manifest permissions alone.
    fn room_named(&self, room: &str) -> Room {
        self.desktop.borrow().clone().map_or_else(
            || Room::under(&runtime_dir(), room.to_owned()),
            |desktop| {
                let handle = desktop.with_policy(|policy| policy.handle().clone());
                handle.read(|tree| {
                    let held = tree
                        .workspaces
                        .iter()
                        .find(|workspace| workspace.room().to_string() == room);
                    let mut launched = Room::under(&runtime_dir(), room.to_owned());
                    if let Some(workspace) = held {
                        launched.grants = workspace.launch_grants(unix_seconds());
                    }
                    launched
                })
            },
        )
    }

    /// Start `launch` in `room`, unless the app already has a window there:
    /// that one is raised and nothing starts.
    ///
    /// # Errors
    /// The launch starts nothing, or the process could not be started.
    pub fn start(&self, launch: &Launch, room: &Room) -> Result<(), DoorError> {
        let Some(new_window) = self.starting(launch, room) else {
            return Ok(());
        };
        let planned = {
            let state = self.state.borrow();
            let context = PlanContext {
                environment: &state.environment,
                room,
                launcher: state.launcher.clone(),
                identity: state.identity.clone(),
                render_node: state.render_node.clone(),
                new_window,
            };
            Planned::of(launch, &context)
        };
        let planned =
            planned.map_err(|error| DoorError::Failed(format!("{}: {error}", launch.id())))?;
        let started = self.state.borrow_mut().spawner.start(&planned);
        match started {
            Ok(started) => {
                let mut state = self.state.borrow_mut();
                state.running.insert(started.id, launch.id());
                if let Some(launches) = &state.launches {
                    launches.started(started.pid, &planned.app, &room.id);
                }
                tracing::info!(
                    app = %planned.app,
                    pid = started.pid,
                    "started {}",
                    planned.describe()
                );
                Ok(())
            }
            Err(error) => Err(DoorError::Failed(error.to_string())),
        }
    }
}

impl AppActions for Apps {
    /// Nothing is published through this session yet: the action host an
    /// app's resources and jobs arrive on is not mounted here.
    fn published(&self) -> Vec<PublishedActions> {
        Vec::new()
    }

    fn jobs(&self) -> Vec<JobEntry> {
        Vec::new()
    }

    /// The one action this session carries out itself is the workspace's
    /// `open` of an app — the app's own action on its own resource, which
    /// starts it. Everything else is delivered to a running app, and there
    /// is no action host here to deliver through.
    fn invoke(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), DoorError> {
        let open = target.address.action == APPLICATION_OPEN_ACTION
            && arguments.is_empty()
            && target
                .address
                .resource
                .starts_with(APPLICATION_RESOURCE_PREFIX);
        let app = target
            .address
            .resource
            .strip_prefix(APPLICATION_RESOURCE_PREFIX);
        match (open, app, workspace_provider_room(&target.provider)) {
            (true, Some(app), Some(room)) => {
                let room = self.room_named(room);
                self.start(
                    &Launch::Package {
                        app: app.to_owned(),
                        open: None,
                    },
                    &room,
                )
            }
            _ => Err(no_action_host()),
        }
    }

    fn cancel(&self, _app: &str, _room: &str, _invocation: &str) -> Result<(), DoorError> {
        Err(no_action_host())
    }

    /// Open `uri` in the app that takes it: a document's own app when it is
    /// a file on this machine, else the app that claims its scheme.
    fn open_uri(&self, room: &str, uri: &str) -> Result<(), DoorError> {
        let installed = tilo_installed_apps::read_from_env().unwrap_or_default();
        let entries = desktop_entry::cache::cached_scan();
        let launch = match uri.strip_prefix("file://") {
            // A file URI is percent-encoded; the path it names is not.
            Some(path) => {
                routing::document(Path::new(&ui::uri::decode(path)), &installed, &entries)
            }
            None => routing::link(uri, &installed, &entries),
        }
        .ok_or_else(|| DoorError::Failed(format!("nothing on this device opens {uri}")))?;
        let room = self.room_named(room);
        self.start(&launch, &room)
    }

    fn start_host_app(&self, desktop_id: &str) -> Result<(), DoorError> {
        // One of the machine's own apps runs unconfined, so it is launched
        // into no room: it is told nothing of Tilo's authority.
        self.start(
            &Launch::Host {
                desktop_id: desktop_id.to_owned(),
                open: None,
            },
            &Room::under(&runtime_dir(), String::new()),
        )
    }
}

/// Why an action an app publishes cannot be run here: the action host an
/// app's resources arrive on is not mounted in this session.
fn no_action_host() -> DoorError {
    DoorError::Unavailable {
        code: "action_host_unavailable".into(),
        reason: "The session runs without an action host".into(),
    }
}

/// Where a launch's private files live: the per-user runtime directory.
fn runtime_dir() -> PathBuf {
    tilo_paths::xdg::runtime_dir_or_default()
}

/// The time grants are decided at: seconds since the epoch, as the room
/// stores them.
fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}
