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
use std::sync::Arc;

use tilo_action_broker::collab_app_broker::AppBrokerState;
use tilo_action_broker::services::LaunchPlace;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, Caller, InvokeArgument, InvokeRequest,
    RemoteActionTarget, application_open_target, application_provider,
};
use tilo_actions::{ActionDescriptor, DoorError, ExecutionLocus, InvocationId};
use tilo_app_launch::routing;
use tilo_app_launch::{
    Answer, Authority, Cause, Doors as WindowDoors, HeldDoor, Instance, Kind, Launch, PlanContext,
    Planned, Routed, SessionEnvironment, Shown, Spawner, StartAlone, Workspace,
};
use tilo_desktop_search::LaunchPlace as OpenPlace;
use tilo_desktop_search::providers::PublishedActions;
use tilo_desktop_search::providers::jobs::JobEntry;
use tilo_desktop_surface::outcome::{Closed, Outcome};
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
    /// The compositor's X11 displays, by workspace: the display a launch into
    /// `workspace` starts its app on.
    x11: Option<tilo_compositor::X11Displays>,
    spawner: Spawner,
    /// The launches running now, by the id the spawner gave each: what a
    /// crash notice's Reopen asks for again.
    running: BTreeMap<u64, String>,
    /// The `tilo-launcher` every app starts through, a package or one of the
    /// machine's own; `None` when none is installed, and then no app can
    /// start.
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
    /// The session's action broker, once it starts one: what an action an
    /// app publishes is delivered through, so a row or a card that names one
    /// reaches the app whether it runs or has to start for it.
    broker: Option<Arc<AppBrokerState>>,
    /// What wakes the session's loop: a delivery completes on the broker's
    /// own runtime, and the loop reads its answer on its next pass.
    wake: Option<tilo_service::Wake>,
}

/// What a launch does about the app it names: the window the app has, or is
/// on its way to, comes forward, or the app starts.
enum Beginning {
    /// The window the app has, or the place one is on its way to, came
    /// forward: nothing starts.
    Raised,
    /// The app starts, with a window of its own when `new_window`.
    Start { new_window: bool },
    /// The app starts into the tab `tab`, a saved window's that nothing was
    /// asking for: the window the person left open fills it.
    StartIn { tab: u64 },
}

impl Apps {
    /// The apps `environment` starts, waking `wake`'s loop when one ends.
    #[must_use]
    pub fn new(environment: SessionEnvironment, wake: Option<tilo_service::Wake>) -> Self {
        let launcher = PlanContext::find_launcher();
        if launcher.is_none() {
            tracing::warn!("no tilo-launcher is installed: no app can start here");
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
                x11: None,
                spawner: Spawner::new(wake.clone()),
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
                broker: None,
                wake,
            }),
        }
    }

    /// The compositor bound its Wayland socket `socket` and its X11 displays
    /// `x11`: apps started from now on connect to these, whatever the session
    /// itself was started with. A display is asked for per workspace, when an
    /// app is launched into it.
    pub fn set_displays(
        &self,
        socket: PathBuf,
        x11: Option<tilo_compositor::X11Displays>,
        session_bus: Option<String>,
    ) {
        let mut state = self.state.borrow_mut();
        state.environment.wayland_socket = Some(socket);
        state.x11 = x11;
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

    /// The session's action broker: from here on an action an app publishes
    /// is delivered through it, so a row, a link or a card that names one
    /// opens the app on it, starting the app when it is not up.
    pub fn lend_broker(&self, broker: Option<Arc<AppBrokerState>>) {
        self.state.borrow_mut().broker = broker;
    }

    /// Hand the app action `target` to the app that publishes it, as the
    /// person at this seat: the broker starts the app when it is closed,
    /// waits for it to publish the action and delivers the call, on its own
    /// runtime. What the app shows for it — the window a `open` reveals —
    /// arrives the way every app's window does.
    fn deliver(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
        workspace: &str,
    ) -> Result<(), DoorError> {
        let (broker, caller, wake) = {
            let state = self.state.borrow();
            let Some(broker) = &state.broker else {
                return Err(no_action_host());
            };
            (
                Arc::clone(broker),
                Caller::local_person(state.person.clone())
                    .in_new_execution()
                    .in_workspace(workspace),
                state.wake.clone(),
            )
        };
        let request = InvokeRequest {
            invocation: InvocationId::new(),
            target,
            argument: InvokeArgument::Json(arguments),
            context: None,
            confirmation: None,
            revision: None,
            on_behalf_of: None,
            wait_ms: None,
            appearance: None,
        };
        // The delivery outlives this pass: its answer runs on the thread that
        // completed it, and wakes this loop, which reports what a person's own
        // action could not do.
        let resource = request.target.address.resource.clone();
        let action = request.target.address.action.clone();
        broker
            .action_invoke_local_person(&caller, &request)
            .then(move |answer| {
                if let Some(Err(error)) = answer {
                    tracing::warn!(%error, %resource, %action, "an app action was not delivered");
                }
                if let Some(wake) = &wake {
                    wake();
                }
            });
        Ok(())
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

    /// The windows the person can see, as the desk last laid them out: the
    /// front tab of each group on the strip in front, and the dialogs
    /// floating over those. `None` while no desk is mounted, when nothing is
    /// known about what is in view.
    pub fn windows_in_view(&self) -> Option<Vec<u64>> {
        let desktop = self.desktop.borrow().clone()?;
        let layout = desktop.with_policy(|policy| policy.layout());
        Some(
            layout
                .shown
                .into_iter()
                .chain(layout.floating)
                .map(|(window, _)| window)
                .collect(),
        )
    }

    /// Put every app this session runs in the tier of what the person can
    /// see: `on_screen` is the process of each window the desk shows.
    pub fn tier_by_visibility(&self, on_screen: &[u32]) {
        self.state.borrow().spawner.tier_by_visibility(on_screen);
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
                    self.start_beside(&request.launch, &workspace, None)
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
            let Some(launch) = Launch::parse(&asked.app) else {
                tracing::warn!(asked = %asked.app, "not a launch this session knows");
                self.could_not_open(
                    asked.tab,
                    &asked.app,
                    &tilo_desktop_surface::i18n::t("answer-why-unknown-app"),
                );
                continue;
            };
            // A window is on its way to its own tab, and a notice starts
            // nothing: neither asks for a process.
            if matches!(launch, Launch::Window { .. } | Launch::Crashed { .. }) {
                continue;
            }
            let launch = as_this_machine_knows_it(launch);
            let workspace = self.active_workspace();
            let started = self.start_at(&launch, &workspace, asked.tab);
            match &started {
                // The window is coming, or the app's own window came forward
                // in place of a second one: the tab it was held in takes it.
                Ok(true) => {}
                Ok(false) => {
                    if let Some(tab) = asked.tab {
                        desktop.with_policy(|policy| policy.release_tab(tab));
                    }
                }
                Err(error) => {
                    tracing::warn!(asked = %asked.app, %error, "an app did not start");
                    self.could_not_open(asked.tab, &asked.app, &error.to_string());
                }
            }
        }
    }

    /// What the person asked for in `tab` did not happen, `why` says why, as
    /// the desk says such things: the tab keeps its place and shows the
    /// answer, with the button that asks for the app again. A launch no tab
    /// holds (a service's own) is answered by whoever asked for it, not
    /// here.
    fn could_not_open(&self, tab: Option<u64>, asked: &str, why: &str) {
        use tilo_desktop_surface::i18n::{t, t_args};
        let (Some(tab), Some(desktop)) = (tab, self.desktop.borrow().clone()) else {
            return;
        };
        let name = desktop.with_policy(|policy| policy.name_of(asked));
        let outcome = Outcome::new(t_args("answer-app-not-opened", &[("app", name.into())]))
            .saying(why)
            .retryable(t("answer-try-again"));
        desktop.with_policy(|policy| policy.failed_at(tab, asked, outcome));
        // The answer is a desktop command like any other: the desk carries it
        // out, and lays its tab out, on its next pass.
        desktop.with_widgets(WidgetHost::relayout);
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

    /// [`Self::open`] for a launch the desk held the tab `instance` for: the
    /// window it opens is that tab. Whether a window is on its way to fill
    /// it: one a running process of the app opens itself is.
    fn start_at(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        instance: Option<u64>,
    ) -> Result<bool, DoorError> {
        match self.starting(launch, workspace, instance) {
            Beginning::Raised => Ok(false),
            Beginning::Start { new_window } => {
                self.start_window(launch, workspace, new_window, instance)
            }
            Beginning::StartIn { tab } => {
                let started = self.start_window(launch, workspace, false, Some(tab));
                // The desk's own tab for this launch holds the place of the
                // window that now fills the tab a saved window waited in: it
                // goes, so the app is not left showing twice.
                if started.is_ok()
                    && let Some(theirs) = instance
                    && theirs != tab
                {
                    let desktop = self.desktop.borrow().clone();
                    if let Some(desktop) = desktop {
                        desktop.with_policy(|policy| policy.release_tab(theirs));
                    }
                }
                started
            }
        }
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
                let asked = asked.unwrap_or_else(|| event.app.clone());
                // A launch that ended before its window came never opened,
                // and says that in the tab the person asked for it in: what
                // stopped in front of them is a window that did not arrive,
                // not one that went.
                let waited = desktop.with_policy(|policy| {
                    policy
                        .anchor_for(&asked)
                        .filter(|(tab, ..)| policy.holds_a_place(*tab))
                        .map(|(tab, ..)| tab)
                });
                if let Some(tab) = waited {
                    self.could_not_open(
                        Some(tab),
                        &asked,
                        &tilo_desktop_surface::i18n::t("answer-why-not-opened"),
                    );
                    continue;
                }
                let notice =
                    desktop.with_policy(|policy| policy.stopped(&asked, &event.title, closed));
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
    /// `workspace`: start it, start it into the tab a saved window of it
    /// waits in, or raise the window it has or the place one is on its way
    /// to.
    ///
    /// A launch is decided against the tabs the desk has for apps on the
    /// workspace in front, which is the one it belongs to, less `instance`:
    /// the tab this launch itself holds, which is its own window's place and
    /// not another launch of the app. A launch into another workspace starts
    /// its own process, as it always has: that workspace's windows are not
    /// showing.
    fn starting(&self, launch: &Launch, workspace: &Workspace, instance: Option<u64>) -> Beginning {
        let Some(desktop) = self.desktop.borrow().clone() else {
            return Beginning::Start { new_window: false };
        };
        if workspace.is_workspace() && self.active_workspace().id != workspace.id {
            return Beginning::Start { new_window: false };
        }
        let Some(app) = Instance::of(
            launch,
            &desktop_entry::cache::cached_scan(&crate::app_names::entry_locales()),
        ) else {
            return Beginning::Start { new_window: false };
        };
        let shown = Shown::new(
            desktop
                .with_policy(|policy| policy.app_tabs_in_front())
                .into_iter()
                .filter(|(tab, _)| Some(*tab) != instance),
        );
        let answer = app.answer(launch, &shown);
        if let Answer::Raise { tab } = answer
            && desktop.with_policy(|policy| policy.focus_tab(tab))
        {
            desktop.with_widgets(WidgetHost::relayout);
            tracing::info!(
                app = %app.id,
                tab,
                "raised the window the app already has, or the place one is on its way to"
            );
            return Beginning::Raised;
        }
        // A window the person left open waits in its own tab for the app, and
        // nothing has asked for it yet: this launch starts the app into that
        // tab — the way the restore opens a saved window — instead of beside
        // it, so one window of the app comes and not two.
        if let Some(tab) = desktop.with_policy(|policy| policy.waiting_tab(&app.id))
            && desktop.with_policy(|policy| policy.adopt_tab(tab, false))
        {
            return Beginning::StartIn { tab };
        }
        Beginning::Start {
            new_window: matches!(answer, Answer::Start { new_window: true }),
        }
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

    /// What a search row names, run as the person's own choice runs it: an
    /// app in the workspace it was published in, and the action to run
    /// there. `place` is where they asked the window of an `open` to go.
    fn invoke_at(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
        place: OpenPlace,
    ) -> Result<(), DoorError> {
        let Some((workspace, _)) = application_provider(&target.provider) else {
            return Err(no_action_host());
        };
        let workspace = workspace.to_owned();
        let app = target
            .address
            .resource
            .strip_prefix(APPLICATION_RESOURCE_PREFIX)
            .map(str::to_owned);
        let open = target.address.action == APPLICATION_OPEN_ACTION && arguments.is_empty();
        match (open, app) {
            (true, Some(app)) => {
                let workspace = self.workspace_named(&workspace);
                let launch = Launch::Package { app, open: None };
                match place {
                    // A tab of the window in front is the desk's to arrange:
                    // the session opens the app the way it opens, and the
                    // desk puts the tab where the person asked.
                    OpenPlace::Focus | OpenPlace::TabHere => self.start(&launch, &workspace),
                    OpenPlace::Beside => self.start_beside(&launch, &workspace, None),
                }
            }
            _ => self.deliver(target, arguments, &workspace),
        }
    }

    /// Start `launch` in `workspace`, unless the app already has a window
    /// there: that one is raised and nothing starts.
    ///
    /// # Errors
    /// The launch starts nothing, or the process could not be started.
    pub fn start(&self, launch: &Launch, workspace: &Workspace) -> Result<(), DoorError> {
        self.start_at(launch, workspace, None).map(|_| ())
    }

    /// Start `launch` as a window of its own, beside the tab its app already
    /// has: what the app's own ask for another window does. A window the app
    /// has is never raised instead, since this window is meant to be new.
    /// `instance` is the tab the window takes, when the session already holds
    /// one for it: the app is told it, so what it reports of the window is
    /// about that tab.
    ///
    /// # Errors
    /// The launch starts nothing, or the process could not be started.
    pub fn start_beside(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        instance: Option<u64>,
    ) -> Result<(), DoorError> {
        self.start_window(launch, workspace, true, instance)
            .map(|_| ())
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
    /// Whether a window comes for the tab held for this launch.
    fn start_window(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        new_window: bool,
        instance: Option<u64>,
    ) -> Result<bool, DoorError> {
        let authored = self.authored(launch, workspace);
        let workspace = &authored;
        let planned = {
            let state = self.state.borrow();
            // The workspace's own X11 display: an app started there connects
            // to a server no other workspace's apps share.
            let mut environment = state.environment.clone();
            environment.x11_display = state
                .x11
                .as_ref()
                .filter(|_| workspace.is_workspace())
                .and_then(|x11| x11.display_for(&workspace.id))
                .map(|number| format!(":{number}"));
            let context = PlanContext {
                environment: &environment,
                workspace,
                launcher: state.launcher.clone(),
                identity: state.identity.clone(),
                authority: Some(&state.authority),
                render_node: state.render_node.clone(),
                new_window,
                continuity_instance: instance,
            };
            Planned::of(launch, &context)
        };
        let mut planned = planned.map_err(|error| DoorError::Failed(error.to_string()))?;
        // The tool this launch opens is told what to show: only a package is
        // given it.
        if let (Some(context), Kind::Package) =
            (self.state.borrow().tool_context.clone(), planned.kind)
        {
            planned
                .env
                .insert(runtime_portal::tools::TOOL_CONTEXT_ENV.to_owned(), context);
        }
        let asked = launch.id();
        let Some(doors) = self.state.borrow().doors.clone() else {
            return self
                .spawn(&planned, &asked, &workspace.id, None)
                .map(|()| true);
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
                return Ok(true);
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
        started.map(|()| true)
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
    /// What the apps publish, read from the session's action broker: the one
    /// host every app publishes on, whatever it is built with. A tilolang app
    /// publishes through its host and an app on the SDK through its own
    /// connection, both on the channel they joined the workspace with, and
    /// the broker keeps the last surface an app published after it closes,
    /// which is why a closed app's actions are offered too: choosing one
    /// starts the app and delivers the call there. Read when a search opens.
    fn published(&self, workspace: &str) -> Vec<PublishedActions> {
        let state = self.state.borrow();
        let Some(broker) = &state.broker else {
            return Vec::new();
        };
        // By app, workspace and address: the broker holds a surface per
        // record, and an action two of them carry is still the one action
        // that workspace addresses.
        let mut listed: BTreeMap<(String, String), BTreeMap<(String, String), ActionDescriptor>> =
            BTreeMap::new();
        for surface in broker.published_surfaces() {
            let published_in = &surface.route.cluster_id;
            if !workspace.is_empty() && published_in != workspace {
                continue;
            }
            let actions = listed
                .entry((surface.app_id.clone(), published_in.clone()))
                .or_default();
            for action in &surface.actions {
                // A live surface is listed before the one retained from
                // before the app closed, and is the one to keep.
                actions
                    .entry((action.resource.clone(), action.action.clone()))
                    .or_insert_with(|| action.descriptor(ExecutionLocus::ClientLocal));
            }
        }
        listed
            .into_iter()
            .map(|((app, workspace), actions)| PublishedActions {
                app,
                workspace,
                actions: actions.into_values().collect(),
                // An exported action names its resource's kind but not what
                // the resource is to a person ([`PublishedActions::types`]):
                // that stays with the app's own host.
                types: BTreeMap::new(),
            })
            .collect()
    }

    /// The broker follows each invocation it delivered, but offers no listing
    /// that names the app, workspace and action label a job's row reads, so
    /// there is nothing to list here yet.
    fn jobs(&self) -> Vec<JobEntry> {
        Vec::new()
    }

    /// The workspace's `open` of an app is the app's own action on its own
    /// resource, which starts it, and is carried out here. Every other
    /// action an app publishes is the app's to run: it goes to the session's
    /// broker, which starts the app when it is closed, waits for it to
    /// publish the action and delivers the call there.
    fn invoke(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), DoorError> {
        self.invoke_at(target, arguments, OpenPlace::Focus)
    }

    /// Open `app` where the person asked it, through the same run of the
    /// workspace's `open` as [`Self::invoke`]: `place` is what the modifier
    /// they held over the row asked of the window.
    fn open_application(
        &self,
        workspace: &str,
        app: &str,
        place: OpenPlace,
    ) -> Result<(), DoorError> {
        self.invoke_at(
            application_open_target(workspace, app),
            serde_json::Map::new(),
            place,
        )
    }

    fn cancel(&self, _app: &str, _workspace: &str, _invocation: &str) -> Result<(), DoorError> {
        // A job's row carries the invocation the broker follows, and no row
        // is listed yet ([`Self::jobs`]), so nothing names one to stop.
        Err(no_action_host())
    }

    /// Open `uri` in the app that takes it: a document's own app when it is
    /// a file on this machine, else the app that claims its scheme.
    fn open_uri(&self, workspace: &str, uri: &str) -> Result<(), DoorError> {
        let installed = tilo_installed_apps::read_from_env().unwrap_or_default();
        let entries = desktop_entry::cache::cached_scan(&crate::app_names::entry_locales());
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
    let scan = desktop_entry::cache::cached_scan(&crate::app_names::entry_locales());
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

/// The resource kinds `app`'s installed package declares for the records it
/// keeps, from its signed manifest: what a workspace takes a record it starts
/// under ([`crate::engine::Engine`] starting an app's first record). `None`
/// for an app this device has not installed.
pub(crate) fn declared_resource_kinds(app: &str) -> Option<Vec<String>> {
    let package = installed_package(app)?;
    let installed = tilo_installed_apps::read_one_from_env(&package)
        .ok()
        .flatten()?;
    Some(installed.declared_resource_kinds())
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
