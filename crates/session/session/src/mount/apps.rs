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
use std::rc::{Rc, Weak};

use tilo_action_broker::services::LaunchPlace;
use tilo_actions::DoorError;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, RemoteActionTarget, provider_workspace,
};
use tilo_app_launch::routing;
use tilo_app_launch::{
    Answer, Authority, Cause, Doors as WindowDoors, HeldDoor, Instance, Kind, Launch, PlanContext,
    Planned, Routed, SessionEnvironment, Shown, Spawner, StartAlone, Workspace,
};
use tilo_desktop_search::providers::PublishedActions;
use tilo_desktop_search::providers::jobs::JobEntry;
use tilo_desktop_surface::crashed_app::Closed;
use tilo_surface_host::{DesktopHost, WidgetHost};

use crate::search::AppActions;

/// The apps this session starts, and everything that asks it for one.
pub struct Apps {
    /// The desktop the launches come from, once the session has mounted it:
    /// what a launch reads its workspace from, and where a stopped app's notice
    /// goes.
    desktop: RefCell<Option<DesktopHost>>,
    state: RefCell<State>,
}

/// What lets a package author its own records in the workspace it opens in,
/// called with the workspace's id and the package's.
type Authorship = Rc<dyn Fn(&str, &str)>;

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
    /// The person at this device, by the id the workspaces' governance names
    /// them by: whose authority each app it starts is handed.
    person: String,
    /// What the person's authority in a workspace is published as, for each
    /// app this session starts there, and this session's hold on it.
    authority: Authority,
    /// The desk's revision the published authority was last brought up to:
    /// nothing is republished while the desk has not moved.
    authority_at: Option<u64>,
    /// The GPU the compositor renders on, for an app to render on the same.
    render_node: Option<PathBuf>,
    /// The session's launch records, once it has lent them: what its broker
    /// admits a connection by, and so what each launch it makes is written
    /// into.
    launches: Option<std::sync::Arc<crate::Launches>>,
    /// The window doors of the apps this session starts, once it lent them:
    /// one process per app per workspace opens that app's later windows itself.
    doors: Option<Rc<WindowDoors>>,
    /// This session, while the doors are lent: a window that must start a
    /// process of its own starts it here.
    me: Weak<Apps>,
    /// What lets a package author its own records in the workspace it opens
    /// in, once it is lent: asked before the process is planned, so the app
    /// starts with the authorship rather than waiting for a window to reopen.
    authorship: Option<Authorship>,
    /// The door of each launch still running, by the launch's id: the end of
    /// the launch retires it, so the app's next window there starts a process
    /// again.
    held: BTreeMap<u64, HeldDoor>,
    /// What the tool a launch opens was asked to show, for the one launch
    /// being planned (`TILO_TOOL_CONTEXT`): set only while a launch a service
    /// asked for is being started, and read where it is planned.
    tool_context: Option<String>,
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
                person: String::new(),
                authority: Authority::new(),
                authority_at: None,
                render_node: None,
                launches: None,
                doors: None,
                me: Weak::new(),
                authorship: None,
                held: BTreeMap::new(),
                tool_context: None,
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

    /// The render node of the GPU the compositor draws on: an app the
    /// person granted `gpu` is pinned to it, as it is pinned to the same
    /// driver, instead of choosing whichever GPU wgpu offers it.
    pub fn set_render_node(&self, node: Option<PathBuf>) {
        self.state.borrow_mut().render_node = node;
    }

    /// The person at this device, by the id the workspaces' governance names
    /// them by: whose authority in a workspace each app started from here on
    /// is handed, so it decides their actions as the workspace's policy says.
    pub fn set_person(&self, person: impl Into<String>) {
        self.state.borrow_mut().person = person.into();
    }

    /// The session's launch records: each app started from here on is
    /// written into them, so its connection to the session's services is
    /// admitted for the launch that made it.
    pub fn lend_launches(&self, launches: std::sync::Arc<crate::Launches>) {
        self.state.borrow_mut().launches = Some(launches);
    }

    /// The window doors the session keeps for the apps it starts: from here
    /// on the first launch of an app in a workspace hands its process the
    /// door, and every later window of that app there opens in that process.
    pub fn lend_doors(self: &Rc<Self>, doors: Rc<WindowDoors>) {
        let mut state = self.state.borrow_mut();
        state.me = Rc::downgrade(self);
        state.doors = Some(doors);
    }

    /// What lets a package author its own records in the workspace it opens
    /// in, once it is lent: called with the workspace's id and the package's,
    /// before the launch is planned, so the process starts as an author.
    pub fn lend_authorship(&self, authorship: Authorship) {
        self.state.borrow_mut().authorship = Some(authorship);
    }

    /// The desktop the person works on, which the launches come from.
    pub fn attach(&self, desktop: DesktopHost) {
        *self.desktop.borrow_mut() = Some(desktop);
    }

    /// The session is ending: every app it started that still runs is asked
    /// to end with it.
    pub fn end_running(&self) {
        self.state.borrow().spawner.end_running();
    }

    /// Start what the desktop queued since the last pass, show what
    /// stopped, and start what a service, an app or an agent asked for: called
    /// once per pass of the session's loop.
    pub fn follow(&self) {
        self.start_asked();
        self.start_asked_of_services();
        self.follow_ended();
        self.follow_policy();
    }

    /// Bring what the person's authority in a workspace is published as up to
    /// the policy the desk holds now: a part someone is given or taken back
    /// reaches the apps they already run, rather than only their next launch.
    /// The desk's revision says whether anything may have changed at all, so a
    /// pass that changed nothing writes nothing.
    fn follow_policy(&self) {
        let Some(desktop) = self.desktop.borrow().clone() else {
            return;
        };
        let revision = desktop.with_policy(|policy| policy.handle().revision());
        {
            let state = self.state.borrow();
            if state.authority.is_empty() || state.authority_at == Some(revision) {
                return;
            }
        }
        self.state.borrow_mut().authority_at = Some(revision);
        for room in self.state.borrow().authority.workspaces() {
            let workspace = self.workspace_named(&room);
            if let Err(error) = self.state.borrow().authority.refresh(&workspace) {
                tracing::warn!(%room, %error, "the person's authority there was not published");
            }
        }
    }

    /// The launches services, apps and agents asked the session for: each is
    /// started on the path the desk's own launches take, in the workspace the
    /// asker named, and the asker is told how it went.
    fn start_asked_of_services(&self) {
        let Some(launches) = self.state.borrow().launches.clone() else {
            return;
        };
        for request in launches.take_waiting() {
            let workspace = if request.workspace.is_empty() {
                self.active_workspace()
            } else {
                self.workspace_named(&request.workspace)
            };
            let context = request.context.clone();
            let started = self.starting_tool(context.as_deref(), || {
                if matches!(request.place, LaunchPlace::Beside { .. }) {
                    self.start_beside(&request.launch, &workspace)
                } else {
                    self.start(&request.launch, &workspace)
                }
            });
            if let Err(error) = &started {
                tracing::warn!(
                    app = %request.launch.id(),
                    workspace = %workspace.id,
                    %error,
                    "an app a service asked for did not start"
                );
            }
            request.answer(started.map_err(|error| error.to_string()));
        }
    }

    /// Start what `start` launches with the tool `context` it opens: what the
    /// service that asked for the launch told the tool to show.
    fn starting_tool(
        &self,
        context: Option<&str>,
        start: impl FnOnce() -> Result<(), DoorError>,
    ) -> Result<(), DoorError> {
        let previous = std::mem::replace(
            &mut self.state.borrow_mut().tool_context,
            context.map(str::to_owned),
        );
        let started = start();
        self.state.borrow_mut().tool_context = previous;
        started
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
            let launch = as_this_machine_knows_it(launch);
            if let Err(error) = self.open(&launch) {
                tracing::warn!(%asked, %error, "an app did not start");
            }
        }
    }

    /// Start `launch` on the workspace in front, which is where what the
    /// session itself opens belongs.
    ///
    /// # Errors
    /// The launch starts nothing, or the process could not be started.
    pub fn open(&self, launch: &Launch) -> Result<(), DoorError> {
        let workspace = self.active_workspace();
        self.start(launch, &workspace)
    }

    /// The apps that ended since the last pass: one that chose to end leaves
    /// its window behind and is gone; one that crashed gets a notice in the
    /// place it had, offering it again.
    fn follow_ended(&self) {
        let ended = self.state.borrow().spawner.events();
        for event in ended {
            // The launch is over: its door retires with it, so the app's next
            // window there starts a process again.
            self.state.borrow_mut().held.remove(&event.id);
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
            // An app the session closed for memory did not stop working:
            // the notice says which it was, so the person does not read a
            // closing they caused as a crash.
            let closed = match event.end.memory() {
                Some(Cause::Kernel) => Closed::Memory,
                Some(Cause::Runaway) => Closed::Runaway,
                None => Closed::Crashed,
            };
            if let Some(desktop) = self.desktop.borrow().clone() {
                let notice = desktop.with_policy(|policy| {
                    policy.stopped(asked.as_deref().unwrap_or(&event.app), &event.title, closed)
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

    /// What a launch does about the app maybe already showing in
    /// `workspace`: `Some(new_window)` to start it, `None` when the window it
    /// has was raised instead, and nothing starts.
    ///
    /// A launch is decided against the windows on the workspace in front,
    /// which is the one it belongs to. A launch into another workspace starts
    /// its own process, as it always has: that workspace's windows are not
    /// showing.
    fn starting(&self, launch: &Launch, workspace: &Workspace) -> Option<bool> {
        let Some(desktop) = self.desktop.borrow().clone() else {
            return Some(false);
        };
        if workspace.is_workspace() && self.active_workspace().id != workspace.id {
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

    /// The workspace a launch from the desktop belongs to: the one in front,
    /// with what the person granted apps in it and the policy that decides
    /// what they may do in them.
    fn active_workspace(&self) -> Workspace {
        let Some(desktop) = self.desktop.borrow().clone() else {
            return Workspace::under(&runtime_dir(), String::new());
        };
        let handle = desktop.with_policy(|policy| policy.handle().clone());
        let person = self.state.borrow().person.clone();
        let mut launched = handle.read(|tree| {
            tree.active_workspace().map_or_else(
                || Workspace::under(&runtime_dir(), String::new()),
                Self::launching_into,
            )
        });
        launched.person = person;
        launched
    }

    /// The workspace named `workspace`, with its grants when the desktop
    /// holds it and none when it does not: a launch into a workspace this
    /// device does not show gets the app's manifest permissions alone.
    #[must_use]
    pub fn workspace_named(&self, workspace: &str) -> Workspace {
        let person = self.state.borrow().person.clone();
        let Some(desktop) = self.desktop.borrow().clone() else {
            return Workspace::under(&runtime_dir(), workspace.to_owned());
        };
        let handle = desktop.with_policy(|policy| policy.handle().clone());
        let mut launched = handle.read(|tree| {
            tree.workspaces
                .iter()
                .find(|held| held.id().to_string() == workspace)
                .map_or_else(
                    || Workspace::under(&runtime_dir(), workspace.to_owned()),
                    Self::launching_into,
                )
        });
        launched.person = person;
        launched
    }

    /// `shown` as a launch into it is planned: what the person granted apps
    /// there, and the policy their authority in it is projected from.
    fn launching_into(shown: &tilo_desktop_model::Workspace) -> Workspace {
        let mut launched = Workspace::under(&runtime_dir(), shown.id().to_string());
        launched.grants = shown.launch_grants(unix_seconds());
        launched.policy = Some(shown.policy.clone());
        launched
    }

    /// Start `launch` in `workspace`, unless the app already has a window
    /// there: that one is raised and nothing starts.
    ///
    /// # Errors
    /// The launch starts nothing, or the process could not be started.
    pub fn start(&self, launch: &Launch, workspace: &Workspace) -> Result<(), DoorError> {
        let Some(new_window) = self.starting(launch, workspace) else {
            return Ok(());
        };
        self.start_window(launch, workspace, new_window)
    }

    /// Start `launch` as a window of its own, beside the tab its app already
    /// has: what the app's own ask for another window does. A window the app
    /// has is never raised instead, since this window is meant to be new.
    ///
    /// # Errors
    /// The launch starts nothing, or the process could not be started.
    pub fn start_beside(&self, launch: &Launch, workspace: &Workspace) -> Result<(), DoorError> {
        self.start_window(launch, workspace, true)
    }

    /// `workspace` as this launch starts in it: when the launch opens a
    /// package there, the workspace is asked to make that package an author
    /// of its own records first, and read again with what it granted, so the
    /// process starts with the authorship it writes by. A launch into no
    /// workspace, and one whose app the workspace already makes an author,
    /// are left as they are.
    fn authored(&self, launch: &Launch, workspace: &Workspace) -> Workspace {
        let Some(app) = launch.package() else {
            return workspace.clone();
        };
        if !workspace.is_workspace() || workspace.grants.authors(app) {
            return workspace.clone();
        }
        let authorship = self.state.borrow().authorship.clone();
        let Some(authorship) = authorship else {
            return workspace.clone();
        };
        authorship(&workspace.id, app);
        let refreshed = self.workspace_named(&workspace.id);
        if refreshed.grants.authors(app) {
            refreshed
        } else {
            workspace.clone()
        }
    }

    /// Start `launch` in `workspace`: in the process the app already runs
    /// there when that one opens the app's windows, and otherwise in a process
    /// of its own, which holds the app's door for its workspace until it ends.
    fn start_window(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        new_window: bool,
    ) -> Result<(), DoorError> {
        let authored = self.authored(launch, workspace);
        let workspace = &authored;
        let planned = {
            let state = self.state.borrow();
            let context = PlanContext {
                environment: &state.environment,
                workspace,
                launcher: state.launcher.clone(),
                identity: state.identity.clone(),
                authority: Some(&state.authority),
                render_node: state.render_node.clone(),
                new_window,
            };
            Planned::of(launch, &context)
        };
        let mut planned =
            planned.map_err(|error| DoorError::Failed(format!("{}: {error}", launch.id())))?;
        // The tool this launch opens is told what to show: only a package is
        // given it, as only a package is spliced through the launcher.
        if let (Some(context), Kind::Package) =
            (self.state.borrow().tool_context.clone(), planned.kind)
        {
            planned
                .env
                .insert(runtime_portal::tools::TOOL_CONTEXT_ENV.to_owned(), context);
        }
        let asked = launch.id();
        let Some(doors) = self.state.borrow().doors.clone() else {
            return self.spawn(&planned, &asked, &workspace.id, None);
        };
        // The app's own window with nothing named to open is the one its
        // running process opens itself, as it opens any later one.
        let routable =
            planned.kind == Kind::Package && matches!(launch, Launch::Package { open: None, .. });
        let alone = self.start_alone(&planned, &asked, &workspace.id);
        let reserved = doors.route_or_reserve(&planned.app, &workspace.id, routable, &alone);
        let reservation = match reserved {
            // The process running the app opens this window itself: nothing
            // starts.
            Err(Routed::Opened) => {
                tracing::info!(app = %planned.app, "the app's running process opens the window");
                return Ok(());
            }
            // No process of the app serves the workspace: this launch starts
            // one.
            Err(Routed::NotRunning) | Ok(None) => None,
            Ok(reservation) => reservation,
        };
        let door = doors.hold(&planned.app, &workspace.id);
        let started = self.spawn(&planned, &asked, &workspace.id, door);
        if started.is_ok()
            && let Some(reservation) = reservation
        {
            // The launch holds its door from its start: the reservation is
            // spent, not given up.
            reservation.launched();
        }
        started
    }

    /// How a window that must start a process of its own starts it: what a
    /// window asked of a process gone to sleep or built before the door does.
    fn start_alone(
        &self,
        planned: &Planned,
        asked: &str,
        workspace: &str,
    ) -> impl Fn() -> StartAlone {
        let me = self.state.borrow().me.clone();
        let (planned, asked, workspace) = (planned.clone(), asked.to_owned(), workspace.to_owned());
        move || {
            let (me, planned, asked, workspace) = (
                me.clone(),
                planned.clone(),
                asked.clone(),
                workspace.clone(),
            );
            Box::new(move || {
                let Some(apps) = me.upgrade() else {
                    return;
                };
                if let Err(error) = apps.spawn(&planned, &asked, &workspace, None) {
                    tracing::warn!(%error, app = %planned.app, "an app did not start");
                }
            })
        }
    }

    /// Start `planned` with the door `door`, which the launch holds until it
    /// ends, and write it down as a launch of this session before the app can
    /// reach a service of it.
    fn spawn(
        &self,
        planned: &Planned,
        asked: &str,
        workspace: &str,
        door: Option<HeldDoor>,
    ) -> Result<(), DoorError> {
        let launches = self.state.borrow().launches.clone();
        let mut state = self.state.borrow_mut();
        let started = state.spawner.start_with(planned, door, &|pid| {
            if let Some(launches) = &launches {
                // The launcher binds the session's documents store into a
                // package's sandbox and into nothing else, so a package is
                // the launch whose files are answered with paths in the store.
                launches.started(pid, &planned.app, workspace, !planned.kind.is_host());
            }
        });
        let started = match started {
            Ok(started) => started,
            Err(error) => return Err(DoorError::Failed(error.to_string())),
        };
        let (started, door) = started;
        state.running.insert(started.id, asked.to_owned());
        if let Some(door) = door {
            state.held.insert(started.id, door);
        }
        drop(state);
        tracing::info!(
            app = %planned.app,
            pid = started.pid,
            "started {}",
            planned.describe()
        );
        Ok(())
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
        match (open, app, provider_workspace(&target.provider)) {
            (true, Some(app), Some(workspace)) => {
                let workspace = self.workspace_named(workspace);
                self.start(
                    &Launch::Package {
                        app: app.to_owned(),
                        open: None,
                    },
                    &workspace,
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
    fn open_uri(&self, workspace: &str, uri: &str) -> Result<(), DoorError> {
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
        let workspace = self.workspace_named(workspace);
        self.start(&launch, &workspace)
    }

    fn start_host_app(&self, desktop_id: &str) -> Result<(), DoorError> {
        // One of the machine's own apps runs unconfined, so it is launched
        // into no workspace: it is told nothing of Tilo's authority.
        self.start(
            &Launch::Host {
                desktop_id: desktop_id.to_owned(),
                open: None,
            },
            &Workspace::under(&runtime_dir(), String::new()),
        )
    }
}

/// `launch` as this machine can start it: a bare name is the app as a window
/// announced it — an X11 client's class, a desktop entry's `StartupWMClass`,
/// a package's own identity — so it starts the way a saved window starts, as
/// an installed package or as the machine's app that opens windows under it.
fn as_this_machine_knows_it(launch: Launch) -> Launch {
    if let Launch::Package { app, open: None } = &launch
        && let Some(started) = launch_of(app)
    {
        return started;
    }
    launch
}

/// How `app` starts again here: an installed Tilo package through the
/// launcher, or one of the machine's own applications through the entry whose
/// identities hold the name its window announced. `None` when this machine
/// has neither.
pub(crate) fn launch_of(app: &str) -> Option<Launch> {
    if let Some(package) = installed_package(app) {
        return Some(Launch::Package {
            app: package,
            open: None,
        });
    }
    let scan = desktop_entry::cache::cached_scan();
    let entry = scan.entries.iter().find(|entry| {
        entry.id.eq_ignore_ascii_case(app)
            || entry
                .window_identities()
                .iter()
                .any(|identity| identity.eq_ignore_ascii_case(app))
    })?;
    Some(Launch::Host {
        desktop_id: entry.id.clone(),
        open: None,
    })
}

/// The package id `app` is installed under: the id itself, or the package
/// whose short name it is.
fn installed_package(app: &str) -> Option<String> {
    if let Ok(Some(installed)) = tilo_installed_apps::read_one_from_env(app) {
        return Some(installed.package_id);
    }
    if app.contains('.') {
        return None;
    }
    let installed = tilo_installed_apps::read_from_env().ok()?;
    installed
        .into_iter()
        .find(|package| {
            tilo_app_launch::package_identities(&package.package_id)
                .iter()
                .any(|identity| identity.eq_ignore_ascii_case(app))
        })
        .map(|package| package.package_id)
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

/// The time grants are decided at: seconds since the epoch, as the workspace
/// stores them.
fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_app_launch::{End, Event};

    /// The desktop the apps are started from, laid out as wide as an output:
    /// what its notice tabs draw on.
    fn desk() -> DesktopHost {
        let (desktop, _strips) = tilo_surface_host::desktop(7, 1.0);
        desktop.with_widgets(|widgets| {
            widgets.set_placement(tilo_surface_host::Placement {
                origin: (0, 0),
                size: (1200, 800),
                scale: 1.0,
            });
        });
        desktop
    }

    /// What the desktop shows after `end` ended the app `asked`, which this
    /// session started as `running`.
    fn showed_after(end: End) -> Vec<String> {
        let apps = Apps::with_launcher(SessionEnvironment::from_process(), None, None);
        let desktop = desk();
        apps.attach(desktop.clone());
        apps.state
            .borrow_mut()
            .running
            .insert(1, "example.Canvas".to_owned());
        apps.state.borrow().spawner.report(Event {
            id: 1,
            app: "example.Canvas".to_owned(),
            title: "Canvas".to_owned(),
            end,
        });
        apps.follow();
        desktop.with_widgets(|widgets| {
            widgets.relayout();
            ui::a11y::texts(widgets.host_mut().root_widget())
        })
    }

    #[test]
    fn an_app_closed_for_memory_says_so_instead_of_crashing() {
        let closed = showed_after(End::signalled(9).remembering(Cause::Kernel));
        assert!(
            closed
                .iter()
                .any(|text| text.contains("was closed to free memory")),
            "the notice says the session ended it: {closed:?}"
        );
        assert!(
            !closed.iter().any(|text| text.contains("stopped working")),
            "a closing the session did is not a crash: {closed:?}"
        );
        assert!(
            closed.iter().any(|text| text == "Reopen"),
            "the notice offers a way back: {closed:?}"
        );

        let runaway = showed_after(End::signalled(9).remembering(Cause::Runaway));
        assert!(
            runaway
                .iter()
                .any(|text| text.contains("was using too much memory")),
            "a runaway app is told apart: {runaway:?}"
        );

        let crashed = showed_after(End::signalled(9));
        assert!(
            crashed
                .iter()
                .any(|text| text.starts_with("Canvas stopped working")),
            "without a cause the same end is still a crash: {crashed:?}"
        );
    }
}
