//! Opening an app: the launches the desktop asks for, planned into the process that starts them
//! and started in the session's own environment.
//!
//! The desktop surface knows an app by name but cannot start a process: it queues the launch for
//! the session ([`tilo_app_launch`]), which plans it against the workspace in front and the
//! displays this session bound, and starts it without waiting. Search rows, files and links open
//! the same way, and an app that stops without choosing to is offered again where its window was.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use crate::platform::surfaces::{DesktopHost, WidgetHost};
use tilo_action_broker::collab_app_broker::AppBrokerState;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, Caller, InvokeArgument, InvokeRequest,
    RemoteActionTarget, application_open_target, application_provider, provider_workspace,
};
use tilo_actions::{ActionDescriptor, DoorError, ExecutionLocus, InvocationId};
use tilo_app_launch::routing;
use tilo_app_launch::{
    Answer, Authority, Cause, Doors as WindowDoors, HeldDoor, Instance, Kind, Launch, PlanContext,
    Planned, Routed, SessionEnvironment, Shown, Spawner, StartAlone, Workspace,
};
use tilo_broker_core::services::LaunchPlace;
use tilo_desktop_search::LaunchPlace as OpenPlace;
use tilo_desktop_search::providers::PublishedActions;
use tilo_desktop_search::providers::jobs::JobEntry;
use tilo_desktop_surface::outcome::{Closed, Outcome};

use crate::agents::Kept;
use crate::search::AppActions;

/// The agent the person's workspace records name: a participant without a window, which the session
/// runs for the person who keeps it.
const AGENT_PACKAGE: &str = "so.tilo.agent";

/// The apps this session starts, and everything that asks it for one.
pub struct Apps {
    /// The desktop the launches come from, once the session has mounted it: what a launch reads its
    /// workspace from, and where a stopped app's notice goes.
    desktop: RefCell<Option<DesktopHost>>,
    state: RefCell<State>,
}

/// What lets a package author its own records in the workspace it opens in,
/// called with the workspace's id and the package's.
type Authorship = Rc<dyn Fn(&str, &str)>;

/// What starting an app needs besides the launch itself.
struct State {
    environment: SessionEnvironment,
    /// The compositor's X11 displays, by workspace: the display a launch into `workspace` starts
    /// its app on.
    x11: Option<tilo_compositor::X11Displays>,
    spawner: Spawner,
    /// The launches running now, by the id the spawner gave each: what a crash notice's Reopen asks
    /// for again.
    running: BTreeMap<u64, String>,
    /// The `tilo-launcher` every app starts through, a package or one of the machine's own; `None`
    /// when none is installed, and then no app can start.
    launcher: Option<PathBuf>,
    /// The identity record this workspace's apps are given, when the session resolved one.
    identity: Option<PathBuf>,
    /// The person at this device, by the id the workspaces' governance names them by: whose
    /// authority each app it starts is handed.
    person: String,
    /// What the person's authority in a workspace is published as, for each app this session starts
    /// there, and this session's hold on it.
    authority: Authority,
    /// The desk's revision the published authority was last brought up to: nothing is republished
    /// while the desk has not moved.
    authority_at: Option<u64>,
    /// The GPU the compositor renders on, for an app to render on the same.
    render_node: Option<PathBuf>,
    /// The compositor drew its first frame with no GPU to import from: its apps were pinned to the
    /// software renderer it draws with ([`Apps::follow_render_node`]).
    software: bool,
    /// The session's launch records, once it has lent them: what its broker admits a connection by,
    /// and so what each launch it makes is written into.
    launches: Option<std::sync::Arc<crate::Launches>>,
    /// The window doors of the apps this session starts, once it lent them: one process per app per
    /// workspace opens that app's later windows itself.
    doors: Option<Rc<WindowDoors>>,
    /// This session, while the doors are lent: a window that must start a process of its own starts
    /// it here.
    me: Weak<Apps>,
    /// What lets a package author its own records in the workspace it opens in, once it is lent:
    /// asked before the process is planned, so the app starts with the authorship rather than
    /// waiting for a window to reopen.
    authorship: Option<Authorship>,
    /// The door of each launch still running, by the launch's id: the end of the launch retires it,
    /// so the app's next window there starts a process again.
    held: BTreeMap<u64, HeldDoor>,
    /// What the tool a launch opens was asked to show, for the one launch being planned
    /// (`TILO_TOOL_CONTEXT`): set only while a launch a service asked for is being started, and
    /// read where it is planned.
    tool_context: Option<String>,
    /// The session's action broker, once it starts one: what an action an app publishes is
    /// delivered through, so a row or a card that names one reaches the app whether it runs or has
    /// to start for it.
    broker: Option<Arc<AppBrokerState>>,
    /// What wakes the session's loop: a delivery completes on the broker's own runtime, and the
    /// loop reads its answer on its next pass.
    wake: Option<tilo_service::Wake>,
    /// The agents this device runs because the person here keeps them, by workspace and id: what
    /// each was started with, and the launch running it while one does.
    agents: BTreeMap<(String, String), AgentProcess>,
    /// The session's services, once it lent them: a launch asks them whether one runs the app's
    /// model, and if so names the resource it publishes for the app, so the app launches hosted
    /// (`docs/architecture/apps.md` §8 step 2, `one-app-model.md` §4).
    services: Option<Rc<RefCell<crate::Services>>>,
    /// The person's arrangement, once it lent it: an app whose model the session runs itself is
    /// opened by placing its root on the canvas, not by starting a window process of its own
    /// (`docs/designs/app-language/one-app-model.md` §3).
    arrangement: Option<Rc<RefCell<crate::arrangement::Arrangement>>>,
    /// The tilolang hosts, once the session lent its doors: one per workspace and author, running
    /// the instances of the tilolang apps' windows.
    hosts: Option<Arc<tilo_app_launch::tilolang_hosts::Hosts>>,
    /// The connectors of the apps' outside services: what a hosted instance reaches its origin by.
    connectors: Option<Rc<crate::connectors::ConnectorHost>>,
}

/// An agent this device started, as the record of it the launch pass keeps.
struct AgentProcess {
    /// The name and model it was started with: a change to either is what starts it again, so a
    /// process that ended is not started in a loop.
    wanted: (String, Option<String>),
    /// The launch running it; `None` once it ended, and after it never started.
    launch: Option<u64>,
}

/// What a launch does about the app it names: the window the app has, or is on its way to, comes
/// forward, or the app starts.
enum Beginning {
    /// The window the app has, or the place one is on its way to, came forward: nothing starts.
    Raised,
    /// The app starts, with a window of its own when `new_window` .
    Start { new_window: bool },
    /// The app starts into the tab `tab` , a saved window's that nothing was asking for: the window
    /// the person left open fills it.
    StartIn { tab: u64 },
}

impl Apps {
    /// The apps `environment` starts, waking `wake` 's loop when one ends.
    #[must_use]
    pub fn new(environment: SessionEnvironment, wake: Option<tilo_service::Wake>) -> Self {
        let launcher = PlanContext::find_launcher();
        if launcher.is_none() {
            tracing::warn!("no tilo-launcher is installed: no app can start here");
        }
        Self::with_launcher(environment, wake, launcher)
    }

    /// As [`Self::new`], starting packages through `launcher` : a session built beside its own
    /// launcher (a test's) knows where it is.
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
                software: false,
                launches: None,
                doors: None,
                me: Weak::new(),
                authorship: None,
                held: BTreeMap::new(),
                tool_context: None,
                broker: None,
                wake,
                agents: BTreeMap::new(),
                services: None,
                arrangement: None,
                hosts: None,
                connectors: None,
            }),
        }
    }

    /// The compositor bound its Wayland socket `socket` and its X11 displays `x11` : apps started
    /// from now on connect to these, whatever the session itself was started with. A display is
    /// asked for per workspace, when an app is launched into it.
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

    /// The render node of the GPU the compositor draws on: an app the person granted `gpu` is
    /// pinned to it, as it is pinned to the same driver, instead of choosing whichever GPU wgpu
    /// offers it.
    pub fn set_render_node(&self, node: Option<PathBuf>) {
        self.state.borrow_mut().render_node = node;
    }

    /// Take the compositor's render node once it has one: its backend names the GPU clients
    /// import on when it starts importing dma-bufs, which can be after the session first hands
    /// the launcher its node. Until then a launch had none, and an app that opens the GPU itself
    /// got no pin at all (on a machine without a GPU, every installed driver in its loader).
    ///
    /// Once the compositor has drawn (`drawn`) with no GPU to import from, it draws with the
    /// software renderer, and so must its apps: the launcher of each is handed the software pin
    /// (lavapipe alone, Mesa's EGL vendor, no device-select layer), which a launch with no render
    /// node takes into its sandbox (`launcher::gpu::inherited_software_pin`). Without it an app
    /// that opens the GPU itself loaded every installed driver, and Chat crashed in a VM without
    /// 3D.
    pub fn follow_render_node(&self, node: impl FnOnce() -> Option<PathBuf>, drawn: bool) {
        let mut state = self.state.borrow_mut();
        if state.render_node.is_some() || state.software {
            return;
        }
        state.render_node = node();
        if state.render_node.is_none() && drawn {
            state.software = true;
            #[cfg(target_os = "linux")]
            match launcher::gpu::software_pin(&launcher::gpu::IcdSearch::default()) {
                Ok(pin) => {
                    for (key, value) in pin.env() {
                        state
                            .environment
                            .forwarded
                            .entry(key.to_owned())
                            .or_insert(value);
                    }
                    tracing::info!(
                        vulkan = %pin.vulkan_icd_json.display(),
                        "no GPU: the apps opened from here on render with the software renderer"
                    );
                }
                Err(why) => tracing::info!(%why, "no GPU, and no software renderer to pin apps to"),
            }
        }
    }

    /// The person at this device, by the id the workspaces' governance names them by: whose
    /// authority in a workspace each app started from here on is handed, so it decides their
    /// actions as the workspace's policy says.
    pub fn set_person(&self, person: impl Into<String>) {
        self.state.borrow_mut().person = person.into();
    }

    /// The session's launch records: each app started from here on is written into them, so its
    /// connection to the session's services is admitted for the launch that made it.
    pub fn lend_launches(&self, launches: std::sync::Arc<crate::Launches>) {
        self.state.borrow_mut().launches = Some(launches);
    }

    /// The session's services: from here on a launch asks them whether one runs the app's model,
    /// and names the resource it publishes for the app where one does, so the app launches hosted
    /// (`docs/architecture/apps.md` §8 step 2).
    pub fn lend_services(&self, services: Rc<RefCell<crate::Services>>) {
        self.state.borrow_mut().services = Some(services);
    }

    /// The person's arrangement: from here on opening an app whose model the session runs itself
    /// places its root on the canvas as a view of the session's record, in the workspace the
    /// person opened it in, rather than starting a window process of its own
    /// (`docs/designs/app-language/one-app-model.md` §3, "Windows belong to the shell").
    pub fn lend_arrangement(&self, arrangement: Rc<RefCell<crate::arrangement::Arrangement>>) {
        self.state.borrow_mut().arrangement = Some(arrangement);
    }

    /// The resource address the session's document host publishes `app` 's record at, when a
    /// service runs its model: whether this launch is hosted follows from that, and no per-app code
    /// says otherwise.
    fn hosted(&self, app: &str) -> Option<String> {
        let services = self.state.borrow().services.clone()?;
        services.borrow().hosted(app)
    }

    /// Whether a service of this session runs `app`'s model: its windows are places of the shell's
    /// arrangement, drawn from the session's reading, and no window process of it starts
    /// (`docs/designs/app-language/one-app-model.md` §3).
    #[must_use]
    pub fn serves(&self, app: &str) -> bool {
        self.hosted(app).is_some()
    }

    /// Place the root of the app `app` whose model the session runs, on the canvas of `workspace`,
    /// as the shell's own place: the place draws the app's default view over the session's
    /// reading of its root, and no window process of the app starts
    /// (`docs/designs/app-language/one-app-model.md` §3). `view` empty shows the root's primary
    /// view. Returns the place's id, or `None` for an app no service runs — which launches as
    /// every app does — and for one the workspace already shows, which came forward instead.
    fn place_served(
        &self,
        app: &str,
        view: &str,
        workspace: &Workspace,
    ) -> Option<Result<Option<u64>, DoorError>> {
        let root = self.hosted(app)?;
        let arrangement = self.state.borrow().arrangement.clone();
        let Some(arrangement) = arrangement else {
            // No arrangement to place on: the app opens its own window as it always did.
            return None;
        };
        // The place is the person's own arranging step, in the workspace they opened the app in:
        // one attributed, undoable change of the desk, exactly as typing what the app shows and
        // pressing it would be (shell.md §8).
        let slot = self.workspace_slot(&workspace.id)?;
        let person = self.state.borrow().person.clone();
        let placed = arrangement
            .borrow_mut()
            .place_root(&person, slot, &root, view);
        Some(placed.map_err(DoorError::Failed))
    }

    /// Stand the root of the served app `app` on the canvas of the workspace `workspace` (a slot),
    /// as the place its window would have been: what a device that left the app open restores,
    /// the place the desk draws from the session's reading instead of a window process. Whether a
    /// place stands there now.
    pub(crate) fn restore_served(&self, app: &str, workspace: u64) -> bool {
        let view = "";
        let Some(root) = self.hosted(app) else {
            return false;
        };
        let Some(arrangement) = self.state.borrow().arrangement.clone() else {
            return false;
        };
        let person = self.state.borrow().person.clone();
        matches!(
            arrangement
                .borrow_mut()
                .place_root(&person, workspace, &root, view),
            Ok(Some(_))
        )
    }

    /// The desk's slot for the workspace `id` : where a place the session makes in that workspace
    /// stands. `None` for a workspace this desk does not hold.
    fn workspace_slot(&self, id: &str) -> Option<u64> {
        let desktop = self.desktop.borrow().clone()?;
        let handle = desktop.with_policy(|policy| policy.handle().clone());
        handle.read(|tree| {
            tree.workspaces
                .iter()
                .find(|held| held.id().to_string() == id)
                .map(|held| held.slot.0)
        })
    }

    /// The session's action broker: from here on an action an app publishes is delivered through
    /// it, so a row, a link or a card that names one opens the app on it, starting the app when it
    /// is not up.
    pub fn lend_broker(&self, broker: Option<Arc<AppBrokerState>>) {
        self.state.borrow_mut().broker = broker;
    }

    /// Hand the app action `target` to the app that publishes it, as the person at this seat: the
    /// broker starts the app when it is closed, waits for it to publish the action and delivers the
    /// call on its own runtime.
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
        // The delivery outlives this pass: its answer runs on the thread that completed it, and
        // wakes this loop, which reports what a person's own action could not do.
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

    /// The window doors the session keeps for the apps it starts: from here on the first launch of
    /// an app in a workspace hands its process the door, and every later window of that app there
    /// opens in that process.
    ///
    /// A tilolang app's windows hold none of its data: each window's instance runs in the
    /// tilolang host of its workspace and author, reaching its outside service through
    /// `connectors`.
    pub fn lend_doors(
        self: &Rc<Self>,
        doors: Rc<WindowDoors>,
        connectors: Rc<crate::connectors::ConnectorHost>,
    ) {
        let me = Rc::downgrade(self);
        doors.hand_instances_to(Box::new(move |hosted| {
            if let Some(apps) = me.upgrade() {
                apps.host_window(hosted);
            }
        }));
        let mut state = self.state.borrow_mut();
        state.me = Rc::downgrade(self);
        state.hosts = state
            .launcher
            .clone()
            .map(tilo_app_launch::tilolang_hosts::Hosts::new);
        state.doors = Some(doors);
        state.connectors = Some(connectors);
    }

    /// The person's region formats changed: every tilolang host running here is told, so the
    /// instances it runs write the dates, clocks and weeks their windows show again in the new
    /// ones ([`crate::formats`]).
    pub fn formats_changed(&self) {
        let hosts = self.state.borrow().hosts.clone();
        if let Some(hosts) = hosts {
            hosts.formats_changed();
        }
    }

    /// Whether `app`'s windows hold none of its data, their instances run in a tilolang host:
    /// a tilolang app whose model no service of the session runs, in a session with doors and a
    /// launcher to start hosts through.
    fn runs_in_host(&self, app: &str) -> bool {
        let state = self.state.borrow();
        if state.doors.is_none() || state.hosts.is_none() {
            return false;
        }
        drop(state);
        self.hosted(app).is_none() && tilo_app_launch::tilolang_hosts::runs_in_host(app)
    }

    /// Hand the instance of the tilolang window `hosted` to the host of its workspace and author,
    /// with the connections an instance holds: who is at it, its identity service and its app's
    /// origin connector, when the app has one.
    fn host_window(&self, hosted: tilo_app_launch::HostedWindow) {
        let (hosts, person, connectors) = {
            let state = self.state.borrow();
            (
                state.hosts.clone(),
                state.person.clone(),
                state.connectors.clone(),
            )
        };
        let app = hosted.app.clone();
        let instance = hosted.instance;
        let refused = |why: String| {
            tracing::error!(
                app,
                instance,
                why,
                "a tilolang window's instance runs nowhere"
            );
        };
        let Some(hosts) = hosts else {
            return refused("no tilolang host can start here".to_owned());
        };
        // Opened and named for the app and workspace exactly as the launcher opens a launched
        // app's: the identity service signs for that app in that workspace only.
        let identity = match launcher::services::resolve_identity_bind(
            &hosted.app,
            Some(&hosted.workspace),
            false,
        ) {
            Ok(Some(identity)) => identity.host,
            Ok(None) => return refused("the identity service is unavailable".to_owned()),
            Err(error) => return refused(format!("the identity service: {error}")),
        };
        // An app whose types hold an outside service's resources reaches its connector, which
        // holds the network and custody the host has none of.
        let origin = match tilo_app_launch::tilolang_hosts::origin_of(&hosted.app) {
            Ok(None) => None,
            Ok(Some(origin)) => {
                let Some(connectors) = connectors else {
                    return refused("no connector can start here".to_owned());
                };
                let workspace = self.workspace_named(&hosted.workspace);
                match connectors.connection(&hosted.app, &origin, &workspace.id, &|origin| {
                    self.plan_connector(&hosted.app, origin, &workspace)
                }) {
                    Ok(connection) => Some(connection),
                    Err(error) => return refused(error),
                }
            }
            Err(error) => return refused(error),
        };
        hosts.hand_over(tilo_app_launch::tilolang_hosts::Instance {
            app: hosted.app,
            workspace: hosted.workspace,
            instance: hosted.instance,
            person,
            patch: hosted.patch,
            collaboration: hosted.collaboration,
            identity,
            origin,
            opens: hosted.opens,
        });
    }

    /// The connector of `app` for `origin` in `workspace`, planned as the app's own launch with
    /// the origin named and no window: the sandbox the app's manifest declares for that origin.
    ///
    /// # Errors
    /// The package is not installed here, its manifest does not declare `origin`, or no
    /// launcher can start it.
    pub fn plan_connector(
        &self,
        app: &str,
        origin: &str,
        workspace: &Workspace,
    ) -> Result<Planned, String> {
        let state = self.state.borrow();
        let Some(launcher) = state.launcher.clone() else {
            return Err("no tilo-launcher is installed, so no connector can start".to_owned());
        };
        let installed = app_catalogue::apps::read_one_from_env(app)
            .ok()
            .flatten()
            .ok_or_else(|| format!("{app} is not installed here"))?;
        let mut environment = state.environment.clone();
        environment.x11_display = None;
        let context = PlanContext {
            environment: &environment,
            workspace,
            launcher: Some(launcher),
            identity: state.identity.clone(),
            authority: Some(&state.authority),
            render_node: None,
            new_window: false,
            continuity_instance: None,
            connector: Some(origin),
            // A connector reads the origin outside, not a record the session serves: it is never
            // the document host's launch.
            hosted: None,
        };
        let planned = Planned::of(
            &Launch::Package {
                app: installed.package_id,
                open: None,
            },
            &context,
        )
        .map_err(|error| error.to_string())?;
        Ok(planned)
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

    /// The windows the person can see, as the desk last laid them out: the front tab of each group
    /// on the strip in front, and the dialogs floating over those. `None` while no desk is mounted,
    /// when nothing is known about what is in view.
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

    /// Put every cgroup a launch of this session runs in in its tier: `on_screen` is the process of
    /// each window the desk shows, `own` the cgroup this session runs in (see
    /// [`tilo_app_launch::Spawner`]).
    pub fn tier_by_visibility(&self, on_screen: &[u32], own: Option<&std::path::Path>) {
        self.state
            .borrow()
            .spawner
            .tier_by_visibility(on_screen, own);
    }

    /// The session is ending: every app it started that still runs is asked to end with it.
    pub fn end_running(&self) {
        self.state.borrow().spawner.end_running();
    }

    /// Start what the desktop queued since the last pass, show what stopped, and start what a
    /// service, an app or an agent asked for: called once per pass of the session's loop.
    pub fn follow(&self) {
        self.start_asked_now();
        self.follow_ended();
        self.follow_policy();
        self.follow_served();
    }

    /// Tell the launches what this session serves: each app whose model a service runs, by the
    /// resource it is served at. An `open` of one places that resource rather than awaiting a
    /// window process that never publishes
    /// (`docs/designs/app-language/one-app-model.md` §3), and a task's `open` of one is refused at
    /// once with the same reason.
    fn follow_served(&self) {
        let (Some(services), Some(launches)) = (
            self.state.borrow().services.clone(),
            self.state.borrow().launches.clone(),
        ) else {
            return;
        };
        for (app, root) in services.borrow().served() {
            launches.serves(&app, &root);
        }
    }

    /// Start what the desktop and anything waiting on a launch asked for: a launch's answer, and
    /// the app's own start, then run beside the frame this pass waits for instead of after it.
    pub(crate) fn start_asked_now(&self) {
        self.start_asked();
        self.start_asked_of_services();
    }

    /// Start the launches a service, an app or an agent waits on, when one waits: what a pass woken
    /// while it waits for its frame does at once, so the asker hears back before the frame.
    pub(crate) fn start_waiting(&self) {
        let waiting = self
            .state
            .borrow()
            .launches
            .as_ref()
            .is_some_and(|launches| launches.waiting());
        if waiting {
            self.start_asked_of_services();
        }
    }

    /// Bring what the person's authority in a workspace is published as up to the policy the desk
    /// holds now, so a part given or taken back reaches the apps they already run. The desk's
    /// revision says whether anything may have changed.
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

    /// The launches services, apps and agents asked the session for: each is started on the path
    /// the desk's own launches take, in the workspace the asker named, and the asker is told how it
    /// went.
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
            if request.windowless {
                let started = self.start_windowless(&request.launch, &workspace);
                if let Err(error) = &started {
                    tracing::warn!(
                        app = %request.launch.id(),
                        workspace = %workspace.id,
                        %error,
                        "an app asked for without a window did not start"
                    );
                }
                request.answer(started.map_err(|error| error.to_string()));
                continue;
            }
            let context = request.context.clone();
            let started = self.starting_tool(context.as_deref(), || {
                // The place the asker named is where the window it asks for stands, and never that
                // there is a second one: an app the desk already shows comes forward, as it does for
                // the person's own launches.
                self.start_at_place(&request.launch, &workspace, None, Some(&request.place))
                    .map(|_| ())
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

    /// Start what `start` launches with the tool `context` it opens: what the service that asked
    /// for the launch told the tool to show.
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

    /// The launches the desktop queued: each named app is planned against the workspace whose tab
    /// the desk made for its window, and started there.
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
            // A window is on its way to its own tab, and a notice starts nothing: neither asks for
            // a process.
            if matches!(launch, Launch::Window { .. } | Launch::Crashed { .. }) {
                continue;
            }
            let launch = as_this_machine_knows_it(launch);
            let workspace = self.workspace_of(asked.tab);
            let started = self.start_at(&launch, &workspace, asked.tab);
            match &started {
                // The window is coming, or the app's own window came forward in place of a second
                // one: the tab it was held in takes it.
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

    /// What the person asked for in `tab` did not happen, `why` says why: the tab keeps its place
    /// and shows the answer, with the button that asks for the app again. A launch no tab holds is
    /// answered by whoever asked for it.
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
        // The answer is a desktop command like any other: the desk carries it out, and lays its tab
        // out, on its next pass.
        desktop.with_widgets(WidgetHost::relayout);
    }

    /// Start `launch` on the workspace in front, which is where what the session itself
    /// opens belongs.
    /// # Errors
    /// The launch starts nothing, or the process could not be started.
    pub fn open(&self, launch: &Launch) -> Result<(), DoorError> {
        let workspace = self.active_workspace();
        self.start(launch, &workspace)
    }

    /// [`Self::open`] for a launch the desk held the tab `instance` for: the window it opens is
    /// that tab. Whether a window is on its way to fill it: one a running process of the app opens
    /// itself is.
    fn start_at(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        instance: Option<u64>,
    ) -> Result<bool, DoorError> {
        self.start_at_place(launch, workspace, instance, None)
    }

    /// [`Self::start_at`] at `place`, the place a service, an app or an agent named: where the window
    /// this launch starts stands — beside the view of the app the asker works in, or beside the
    /// person's without taking their focus. A place is where a window goes, never that there is a
    /// second one: a launch of an app the desk already shows raises that window, whatever place it
    /// was asked at, which is the answer [`Instance::answer`] gives every launch.
    fn start_at_place(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        instance: Option<u64>,
        place: Option<&LaunchPlace>,
    ) -> Result<bool, DoorError> {
        match self.starting(launch, workspace, instance, place) {
            Beginning::Raised => Ok(false),
            Beginning::Start { new_window } => {
                self.start_window(launch, workspace, new_window, instance, None)
            }
            Beginning::StartIn { tab } => {
                let started = self.start_window(launch, workspace, false, Some(tab), None);
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

    /// The apps that ended since the last pass: one that chose to end leaves its window behind and
    /// is gone; one that crashed gets a notice in the place it had, offering it again.
    fn follow_ended(&self) {
        let ended = self.state.borrow().spawner.events();
        for event in ended {
            // An agent has no window and no place of its own on the desk: its end is logged, and
            // the record's next change starts it again rather than this pass starting it in a loop.
            if let Some((workspace, agent)) = self.agent_ended(event.id) {
                tracing::warn!(
                    %agent,
                    %workspace,
                    end = %event.end,
                    "an agent stopped"
                );
                continue;
            }
            // The launch is over: its door retires with it, so the app's next window there starts a
            // process again.
            self.state.borrow_mut().held.remove(&event.id);
            if !event.end.crashed() {
                self.state.borrow_mut().running.remove(&event.id);
                tracing::info!(app = %event.app, "an app closed");
                continue;
            }
            tracing::warn!(app = %event.app, end = %event.end, "an app stopped");
            // What the app itself said before it ended, at warn beside the end: a launch that never
            // opened is otherwise a warning with no reason in it, and the app's own account is the
            // only one there is.
            for said in &event.said.stderr {
                tracing::warn!(
                    target: tilo_startup_metrics::APP_OUTPUT_TARGET,
                    app = %event.app,
                    stream = "stderr",
                    "{}",
                    tilo_app_launch::output::line(&event.app, "stderr", said)
                );
            }
            // Reopen asks for the app the way it was asked for this time: a host app by its desktop
            // id, a Tilo app by its package id.
            let asked = self.state.borrow_mut().running.remove(&event.id);
            if asked.is_none() {
                tracing::warn!(app = %event.app, "a stopped app this session did not start");
            }
            // An app the session closed for memory did not stop working: the notice says which it
            // was, so the person does not read a closing they caused as a crash.
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
                // The notice is a desktop command like any other: the desk carries it out, and lays
                // its tab out, on its next pass.
                desktop.with_widgets(WidgetHost::relayout);
            }
        }
    }

    /// What a launch does about the app maybe already showing in `workspace`: start it,
    /// start it into the tab a saved window of it waits in, or raise the window it has or
    /// the place one is on its way to. A launch is decided against the tabs the desk has
    /// on the workspace in front, less `instance` — the tab this launch itself holds. A
    /// launch into another workspace starts its own process. `place` is the place a service,
    /// an app or an agent asked the launch at: it says where the window this launch starts
    /// stands when the app shows nothing yet, and never that an app showing something gets a
    /// second window — raising the one it has is the whole answer there.
    fn starting(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        instance: Option<u64>,
        place: Option<&LaunchPlace>,
    ) -> Beginning {
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
        // A window the person left open waits in its own tab for the app, and nothing has asked for
        // it yet: this launch starts the app into that tab — the way the restore opens a saved
        // window — instead of beside it, so one window of the app comes and not two.
        if let Some(tab) = desktop.with_policy(|policy| policy.waiting_tab(&app.id))
            && desktop.with_policy(|policy| policy.adopt_tab(tab, false))
        {
            return Beginning::StartIn { tab };
        }
        Beginning::Start {
            new_window: matches!(answer, Answer::Start { new_window: true })
                // A place of its own — one an app or an agent asked for, never the person's own
                // (`Focus`) — starts the window beside the view it was asked by, so the person's
                // window keeps its place.
                || place.is_some_and(|place| !matches!(place, LaunchPlace::Focus)),
        }
    }

    /// The workspace a launch that names none belongs to: the one in front, with what the person
    /// granted apps in it and the policy that decides what they may do in them, or a launch into no
    /// workspace at all when the desk shows none.
    fn active_workspace(&self) -> Workspace {
        let front = self.desktop.borrow().clone().and_then(|desktop| {
            let handle = desktop.with_policy(|policy| policy.handle().clone());
            handle.read(|tree| {
                tree.active_workspace()
                    .map(|workspace| workspace.id().to_string())
            })
        });
        front.map_or_else(
            || Workspace::under(&runtime_dir(), String::new()),
            |workspace| self.workspace_named(&workspace),
        )
    }

    /// The workspace the window in `tab` belongs to: the one the desk made its tab in, with what
    /// the person granted apps there. A window's tab is its place, so where the desk stands now is
    /// no part of the answer — a workspace that arrived, or a switch made while the desk was on its
    /// way, would otherwise start the app in another workspace's records and grants, and the desk
    /// would carry the window out of the workspace the person opened it in. The workspace in front
    /// answers only for a window the desk holds no tab for.
    pub(crate) fn workspace_of(&self, tab: Option<u64>) -> Workspace {
        tab.and_then(|tab| {
            let desktop = self.desktop.borrow().clone()?;
            let handle = desktop.with_policy(|policy| policy.handle().clone());
            handle.read(|tree| {
                tree.workspaces
                    .iter()
                    .find(|held| held.tabs().any(|shown| shown.id == tab))
                    .map(|held| held.id().to_string())
            })
        })
        .map_or_else(
            || self.active_workspace(),
            |workspace| self.workspace_named(&workspace),
        )
    }

    /// The workspace named `workspace` , with its grants when the desktop holds it and none when it
    /// does not: a launch into a workspace this device does not show gets the app's manifest
    /// permissions alone.
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

    /// `shown` as a launch into it is planned: what the person granted apps there, and the policy
    /// their authority in it is projected from.
    fn launching_into(shown: &tilo_desktop_model::Workspace) -> Workspace {
        let mut launched = Workspace::under(&runtime_dir(), shown.id().to_string());
        launched.grants = shown.launch_grants(unix_seconds());
        launched.policy = Some(shown.policy.clone());
        launched
    }

    /// What a search row names, run as the person's own choice runs it: an app in the workspace it
    /// was published in, and the action to run there. `place` is where they asked the window of an
    /// `open` to go.
    fn invoke_at(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
        place: OpenPlace,
    ) -> Result<(), DoorError> {
        // A row is published in one of two shapes: a workspace's `open` of an app
        // (`workspace:<workspace>`, what `application_open_target` builds) and an app's own action
        // (`application:<workspace>:<app>`). Read both, or every app row answers that apps cannot be
        // started.
        let workspace = match application_provider(&target.provider) {
            Some((workspace, _)) => workspace.to_owned(),
            None => provider_workspace(&target.provider)
                .ok_or_else(no_action_host)?
                .to_owned(),
        };
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
                    // A tab of the window in front is the desk's to arrange: the session opens the
                    // app the way it opens, and the desk puts the tab where the person asked.
                    OpenPlace::Focus | OpenPlace::TabHere => self.start(&launch, &workspace),
                    OpenPlace::Beside => self.start_beside(&launch, &workspace, None, None),
                }
            }
            _ => self.deliver(target, arguments, &workspace),
        }
    }

    /// Start `launch` in `workspace`, unless the app already has a window there: that
    /// one is raised and nothing starts.
    /// # Errors
    /// The launch starts nothing, or the process could not be started.
    pub fn start(&self, launch: &Launch, workspace: &Workspace) -> Result<(), DoorError> {
        self.start_at(launch, workspace, None).map(|_| ())
    }

    /// Start `launch` as a window of its own, beside the tab its app already has: what
    /// the app's own ask for another window does, and the person's own ask from search
    /// for a window of its own. It is the one way to another window of an app showing
    /// one: a launch at a place, an agent's or a service's, raises that window instead
    /// ([`Self::start_at_place`]). `instance` is the tab the window takes, which the app
    /// is told, and `reveal` the item it opens on, when the launch names none: the
    /// conversation a send landed in.
    /// # Errors
    /// The launch starts nothing, or the process could not be started.
    pub fn start_beside(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        instance: Option<u64>,
        reveal: Option<&str>,
    ) -> Result<(), DoorError> {
        self.start_window(launch, workspace, true, instance, reveal)
            .map(|_| ())
    }

    /// Start the agent `kept` in its workspace, unless this session already
    /// runs it: one process per agent per session, so nothing starts twice,
    /// and one that ended is not started again — the record's next change to
    /// its name or model is.
    pub(crate) fn start_agent(&self, kept: &Kept) {
        let key = (kept.workspace.clone(), kept.id.clone());
        if !self.agent_due(&key, kept) {
            return;
        }
        let launch = match self.agent_plan(kept) {
            Ok(planned) => match self.spawn(&planned, None, &kept.workspace, None, None) {
                Ok(launch) => Some(launch),
                Err(error) => {
                    tracing::warn!(
                        agent = %kept.id,
                        workspace = %kept.workspace,
                        %error,
                        "an agent did not start"
                    );
                    None
                }
            },
            Err(error) => {
                tracing::warn!(
                    agent = %kept.id,
                    workspace = %kept.workspace,
                    %error,
                    "an agent did not start"
                );
                None
            }
        };
        self.remember_agent(key, kept, launch);
    }

    /// Whether this session is due to start `kept` : it has not started it with this name and
    /// model, and has not tried. A process it started is not started again after it ended, so an
    /// agent that cannot stay up is not restarted in a loop.
    fn agent_due(&self, key: &(String, String), kept: &Kept) -> bool {
        let wanted = (kept.name.clone(), kept.model.clone());
        self.state
            .borrow()
            .agents
            .get(key)
            .is_none_or(|run| run.wanted != wanted)
    }

    /// Write down what this session started for `kept` , and the launch running it while one does.
    fn remember_agent(&self, key: (String, String), kept: &Kept, launch: Option<u64>) {
        self.state.borrow_mut().agents.insert(
            key,
            AgentProcess {
                wanted: (kept.name.clone(), kept.model.clone()),
                launch,
            },
        );
    }

    /// What the launcher is asked to run for `kept` : its package as a service, with the agent's
    /// own argv after the entrypoint. A service is handed no display — it draws nothing — and the
    /// launcher refuses one with a socket.
    fn agent_plan(&self, kept: &Kept) -> Result<Planned, DoorError> {
        let workspace = self.workspace_named(&kept.workspace);
        let launch = Launch::Package {
            app: AGENT_PACKAGE.to_owned(),
            open: None,
        };
        let planned = {
            let state = self.state.borrow();
            let mut environment = state.environment.clone();
            environment.wayland_socket = None;
            environment.x11_display = None;
            // The agent joins as its own actor through the broker, so it is handed none of the
            // person's authority or identity record.
            let context = PlanContext {
                environment: &environment,
                workspace: &workspace,
                launcher: state.launcher.clone(),
                identity: None,
                authority: None,
                render_node: None,
                new_window: false,
                continuity_instance: None,
                connector: None,
                hosted: None,
            };
            Planned::of(&launch, &context)
        };
        let mut planned = planned.map_err(|error| DoorError::Failed(error.to_string()))?;
        let provider = provider_selection(&agent_provider_config::ProviderConfig::load(), kept);
        planned.args.extend(agent_argv(kept, provider.as_deref()));
        Ok(planned)
    }

    /// The agent whose launch `id` was, marked as ended. A process that ended is not started again
    /// by the next pass, so an agent that cannot stay up is not restarted in a loop.
    fn agent_ended(&self, id: u64) -> Option<(String, String)> {
        let mut state = self.state.borrow_mut();
        let (key, run) = state
            .agents
            .iter_mut()
            .find(|(_, run)| run.launch == Some(id))?;
        run.launch = None;
        Some(key.clone())
    }

    /// `workspace` as this launch starts in it: a launch that opens a package there has the
    /// workspace make that package an author of its own records first, and is read again with what
    /// it granted.
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

    /// Start `launch` in `workspace` : in the process the app already runs there when that one
    /// opens the app's windows, and otherwise in a process of its own, which holds the app's door
    /// for its workspace until it ends. Whether a window comes for the tab held for this launch.
    fn start_window(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        new_window: bool,
        instance: Option<u64>,
        reveal: Option<&str>,
    ) -> Result<bool, DoorError> {
        let authored = self.authored(launch, workspace);
        let workspace = &authored;
        // An app whose model the session runs itself — a tilolang app — has no window process:
        // opening it stands its root on the canvas as a place of the shell's arrangement, drawn
        // by the shell from the session's reading of the record
        // (`docs/designs/app-language/one-app-model.md` §3, "Windows belong to the shell").
        // Only a plain open of the app is a place: a named record or a view the person asked for
        // opens as every app's does, and an app no service runs launches as it always did.
        if let Launch::Package { app, open: None } = launch
            && let Some(placed) = self.place_served(app, "", workspace)
        {
            narrate_placed(app, &workspace.id, &placed);
            return placed.map(|_| true);
        }
        // The open is timed from here to the app's first presented frame: everything this session
        // does for it is in the launch's share, and the phases below say where that share went.
        let app = launch.app();
        let timed = tilo_startup_metrics::launch_requested(app);
        // A tilolang app's window holds none of its data: its instance runs in the tilolang host
        // of its workspace and author, and every window of it opens through its door, which
        // names the instance the window draws.
        let doors = self.state.borrow().doors.clone();
        let hosted = launch.package().is_some() && self.runs_in_host(app);
        let instance = match (&doors, hosted) {
            (Some(doors), true) => instance.or_else(|| Some(doors.next_instance())),
            _ => instance,
        };
        let opens = window_opens(launch, reveal, hosted).map_err(|error| {
            tilo_startup_metrics::launch_failed(timed);
            DoorError::Failed(error)
        })?;
        let mut planned = match self.plan(launch, workspace, new_window, instance) {
            Ok(planned) => planned,
            Err(error) => {
                tilo_startup_metrics::launch_failed(timed);
                return Err(DoorError::Failed(error.to_string()));
            }
        };
        tilo_startup_metrics::launch_phase(app, "planned");
        // The window this launch starts is the one the shell asked for: a
        // process has no door open to it yet, so its launch says which item
        // it opens (`ui::window_door::REVEAL_ENV`).
        if let Some(reveal) = reveal.filter(|_| !hosted) {
            planned
                .env
                .insert(ui::window_door::REVEAL_ENV.to_owned(), reveal.to_owned());
        }
        // The tool this launch opens is told what to show: only a package is given it.
        if let (Some(context), Kind::Package) =
            (self.state.borrow().tool_context.clone(), planned.kind)
        {
            planned
                .env
                .insert(runtime_portal::tools::TOOL_CONTEXT_ENV.to_owned(), context);
        }
        let asked = launch.id();
        let Some(doors) = doors else {
            return self
                .spawn(&planned, Some(&asked), &workspace.id, None, Some(timed))
                .map(|_| true);
        };
        // The app's own window with nothing named to open is the one its running process opens
        // itself, as it opens any later one; a hosted app's every window is.
        let routable = planned.kind == Kind::Package
            && (hosted || matches!(launch, Launch::Package { open: None, .. }));
        let alone: Box<dyn Fn() -> StartAlone> = if hosted {
            self.start_again(launch, workspace, instance, reveal)
        } else {
            Box::new(self.start_alone(&planned, &asked, &workspace.id, timed))
        };
        let reserved = doors.route_or_reserve(
            &planned.app,
            &workspace.id,
            routable,
            &opens,
            &alone,
            instance,
        );
        drop(alone);
        let reservation = match reserved {
            // The process running the app opens this window itself: nothing starts.
            Err(Routed::Opened) => {
                tilo_startup_metrics::launch_failed(timed);
                tracing::info!(app = %planned.app, "the app's running process opens the window");
                return Ok(true);
            }
            // No process of the app serves the workspace: this launch starts one.
            Err(Routed::NotRunning) | Ok(None) => None,
            Ok(reservation) => reservation,
        };
        if hosted {
            // The process is the app's windows alone: its door carries each window's patch
            // connection, and the host of its workspace and author runs their instances.
            if reservation.is_none() || !doors.host_instances(&planned.app, &workspace.id) {
                tilo_startup_metrics::launch_failed(timed);
                return Err(DoorError::Failed(format!(
                    "{} has no window door, which its hosted windows open through",
                    planned.app
                )));
            }
            planned
                .args
                .push(tilo_app_launch::tilolang_hosts::HOSTED_ARG.to_owned());
        }
        tilo_startup_metrics::launch_phase(app, "routed");
        let door = doors.hold(&planned.app, &workspace.id);
        let started = self.spawn(&planned, Some(&asked), &workspace.id, door, Some(timed));
        if started.is_ok()
            && let Some(reservation) = reservation
        {
            // The launch holds its door from its start: the reservation is spent, not given up.
            reservation.launched();
        }
        // A hosted app's first window goes to its host as a later window does.
        if started.is_ok()
            && hosted
            && let Some(instance) = instance
            && let Err(error) =
                doors.first_hosted_window(&planned.app, &workspace.id, instance, &opens)
        {
            tracing::error!(%error, app = %planned.app, "a hosted window's instance runs nowhere");
        }
        started.map(|_| true)
    }

    /// How `launch` starts in `workspace`: a window of its own when `new_window`, in the tab
    /// `instance` when the desk holds one.
    fn plan(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        new_window: bool,
        instance: Option<u64>,
    ) -> Result<Planned, tilo_app_launch::PlanError> {
        // A service running this app's model serves its record: the launch names the resource the
        // session publishes, so the app opens no record of its own (`docs/architecture/apps.md` §8
        // step 2).
        let hosted = matches!(launch, Launch::Record { .. } | Launch::Package { .. })
            .then(|| self.hosted(launch.app()))
            .flatten();
        let state = self.state.borrow();
        // The workspace's own X11 display: an app started there connects to a server no other
        // workspace's apps share.
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
            connector: None,
            hosted: hosted.as_deref(),
        };
        Planned::of(launch, &context)
    }

    /// Start package `app` in `workspace` with no window, for an action that shows nothing: its
    /// process takes the app's actions on its records and makes a window only when the shell
    /// asks it for one, through the door it holds as every launch of the app does. A process of
    /// the app running there already takes the action instead, and nothing starts.
    fn start_windowless(&self, launch: &Launch, workspace: &Workspace) -> Result<(), DoorError> {
        if self.runs_in_host(launch.app()) {
            return Err(DoorError::Failed(format!(
                "{}'s windows hold none of its data: it runs in a tilolang host, never without a window",
                launch.app()
            )));
        }
        let authored = self.authored(launch, workspace);
        let workspace = &authored;
        let Some(doors) = self.state.borrow().doors.clone() else {
            return Err(DoorError::Failed(
                "this session starts no app without a window".into(),
            ));
        };
        let mut planned = self
            .plan(launch, workspace, false, None)
            .map_err(|error| DoorError::Failed(error.to_string()))?;
        planned
            .env
            .insert(ui::window_door::WINDOWLESS_ENV.to_owned(), "1".to_owned());
        let Some(reservation) = doors
            .reserve_unless_running(&planned.app, &workspace.id)
            .map_err(DoorError::Failed)?
        else {
            return Ok(());
        };
        let door = doors.hold(&planned.app, &workspace.id);
        self.spawn(&planned, Some(&launch.id()), &workspace.id, door, None)?;
        // The launch holds its door from its start: the reservation is spent, not given up.
        reservation.launched();
        Ok(())
    }

    /// How a window that must start a process of its own starts it: what a window asked of a
    /// process gone to sleep or built before the door does.
    fn start_alone(
        &self,
        planned: &Planned,
        asked: &str,
        workspace: &str,
        timed: u64,
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
                if let Err(error) =
                    apps.spawn(&planned, Some(&asked), &workspace, None, Some(timed))
                {
                    tracing::warn!(%error, app = %planned.app, "an app did not start");
                }
            })
        }
    }

    /// How a hosted window that must start a process starts it: as a launch of its own again, on
    /// the path every window of the app takes, so it opens through a door of its own and its
    /// instance runs in its host.
    fn start_again(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        instance: Option<u64>,
        reveal: Option<&str>,
    ) -> Box<dyn Fn() -> StartAlone> {
        let me = self.state.borrow().me.clone();
        let (launch, workspace, reveal) =
            (launch.clone(), workspace.clone(), reveal.map(str::to_owned));
        Box::new(move || {
            let (me, launch, workspace, reveal) = (
                me.clone(),
                launch.clone(),
                workspace.clone(),
                reveal.clone(),
            );
            Box::new(move || {
                let Some(apps) = me.upgrade() else {
                    return;
                };
                if let Err(error) =
                    apps.start_window(&launch, &workspace, false, instance, reveal.as_deref())
                {
                    tracing::warn!(%error, app = %launch.app(), "an app did not start");
                }
            })
        })
    }

    /// Start `planned` with the door `door` , which the launch holds until it ends, and write it
    /// down as a launch of this session before the app can reach a service of it. A service asked
    /// as nothing has no window to offer again. `timed` is the launch timing of the request that
    /// led here, which an agent start of no person's ask begins for itself.
    fn spawn(
        &self,
        planned: &Planned,
        asked: Option<&str>,
        workspace: &str,
        door: Option<HeldDoor>,
        timed: Option<u64>,
    ) -> Result<u64, DoorError> {
        let launches = self.state.borrow().launches.clone();
        let mut state = self.state.borrow_mut();
        let windowless = planned.env.contains_key(ui::window_door::WINDOWLESS_ENV);
        // The launch's share in the launch-latency log runs from the request to the first presented
        // frame: the launcher's and the app's in this log, the surface's in the compositor's. A
        // launch with no window presents none.
        let timed = (!windowless)
            .then(|| timed.unwrap_or_else(|| tilo_startup_metrics::launch_requested(&planned.app)));
        let started = state.spawner.start_with(planned, door, &|pid| {
            if let Some(launches) = &launches {
                // The launcher binds the session's documents store into a package's sandbox and
                // into nothing else, so a package is the launch whose files are answered with paths
                // in the store.
                if windowless {
                    launches.started_windowless(pid, &planned.app, workspace);
                } else {
                    launches.started(pid, &planned.app, workspace, !planned.kind.is_host());
                }
            }
        });
        let started = match started {
            Ok(started) => started,
            Err(error) => {
                if let Some(timed) = timed {
                    tilo_startup_metrics::launch_failed(timed);
                }
                return Err(DoorError::Failed(error.to_string()));
            }
        };
        let (started, door) = started;
        if let Some(timed) = timed {
            tilo_startup_metrics::launch_spawned(timed, started.pid);
        }
        if let Some(asked) = asked {
            state.running.insert(started.id, asked.to_owned());
        }
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
        Ok(started.id)
    }
}

/// What the window `launch` starts shows: the record it names and the item it opens on
/// (`reveal`). A hosted window's instance is handed both, the first window's as every later
/// one's; its app opens no file or link, only its records, so a `hosted` launch naming one is
/// refused.
fn window_opens(
    launch: &Launch,
    reveal: Option<&str>,
    hosted: bool,
) -> Result<tilo_app_launch::Opens, String> {
    if hosted && matches!(launch, Launch::Package { open: Some(_), .. }) {
        return Err(format!(
            "{} opens no file or link: its windows show its records",
            launch.app()
        ));
    }
    Ok(tilo_app_launch::Opens {
        record: match launch {
            Launch::Record { record, .. } => Some(record.clone()),
            _ => None,
        },
        reveal: reveal.map(str::to_owned),
    })
}

/// The argv an agent is run with: the launcher's `--service` , whose own arguments follow the
/// package's entrypoint, then the agent's options.
fn agent_argv(kept: &Kept, provider: Option<&str>) -> Vec<String> {
    let mut argv = vec![
        "--service".to_owned(),
        "--workspace".to_owned(),
        kept.workspace.clone(),
        "--app".to_owned(),
        AGENT_PACKAGE.to_owned(),
        "--agent-id".to_owned(),
        kept.id.clone(),
        "--name".to_owned(),
        kept.name.clone(),
    ];
    if let Some(provider) = provider {
        argv.push("--provider".to_owned());
        argv.push(provider.to_owned());
    }
    argv
}

/// The model `kept` plans with, as the agent's `--provider` selection: the keeper's choice on the
/// record, else this device's Settings. A launched agent cannot read Settings, so without a
/// selection it plans by keyword only.
fn provider_selection(
    config: &agent_provider_config::ProviderConfig,
    kept: &Kept,
) -> Option<String> {
    let (profile, model) = match kept
        .model
        .as_deref()
        .and_then(|chosen| chosen.split_once('/'))
    {
        Some((profile, model)) => {
            let profile = config
                .profiles
                .iter()
                .find(|held| held.id == profile && held.enabled)?;
            (
                profile,
                profile.models.iter().find(|held| held.id == model)?,
            )
        }
        None => config.selection_for(&kept.id)?,
    };
    Some(
        serde_json::json!({
            "profile_id": profile.id,
            "model_id": model.id,
            "protocol": profile.protocol,
            "tool_calling": model.tool_calling,
            "vision": model.vision,
        })
        .to_string(),
    )
}

impl AppActions for Apps {
    /// What the apps publish, read from the session's action broker: the one host every app
    /// publishes on, whatever it is built with. The broker keeps the last surface an app published
    /// after it closes, which is why a closed app's actions are offered too: choosing one starts
    /// the app and delivers the call there.
    fn published(&self, workspace: &str) -> Vec<PublishedActions> {
        let state = self.state.borrow();
        let Some(broker) = &state.broker else {
            return Vec::new();
        };
        // By app, workspace and address: the broker holds a surface per record, and an action two
        // of them carry is still the one action that workspace addresses.
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
                // A live surface is listed before the one retained from before the app closed, and
                // is the one to keep.
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
                // An exported action names its resource's kind but not what the resource is to a
                // person ([`PublishedActions::types`]): that stays with the app's own host.
                types: BTreeMap::new(),
            })
            .collect()
    }

    /// The broker follows each invocation it delivered, but offers no listing that names the app,
    /// workspace and action label a job's row reads, so there is nothing to list here yet.
    fn jobs(&self) -> Vec<JobEntry> {
        Vec::new()
    }

    /// The workspace's `open` of an app is the app's own action on its own resource, and is carried
    /// out here. Every other action goes to the session's broker, which starts the app when it is
    /// closed, waits for it to publish the action and delivers the call there.
    fn invoke(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), DoorError> {
        self.invoke_at(target, arguments, OpenPlace::Focus)
    }

    /// Open `app` where the person asked it, through the same run of the workspace's `open`
    /// as [`Self::invoke`]: `place` is what the modifier they held over the row asked of the window.
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
        // A job's row carries the invocation the broker follows, and no row is listed yet
        // ([`Self::jobs`]), so nothing names one to stop.
        Err(no_action_host())
    }

    /// Open `uri` in the app that takes it: a document's own app when it is a file on this machine,
    /// else the app that claims its scheme.
    fn open_uri(&self, workspace: &str, uri: &str) -> Result<(), DoorError> {
        let (installed, entries) = routes();
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
        // One of the machine's own apps runs unconfined, so it is launched into no workspace: it is
        // told nothing of Tilo's authority.
        self.start(
            &Launch::Host {
                desktop_id: desktop_id.to_owned(),
                open: None,
            },
            &Workspace::under(&runtime_dir(), String::new()),
        )
    }
}

/// This device's apps as routing reads them: the installed Tilo packages and the machine's own
/// desktop entries.
pub(crate) fn routes() -> (Vec<app_catalogue::apps::InstalledApp>, desktop_entry::Scan) {
    let installed = app_catalogue::apps::read_from_env().unwrap_or_default();
    let entries = desktop_entry::cache::cached_scan(&crate::app_names::entry_locales());
    (installed, entries)
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

/// How `app` starts again here: an installed Tilo package through the launcher, or one of the
/// machine's own applications through the entry whose identities hold the name its window
/// announced. `None` when this machine has neither.
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

/// The package id `app` is installed under: the id itself, or the package whose short name it is.
fn installed_package(app: &str) -> Option<String> {
    if let Ok(Some(installed)) = app_catalogue::apps::read_one_from_env(app) {
        return Some(installed.package_id);
    }
    if app.contains('.') {
        return None;
    }
    let installed = app_catalogue::apps::read_from_env().ok()?;
    installed
        .into_iter()
        .find(|package| {
            tilo_app_launch::package_identities(&package.package_id)
                .iter()
                .any(|identity| identity.eq_ignore_ascii_case(app))
        })
        .map(|package| package.package_id)
}

/// Why an action an app publishes cannot be run here: the action host an app's resources arrive on
/// is not mounted in this session.
fn no_action_host() -> DoorError {
    DoorError::Unavailable {
        code: "action_host_unavailable".into(),
        reason: "The session runs without an action host".into(),
    }
}

/// Say where an app the session runs itself was put: the place its root stands in, on the
/// workspace's canvas, or that the workspace already showed it.
fn narrate_placed(app: &str, workspace: &str, placed: &Result<Option<u64>, DoorError>) {
    match placed {
        Ok(Some(place)) => {
            tracing::info!(app, workspace, place, "the app's root stands on the canvas");
        }
        Ok(None) => {
            tracing::info!(app, workspace, "the app's place came forward");
        }
        Err(error) => {
            tracing::warn!(app, workspace, %error, "the app's place was not made");
        }
    }
}

/// Where a launch's private files live: the per-user runtime directory.
fn runtime_dir() -> PathBuf {
    tilo_paths::xdg::runtime_dir_or_default()
}

/// The time grants are decided at: seconds since the epoch, as the workspace stores them.
fn unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_app_launch::output::OutputTail;
    use tilo_app_launch::{End, Event};

    const WORKSPACE: &str = "6f1c2a3b-4d5e-4f60-8a7b-9c0d1e2f3a4b";

    fn kept() -> Kept {
        Kept {
            workspace: WORKSPACE.to_owned(),
            id: "scribe".to_owned(),
            name: "Scribe".to_owned(),
            model: None,
        }
    }

    fn apps() -> Apps {
        Apps::with_launcher(SessionEnvironment::default(), None, None)
    }

    /// A launch takes the compositor's render node once the compositor has one, though the session
    /// asked before its backend named it; the node it took then stays.
    #[test]
    fn a_launch_follows_the_render_node_the_compositor_names_later() {
        let apps = apps();
        apps.set_render_node(None);
        apps.follow_render_node(|| None, false);
        assert_eq!(apps.state.borrow().render_node, None);
        assert!(
            !apps.state.borrow().software,
            "nothing is decided before the first frame"
        );
        apps.follow_render_node(|| Some(PathBuf::from("/dev/dri/card1")), true);
        assert_eq!(
            apps.state.borrow().render_node.as_deref(),
            Some(Path::new("/dev/dri/card1"))
        );
        apps.follow_render_node(|| Some(PathBuf::from("/dev/dri/renderD128")), true);
        assert_eq!(
            apps.state.borrow().render_node.as_deref(),
            Some(Path::new("/dev/dri/card1")),
            "the node it took stays"
        );
    }

    /// A compositor that has drawn with no GPU to import from draws in software, and the apps it
    /// opens are handed the software renderer's pin, where this machine has one.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_compositor_that_drew_without_a_gpu_pins_its_apps_to_the_software_renderer() {
        let apps = apps();
        apps.follow_render_node(|| None, true);
        let state = apps.state.borrow();
        assert!(state.software);
        assert_eq!(state.render_node, None);
        match launcher::gpu::software_pin(&launcher::gpu::IcdSearch::default()) {
            Ok(pin) => {
                for (key, value) in pin.env() {
                    assert_eq!(
                        state.environment.variables().get(key),
                        Some(&value),
                        "{key}"
                    );
                }
            }
            Err(_) => assert!(
                !state.environment.forwarded.contains_key("VK_DRIVER_FILES"),
                "no software renderer here, so nothing is pinned"
            ),
        }
    }

    /// An agent is a service the launcher runs, and the agent's own options follow the entrypoint:
    /// the workspace its record names it in, the id its actor works as, and the model it plans
    /// with.
    #[test]
    fn an_agent_is_run_as_a_service_with_its_own_argv() {
        let kept = kept();
        let provider = r#"{"profile_id":"p","model_id":"m","protocol":"anthropic"}"#;
        assert_eq!(
            agent_argv(&kept, Some(provider)),
            [
                "--service",
                "--workspace",
                WORKSPACE,
                "--app",
                "so.tilo.agent",
                "--agent-id",
                "scribe",
                "--name",
                "Scribe",
                "--provider",
                provider,
            ]
        );
        assert_eq!(
            agent_argv(&kept, None),
            [
                "--service",
                "--workspace",
                WORKSPACE,
                "--app",
                "so.tilo.agent",
                "--agent-id",
                "scribe",
                "--name",
                "Scribe",
            ],
            "an agent with no model configured plans without a selection, and by keyword"
        );
    }

    /// The keeper's choice on the record is what the agent plans with; with none it follows this
    /// device's Settings for it.
    #[test]
    fn an_agents_model_is_the_keepers_choice_then_the_devices_own() {
        use agent_provider_config::ProviderConfig;

        let mut config: ProviderConfig = serde_json::from_value(serde_json::json!({
            "profiles": [
                {
                    "id": "keeper",
                    "label": "Keeper's provider",
                    "enabled": true,
                    "models": [{"id": "chosen", "label": "Chosen"}],
                },
                {
                    "id": "here",
                    "label": "This device's provider",
                    "enabled": true,
                    "default_model": "default",
                    "models": [{"id": "default", "label": "Default"}],
                },
            ],
            "default_profile": "here",
        }))
        .expect("a configuration");
        // The keeper chose a model on the record, for a profile this device holds: that is the one.
        let mut chosen = kept();
        chosen.model = Some("keeper/chosen".to_owned());
        let selection = provider_selection(&config, &chosen).expect("a selection");
        assert!(
            selection.contains(r#""profile_id":"keeper""#),
            "{selection}"
        );
        assert!(selection.contains(r#""model_id":"chosen""#), "{selection}");
        // A profile or model this device does not hold is not guessed.
        chosen.model = Some("elsewhere/model".to_owned());
        assert!(provider_selection(&config, &chosen).is_none());
        // No choice on the record: what this device's Settings hold for it.
        config.agent_models.insert(
            "scribe".to_owned(),
            agent_provider_config::AgentModel {
                profile: "here".to_owned(),
                model: "default".to_owned(),
            },
        );
        let selection = provider_selection(&config, &kept()).expect("this device's selection");
        assert!(selection.contains(r#""profile_id":"here""#), "{selection}");
        assert!(selection.contains(r#""model_id":"default""#), "{selection}");
    }

    /// One process per agent per session: a pass that sees the same record starts nothing, a
    /// process that ended is not started again, and the record's next change is.
    #[test]
    fn an_agent_is_started_once_and_not_restarted_after_it_ends() {
        let apps = apps();
        let kept = kept();
        let key = (kept.workspace.clone(), kept.id.clone());
        assert!(apps.agent_due(&key, &kept));
        apps.remember_agent(key.clone(), &kept, Some(7));
        assert!(!apps.agent_due(&key, &kept), "a running agent starts once");
        apps.state.borrow().spawner.report(Event {
            id: 7,
            app: "so.tilo.agent".to_owned(),
            title: "Agent".to_owned(),
            end: End::exited(1),
            said: OutputTail::default(),
        });
        apps.follow();
        assert!(
            !apps.agent_due(&key, &kept),
            "an agent that ended is not started in a loop"
        );
        let mut renamed = kept;
        renamed.name = "Amanuensis".to_owned();
        assert!(
            apps.agent_due(&key, &renamed),
            "the record's next change starts the agent again"
        );
    }

    /// The desktop the apps are started from, laid out as wide as an output: what its notice tabs
    /// draw on.
    fn desk() -> DesktopHost {
        let (desktop, _strips) = crate::platform::surfaces::desktop(7, 1.0);
        desktop.with_widgets(|widgets| {
            widgets.set_placement(tilo_session_core::surface::Placement {
                origin: (0, 0),
                size: (1200, 800),
                scale: 1.0,
            });
        });
        desktop
    }

    /// What the desktop shows after `end` ended the app `asked` , which this session started as
    /// `running` .
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
            said: OutputTail::default(),
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
