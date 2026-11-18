//! Opening an app: the launches the desktop asks for, planned into the process that starts them
//! and started in the session's own environment.
//!
//! The desktop surface knows an app by name but cannot start a process: it queues the launch for
//! the session ([`tilo_app_launch`]), which plans it against the workspace in front and the
//! displays this session bound, and starts it without waiting. Search rows, files and links open
//! the same way, and an app that stops without choosing to is offered again where its window was.

use std::cell::RefCell;
use std::collections::{BTreeMap, VecDeque};
use std::path::{Path, PathBuf};
use std::rc::{Rc, Weak};
use std::sync::Arc;

use crate::platform::surfaces::{ShellDesktop, WidgetSurface};
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, Caller, InvokeArgument, InvokeRequest,
    RemoteActionTarget, application_open_target, application_provider, provider_workspace,
};
use tilo_actions::{ActionDescriptor, ApiError, ExecutionLocus, InvocationId};
use tilo_app_launch::{
    Answer, Authority, Cause, HeldLink, Instance, Kind, Launch, Links, PlanContext, Planned,
    Routed, SessionEnvironment, Shown, Spawner, StartAlone, Workspace,
};
use tilo_broker_core::services::LaunchPlace;
use tilo_desktop_search::LaunchPlace as OpenPlace;
use tilo_desktop_search::providers::PublishedActions;
use tilo_desktop_search::providers::jobs::JobEntry;
use tilo_desktop_surface::outcome::{Closed, Outcome};
use tilo_workspace_service::apps::collab_app_broker::AppBrokerState;

mod launch;
mod reach;
pub(crate) use reach::ReachFirst;
#[cfg(target_os = "linux")]
mod outdated;
#[cfg(target_os = "linux")]
mod windows;

use crate::agents::Kept;
use crate::search::AppActions;

/// The agent the person's workspace records name: a member without a window, which the session
/// runs for the person who keeps it.
use ui::apps::AGENT as AGENT_PACKAGE;

/// The answer for `asked` that did not open, and `why`, with trying again as its button.
fn not_opened(policy: &tilo_desktop_policy::DesktopPolicy, asked: &str, why: &str) -> Outcome {
    use tilo_desktop_surface::i18n::{t, t_args};
    let name = policy.name_of(asked);
    Outcome::new(t_args("answer-app-not-opened", &[("app", name.into())]))
        .saying(why)
        .retryable(t("answer-try-again"))
}

/// Why a launch this session planned starts nothing, said where a person reads it: an app this
/// device does not have says what is missing once, under the card's title, and any other refusal
/// names the app as the shell names it, never by its package id.
fn refusal_said(error: &tilo_app_launch::PlanError) -> String {
    let Some(app) = error.app() else {
        return error.to_string();
    };
    let name = tilo_desktop_search::names::app_name(app);
    match error {
        // The card above already names the app: the reason says what is missing, once.
        tilo_app_launch::PlanError::NotInstalled(_) => {
            crate::i18n::t("shell-window-installed-nowhere")
        }
        other => other.to_string().replace(app, &name),
    }
}

/// The apps this session starts, and everything that asks it for one.
pub struct Apps {
    /// The desktop the launches come from, once the session has mounted it: what a launch reads its
    /// workspace from, and where a stopped app's notice goes.
    desktop: RefCell<Option<ShellDesktop>>,
    /// The person's answers about this device's camera, microphone, screen and input: what a
    /// launch hands an app of them.
    device_answers: RefCell<Option<tilo_peer_state::DeviceGrants>>,
    state: RefCell<State>,
    /// Why a workspace's apps may not start yet, once the session lent it ([`Self::lend_admission`]).
    admission: RefCell<Option<Admission>>,
    /// What the person is asked about an app's network before it first starts ([`reach`]).
    reach: reach::Reach,
    /// The deliveries whose answer a caller waits for, admitted and not settled yet: the broker
    /// answers a person's action as soon as it is admitted, and the action's own answer is read
    /// from its state once it settles ([`AppActions::poll_answers`]).
    awaited: Arc<std::sync::Mutex<Vec<Awaited>>>,
}

/// A delivery whose answer its caller waits for: the invocation, who asked it, and who hears.
struct Awaited {
    invocation: tilo_actions::InvocationId,
    caller: Caller,
    answered: crate::search::Answered,
}

/// Why the apps of a workspace, by its id, may not start yet: `None` once they may.
pub(crate) type Admission = Rc<dyn Fn(&str) -> Option<String>>;

/// What starting an app needs besides the launch itself.
struct State {
    environment: SessionEnvironment,
    /// The compositor's X11 displays, by workspace: the display a launch into `workspace` starts
    /// its app on.
    x11: Option<tilo_compositor::X11Displays>,
    spawner: Spawner,
    /// Each launch this session started, by the id the spawner gave it: how far it got, so its
    /// end is decided from what happened to the launch itself.
    launched: BTreeMap<u64, Launching>,
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
    /// The shell's revision the published authority was last brought up to: nothing is republished
    /// while the shell has not moved.
    authority_at: Option<u64>,
    /// The GPU the compositor renders on, for an app to render on the same.
    render_node: Option<PathBuf>,
    /// The same display and GPU, as a Windows program started by the compatibility service is
    /// given them.
    windows_display: Arc<crate::windows_compat::Display>,
    /// The compositor drew its first frame with no GPU to import from: its apps were pinned to the
    /// software renderer it draws with ([`Apps::follow_render_node`]).
    software: bool,
    /// The session's launch records, once it has lent them: what its broker admits a connection by,
    /// and so what each launch it makes is written into.
    launches: Option<std::sync::Arc<crate::Launches>>,
    /// The window links of the apps this session starts, once it lent them: one process per app per
    /// workspace opens that app's later windows itself.
    links: Option<Rc<Links>>,
    /// This session, while the links are lent: a window that must start a process of its own starts
    /// it here.
    me: Weak<Apps>,
    /// The link of each launch still running, by the launch's id: the end of the launch retires it,
    /// so the app's next window there starts a process again.
    held: BTreeMap<u64, HeldLink>,
    /// The windows apps asked this session for, by app and in ask order: each starts in the tab the
    /// shell holds for it once the shell asks back ([`Apps::start_asked`]).
    planned: BTreeMap<String, VecDeque<WindowAsk>>,
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
    /// The tilolang processes, once the session lent its links: one per workspace and author, running
    /// the instances of the tilolang apps' windows.
    processes: Option<Arc<tilo_app_launch::tilolang_processes::Processes>>,
    /// The connectors of the apps' outside services: what a hosted instance reaches its origin by.
    connectors: Option<Rc<crate::connectors::ConnectorHost>>,
    /// The files apps are handed, while the session serves them where a launcher binds each app's
    /// view of them: what a document a package opens reaches its sandbox through.
    documents: Option<tilo_documents::Documents>,
    /// The kept apps this run took back, until the restore claims their saved tabs
    /// ([`Apps::take_kept`]).
    #[cfg(target_os = "linux")]
    kept: Vec<tilo_app_launch::Adopted>,
    /// The launches of the kept apps that ended while no desktop was there, until the restore
    /// closes their saved tabs ([`Apps::take_kept`]).
    #[cfg(target_os = "linux")]
    ended_away: Vec<tilo_app_launch::kept::KeptLaunch>,
    /// The apps from before an update that this desktop cannot show, by launch id.
    #[cfg(target_os = "linux")]
    outdated: BTreeMap<u64, outdated::FromBefore>,
    /// The saved tabs whose Windows programs the service was asked for since the last pass, and
    /// has not answered yet.
    windows_waits: Vec<windows::Waiting>,
}

/// A window whose tab the session asked the shell for: the launch that starts it once the shell asks
/// back, and how — the item it shows when one was named, whether it is a window of its own beside
/// the app's, the workspace it was launched into when that is not where the shell made its tab, and
/// what the service that asked for it told the tool to show.
struct WindowAsk {
    launch: Launch,
    reveal: Option<String>,
    new_window: bool,
    workspace: Option<String>,
    tool_context: Option<String>,
}

/// How far a launch this session started has got, as everything that waits for its window reads
/// it: `arriving` while no window of it has come, `landed` once one has, and `could-not-open`
/// with what the app said instead. The page names its windows' states by the same words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LaunchState {
    /// Asked for, and no window of this launch has appeared yet.
    Arriving,
    /// A window this launch's own process made appeared: it opened.
    Landed,
    /// The launch ended before any window of it appeared: `why` is the app's own last word, when
    /// it said one.
    CouldNotOpen { why: String },
}

/// A launch this session started, and how far it got: what its end is decided from, rather than
/// from what the shell happens to show of the app.
#[derive(Clone, Debug)]
struct Launching {
    /// The app as this launch asked for it: what a notice's Reopen asks again.
    asked: String,
    /// The app's package id or desktop-file id, as the shell names the app.
    app: String,
    /// The workspace it was launched into.
    workspace: String,
    /// The process this launch started: a window names the launch it belongs to by descent from
    /// it ([`tilo_app_launch::links::ancestry`]).
    pid: Option<u32>,
    /// The tab the shell held for this launch's window, when it held one: where a launch that
    /// could not open says so.
    instance: Option<u64>,
    /// When it was started, for the log of a launch that could not open.
    started: std::time::Instant,
    /// How far it got.
    got: Got,
}

/// How far a launch got.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Got {
    /// It was started with no window to show, for an action that shows nothing: no window of it
    /// will ever come, and its end is no window that failed to arrive.
    Windowless,
    /// Its window has not appeared yet.
    Arriving,
    /// A window of its own process appeared.
    Landed,
    /// It ended, after opening: an app that closed.
    Closed,
    /// It ended before any window of it appeared: `why` is what the app said.
    CouldNotOpen { why: String },
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
            device_answers: RefCell::new(None),
            admission: RefCell::new(None),
            reach: reach::Reach::default(),
            awaited: Arc::default(),
            state: RefCell::new(State {
                environment,
                x11: None,
                spawner: Spawner::new(wake.clone()),
                launched: BTreeMap::new(),
                launcher,
                identity: None,
                person: String::new(),
                authority: Authority::new(),
                authority_at: None,
                render_node: None,
                windows_display: Arc::default(),
                software: false,
                launches: None,
                links: None,
                me: Weak::new(),
                held: BTreeMap::new(),
                planned: BTreeMap::new(),
                tool_context: None,
                broker: None,
                wake,
                agents: BTreeMap::new(),
                processes: None,
                connectors: None,
                documents: None,
                #[cfg(target_os = "linux")]
                kept: Vec::new(),
                #[cfg(target_os = "linux")]
                ended_away: Vec::new(),
                #[cfg(target_os = "linux")]
                outdated: BTreeMap::new(),
                windows_waits: Vec::new(),
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
        state.windows_display.bound(socket.clone());
        state.environment.wayland_socket = Some(socket);
        state.x11 = x11;
        state.environment.session_bus = session_bus;
    }

    /// The render node of the GPU the compositor draws on: an app the person granted `gpu` is
    /// pinned to it, as it is pinned to the same driver, instead of choosing whichever GPU wgpu
    /// offers it.
    pub fn set_render_node(&self, node: Option<PathBuf>) {
        let mut state = self.state.borrow_mut();
        state.windows_display.drawn_on(node.clone());
        state.render_node = node;
    }

    /// The display and GPU a Windows program is given, kept current as the compositor binds them.
    pub(crate) fn windows_display(&self) -> Arc<crate::windows_compat::Display> {
        Arc::clone(&self.state.borrow().windows_display)
    }

    /// The `tilo-launcher` every app starts through, when one is installed.
    pub(crate) fn launcher(&self) -> Option<PathBuf> {
        self.state.borrow().launcher.clone()
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
        let node = state.render_node.clone();
        state.windows_display.drawn_on(node);
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
    /// Lend what says why a workspace's apps may not start yet: the reason, while this device
    /// waits for the workspace's host to admit it ([`crate::mcp_server::awaited_reason`]).
    pub(crate) fn lend_admission(&self, awaited: Admission) {
        *self.admission.borrow_mut() = Some(awaited);
    }

    /// Why an app of `workspace` may not start yet, when this device waits for its host.
    pub(crate) fn awaited(&self, workspace: &str) -> Option<String> {
        let awaited = self.admission.borrow().clone()?;
        awaited(workspace)
    }

    /// Refuse a launch of one of `workspace`'s apps while this device waits for its host to take it
    /// in: nothing of the app would take part there, so whoever asked hears why at once.
    fn admitted(&self, workspace: &str) -> Result<(), ApiError> {
        let Some(reason) = self.awaited(workspace) else {
            return Ok(());
        };
        tracing::warn!(
            %workspace,
            %reason,
            "a launch was refused: this device is not admitted there yet"
        );
        Err(ApiError::Failed(reason))
    }

    pub fn lend_launches(&self, launches: std::sync::Arc<crate::Launches>) {
        self.state.borrow_mut().launches = Some(launches);
    }

    /// The session's action broker: from here on an action an app publishes is delivered through
    /// it, so a row, a link or a card that names one opens the app on it, starting the app when it
    /// is not up.
    pub fn lend_broker(&self, broker: Option<Arc<AppBrokerState>>) {
        self.state.borrow_mut().broker = broker;
    }

    /// Hand the app action `target` to the app that publishes it, as the person at this device: the
    /// broker starts the app when it is closed, waits for it to publish the action and delivers the
    /// call on its own runtime.
    fn deliver(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
        workspace: &str,
    ) -> Result<(), ApiError> {
        self.deliver_answered(target, arguments, workspace, None)
    }

    /// [`Self::deliver`], telling `answered` what the action answered: at once when the broker
    /// answers it settled, else once it settles ([`AppActions::poll_answers`]).
    fn deliver_answered(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
        workspace: &str,
        answered: Option<crate::search::Answered>,
    ) -> Result<(), ApiError> {
        let (broker, caller, wake) = {
            let state = self.state.borrow();
            let Some(broker) = &state.broker else {
                return Err(no_action_provider());
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
        let asker = caller.clone();
        let awaited = Arc::clone(&self.awaited);
        broker
            .action_invoke_local_person(&caller, &request)
            .then(move |answer| {
                if let Some(Err(error)) = &answer {
                    tracing::warn!(%error, %resource, %action, "an app action was not delivered");
                }
                if let Some(answered) = answered {
                    use tilo_actions::remote::{InvocationReceipt, ReceiptStatus};
                    match answer {
                        Some(Ok(InvocationReceipt { status, invocation })) => match status {
                            ReceiptStatus::Completed { output } => answered(Ok(output.value)),
                            // Admitted and running: its answer is read once it settled.
                            ReceiptStatus::Accepted => {
                                if let Ok(mut held) = awaited.lock() {
                                    held.push(Awaited {
                                        invocation,
                                        caller: asker,
                                        answered,
                                    });
                                }
                            }
                            ReceiptStatus::Failed { error } => answered(Err(error.message)),
                            ReceiptStatus::Rejected { error } => answered(Err(error.to_string())),
                            ReceiptStatus::Cancelled { message } => answered(Err(
                                message.unwrap_or_else(|| "it was cancelled".to_owned())
                            )),
                        },
                        Some(Err(error)) => answered(Err(error.to_string())),
                        None => answered(Err("the action host never answered".to_owned())),
                    }
                }
                if let Some(wake) = &wake {
                    wake();
                }
            });
        Ok(())
    }

    /// The window links the session keeps for the apps it starts: from here on the first launch of
    /// an app in a workspace hands its process the link, and every later window of that app there
    /// opens in that process.
    ///
    /// A tilolang app's windows hold none of its data: each window's instance runs in the
    /// tilolang host of its workspace and author, reaching its outside service through
    /// `connectors`.
    pub fn lend_links(
        self: &Rc<Self>,
        links: Rc<Links>,
        connectors: Rc<crate::connectors::ConnectorHost>,
    ) {
        let me = Rc::downgrade(self);
        links.hand_instances_to(Box::new(move |hosted| {
            if let Some(apps) = me.upgrade() {
                apps.host_window(hosted);
            }
        }));
        let mut state = self.state.borrow_mut();
        state.me = Rc::downgrade(self);
        state.processes = state
            .launcher
            .clone()
            .map(tilo_app_launch::tilolang_processes::Processes::new);
        state.links = Some(links);
        state.connectors = Some(connectors);
    }

    /// The person's region formats or language changed: every tilolang host running here is
    /// told, so the instances it runs write the dates, clocks and weeks their windows show again
    /// in the new formats ([`crate::formats`]), and their windows read their words in the
    /// language ([`crate::i18n`]).
    pub fn locale_changed(&self) {
        let processes = self.state.borrow().processes.clone();
        if let Some(processes) = processes {
            processes.locale_changed();
        }
    }

    /// Whether `app`'s windows hold none of its data, their instances run in a tilolang host:
    /// a tilolang app, in a session with links and a launcher to start processes through.
    fn runs_in_process(&self, app: &str) -> bool {
        let state = self.state.borrow();
        if state.links.is_none() || state.processes.is_none() {
            return false;
        }
        drop(state);
        tilo_app_launch::tilolang_processes::runs_in_process(app)
    }

    /// Hand the instance of the tilolang window `hosted` to the host of its workspace and author,
    /// with the connections an instance holds: who is at it, its identity service and its app's
    /// origin connector, when the app has one.
    fn host_window(&self, hosted: tilo_app_launch::HostedWindow) {
        let (processes, person, connectors) = {
            let state = self.state.borrow();
            (
                state.processes.clone(),
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
        let Some(processes) = processes else {
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
        // holds the network and ownership the host has none of.
        let origin = match tilo_app_launch::tilolang_processes::origin_of(&hosted.app) {
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
        // What the person may do in the app, as a launched app of it reads it: the instance's
        // views offer and refuse by it.
        let access = tilo_app_launch::authority::access(
            &self.workspace_named(&hosted.workspace),
            &hosted.app,
        );
        processes.hand_over(tilo_app_launch::tilolang_processes::Instance {
            access,
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
        let context = PlanContext {
            environment: &state.environment,
            workspace,
            launcher: Some(launcher),
            identity: state.identity.clone(),
            authority: Some(&state.authority),
            // A connector draws nothing, so the plan leaves out the session's display and render
            // node whatever it is given.
            render_node: None,
            new_window: false,
            continuity_instance: None,
            connector: Some(origin),
            // A connector reads the origin outside, not a record the session serves: it is never
            // the document host's launch.
        };
        let planned = Planned::of(
            &Launch::Package {
                app: installed.app_id.to_string(),
                open: None,
            },
            &context,
        )
        .map_err(|error| error.to_string())?;
        Ok(planned)
    }

    /// The files apps are handed, as the session serves them: a package opening one of the
    /// person's files is granted it there and opens it at the path its sandbox holds it at.
    pub fn hand_documents(&self, documents: tilo_documents::Documents) {
        self.state.borrow_mut().documents = Some(documents);
    }

    /// The desktop the person works on, which the launches come from.
    pub fn attach(&self, desktop: ShellDesktop) {
        *self.desktop.borrow_mut() = Some(desktop);
    }

    /// The windows the person can see, as the shell last laid them out: the front tab of each group
    /// on the strip in front, and the dialogs floating over those. `None` while no shell is mounted,
    /// when nothing is known about what is in view.
    pub fn windows_in_view(&self) -> Option<Vec<u64>> {
        let desktop = self.desktop.borrow().clone()?;
        Some(desktop.with_policy(|policy| policy.in_view()))
    }

    /// Put every cgroup a launch of this session runs in in its tier: `on_screen` is the process of
    /// each window the shell shows, `own` the cgroup this session runs in (see
    /// [`tilo_app_launch::Spawner`]).
    pub fn tier_by_visibility(&self, on_screen: &[u32], own: Option<&std::path::Path>) {
        self.state
            .borrow()
            .spawner
            .tier_by_visibility(on_screen, own);
    }

    /// The session is ending: every app it started that still runs is asked to end with it, but
    /// for the kept apps an in-place restart leaves running ([`tilo_app_launch::kept`]).
    pub fn end_running(&self, teardown: tilo_app_launch::Teardown) {
        self.state.borrow().spawner.end_running(teardown);
    }

    /// Start the apps that are not Tilo's behind `keeping`'s keeper from now on.
    #[cfg(target_os = "linux")]
    pub fn keep_behind(&self, keeping: tilo_app_launch::kept::Keeping) {
        self.state.borrow_mut().spawner.keep_behind(keeping);
        self.keep_windows_programs();
    }

    /// The X11 display and cookie file of every kept launch that serves one: what the session's
    /// input method serves as XIM ([`tilo_app_launch::Spawner::kept_x11`]).
    #[cfg(target_os = "linux")]
    pub(crate) fn kept_x11(&self) -> Vec<(String, std::path::PathBuf)> {
        self.state.borrow().spawner.kept_x11()
    }

    /// Take up what the keepers' registry holds as this run starts: after an in-place restart the
    /// apps the run before left running are this session's launches again, as if it had started
    /// them; after any other start what is there is left over, and ends.
    ///
    /// A service's keeper (the Linux VM's window forwarder) is taken up at any start.
    #[cfg(target_os = "linux")]
    pub fn take_up_kept(&self, following_restart: bool) {
        if !following_restart {
            self.state.borrow().spawner.clear_kept();
        }
        let adopted = self.state.borrow_mut().spawner.adopt_kept();
        {
            let mut state = self.state.borrow_mut();
            state.ended_away = state.spawner.away();
        }
        self.took_up(adopted, true);
    }

    /// Show the keepers a service started since the last pass.
    #[cfg(target_os = "linux")]
    fn follow_kept(&self) {
        let adopted = self.state.borrow_mut().spawner.adopt_appeared();
        self.took_up(adopted, false);
    }

    /// Make `adopted` this run's launches; at the start of the run their windows claim the saved
    /// tabs they stood in (`claim`). A service's keeper is no launch of the person's: only its
    /// windows are claimed.
    #[cfg(target_os = "linux")]
    fn took_up(&self, adopted: Vec<tilo_app_launch::Adopted>, claim: bool) {
        self.take_back_windows_programs(&adopted);
        let launches = self.state.borrow().launches.clone();
        let mut state = self.state.borrow_mut();
        for kept in adopted {
            if !kept.launch.service {
                if let Some(launches) = &launches {
                    launches.started(kept.pid, &kept.launch.app, &kept.launch.workspace, false);
                }
                state.launched.insert(
                    kept.id,
                    Launching {
                        asked: kept
                            .launch
                            .asked
                            .clone()
                            .unwrap_or_else(|| kept.launch.app.clone()),
                        app: kept.launch.app.clone(),
                        workspace: kept.launch.workspace.clone(),
                        pid: Some(kept.pid),
                        instance: None,
                        started: std::time::Instant::now(),
                        got: Got::Landed,
                    },
                );
            }
            if claim {
                state.kept.push(kept);
            }
        }
    }

    /// The kept apps this run took back: each of their windows claims the saved tab its keeper
    /// was last told it stood in, which waits for that window rather than starting the app again.
    /// Beside them, the launches of the kept apps that ended while no desktop was there: their
    /// saved tabs close, as any the app closed meanwhile. Taken once.
    #[cfg(target_os = "linux")]
    pub(crate) fn take_kept(
        &self,
    ) -> (
        Vec<tilo_app_launch::Adopted>,
        Vec<tilo_app_launch::kept::KeptLaunch>,
    ) {
        let mut state = self.state.borrow_mut();
        (
            std::mem::take(&mut state.kept),
            std::mem::take(&mut state.ended_away),
        )
    }

    /// Tell each kept window's keeper the tab it stands in now (`placements`), where that changed:
    /// what the run after an in-place restart puts it back in.
    #[cfg(target_os = "linux")]
    pub(crate) fn place_kept(&self, placements: &[(tilo_desktop_policy::KeptWindow, u64)]) {
        let state = self.state.borrow();
        for (window, tab) in placements {
            state
                .spawner
                .place_kept(window.keeper, &window.tag, &tab.to_string());
        }
    }

    /// Show the windows of the kept app whose keeper is `keeper`: once its saved tabs wait for
    /// them.
    #[cfg(target_os = "linux")]
    pub(crate) fn attach_kept(&self, keeper: u32) {
        self.state.borrow().spawner.attach_kept(keeper);
    }

    /// Start what the desktop queued since the last pass, show what stopped, and start what a
    /// service, an app or an agent asked for: called once per pass of the session's loop.
    pub fn follow(&self) {
        self.start_asked_now();
        self.follow_ended();
        self.follow_policy();
        #[cfg(target_os = "linux")]
        self.follow_kept();
        #[cfg(target_os = "linux")]
        self.follow_windows_programs();
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

    /// Bring what the person's authority in a workspace is published as up to the policy the shell
    /// holds now, so a part given or taken back reaches the apps they already run. The shell's
    /// revision says whether anything may have changed.
    fn follow_policy(&self) {
        let Some(desktop) = self.desktop.borrow().clone() else {
            return;
        };
        let revision = desktop.with_policy(|policy| policy.handle().revision());
        let processes = {
            let state = self.state.borrow();
            let hosted = state
                .processes
                .as_ref()
                .is_some_and(|processes| !processes.instances().is_empty());
            if (state.authority.is_empty() && !hosted) || state.authority_at == Some(revision) {
                return;
            }
            state.processes.clone()
        };
        self.state.borrow_mut().authority_at = Some(revision);
        let mut named = self.state.borrow().authority.workspaces();
        if let Some(processes) = &processes {
            named.extend(processes.instances().into_iter().map(|(id, ..)| id));
            named.sort();
            named.dedup();
        }
        for id in named {
            let workspace = self.workspace_named(&id);
            if let Err(error) = self.state.borrow().authority.refresh(&workspace) {
                tracing::warn!(%id, %error, "the person's authority there was not published");
            }
            // The windows whose instances a tilolang host runs are told on its link.
            if let Some(processes) = &processes {
                processes.policy_changed(&workspace);
            }
        }
    }

    /// The launches services, apps and agents asked the session for: each is started on the path
    /// the shell's own launches take, in the workspace the asker named, and the asker is told how it
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
                // there is a second one: an app the shell already shows comes forward, as it does for
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
        start: impl FnOnce() -> Result<(), ApiError>,
    ) -> Result<(), ApiError> {
        let previous = std::mem::replace(
            &mut self.state.borrow_mut().tool_context,
            context.map(str::to_owned),
        );
        let started = start();
        self.state.borrow_mut().tool_context = previous;
        started
    }

    /// The launches the desktop queued: each named app is planned against the workspace whose tab
    /// the shell made for its window, and started there — a window an app asked its session for in
    /// the tab the shell holds for it, from what this session planned.
    fn start_asked(&self) {
        let Some(desktop) = self.desktop.borrow().clone() else {
            return;
        };
        let asked = desktop.take_launches();
        if asked.is_empty() {
            return;
        }
        for asked in asked {
            // A tab the shell holds for a window an app asked its session for: the session planned
            // that window, and starts it in the tab, whether it is a hosted app's window or one of
            // the machine's own.
            if let Some(app) = asked.app.strip_prefix(tilo_desktop_policy::HELD_PREFIX) {
                self.start_planned(app, asked.tab);
                continue;
            }
            // Restart pressed on an app still running from before the update.
            #[cfg(target_os = "linux")]
            if let Some(tab) = asked.tab
                && self.restart_outdated(tab, &asked.app)
            {
                continue;
            }
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
            let started = if asked.beside {
                // A window of the app's own: it opens in the tab the shell made beside the one the
                // app has, as the app's own ask for another window does, and the window the app
                // shows is left where it is.
                self.start_window(&launch, &workspace, true, asked.tab, None)
            } else {
                self.start_at(&launch, &workspace, asked.tab)
            };
            match &started {
                // The window is coming, or the app's own window came forward in place of a second
                // one: the tab it was held in takes it.
                Ok(true) => {}
                Ok(false) => self.release_tab(asked.tab),
                Err(error) => {
                    tracing::warn!(asked = %asked.app, %error, "an app did not start");
                    self.could_not_open(asked.tab, &asked.app, &error.to_string());
                }
            }
        }
    }

    /// Start the window an app asked this session for in the tab the shell holds for it: a window of
    /// its own, beside the app's own, showing the item the app named when it named one. The tab is
    /// the window's instance ([`Self::start_window`]).
    fn start_planned(&self, app: &str, tab: Option<u64>) {
        let planned = {
            let mut state = self.state.borrow_mut();
            state.planned.get_mut(app).and_then(VecDeque::pop_front)
        };
        let Some(planned) = planned else {
            // The tab is held, the window it was asked for is gone: the place goes with it.
            tracing::info!(%app, "a tab was held for a window nothing asked for");
            self.release_tab(tab);
            return;
        };
        let Some(tab) = tab else {
            return;
        };
        // A launch into a workspace the shell is not showing: its tab goes there, and the window
        // starts in that workspace's records whether the shell holds it or not.
        let workspace = match &planned.workspace {
            Some(workspace) => {
                self.carry_to(tab, workspace);
                self.workspace_named(workspace)
            }
            None => self.workspace_of(Some(tab)),
        };
        let started = self.starting_tool(planned.tool_context.as_deref(), || {
            self.start_window(
                &planned.launch,
                &workspace,
                planned.new_window,
                Some(tab),
                planned.reveal.as_deref(),
            )
            .map(|_| ())
        });
        if let Err(error) = started {
            tracing::warn!(%error, %app, "a window an app asked for did not start");
            self.release_tab(Some(tab));
        }
    }

    /// Carry the held `tab` into `workspace`, when the shell holds that workspace.
    fn carry_to(&self, tab: u64, workspace: &str) {
        let Some(desktop) = self.desktop.borrow().clone() else {
            return;
        };
        desktop.with_policy(|policy| {
            let to = policy.handle().read(|tree| {
                tree.workspaces
                    .iter()
                    .find(|held| held.id().to_string() == workspace)
                    .map(tilo_desktop_model::Workspace::id)
            });
            if let Some(to) = to
                && let Err(error) = policy.carry(tab, to)
            {
                tracing::warn!(%error, tab, %workspace, "a launched window stays in front");
            }
        });
    }

    /// The tab the shell held for a window that never came goes, with everything it held.
    fn release_tab(&self, tab: Option<u64>) {
        let (Some(tab), Some(desktop)) = (tab, self.desktop.borrow().clone()) else {
            return;
        };
        desktop.with_policy(|policy| policy.release_tab(tab));
    }

    /// Ask the shell for the tab a window of the app's own takes — right after `anchor`, the tab the
    /// app shows there, and over the shell when `floated` — and plan the window that fills
    /// it: the session starts it once the shell asks back, the tab being the window's instance
    /// ([`Self::start_asked`]). `reveal` is the item the window shows, when the app named one.
    /// Nothing comes when the shell holds no tab of the app to open beside. Whether a tab is held.
    pub fn ask_window(
        &self,
        launch: &Launch,
        anchor: Option<u64>,
        background: bool,
        floated: bool,
        reveal: Option<&str>,
    ) -> bool {
        let Some(desktop) = self.desktop.borrow().clone() else {
            return false;
        };
        let app = launch.app();
        if !desktop.with_policy(|policy| policy.hold(app, anchor, (background, floated), reveal)) {
            return false;
        }
        self.plan_window(WindowAsk {
            launch: launch.clone(),
            reveal: reveal.map(str::to_owned),
            new_window: true,
            workspace: None,
            tool_context: None,
        });
        true
    }

    /// Keep `ask` for the tab the shell holds for it: [`Self::start_planned`] starts it there.
    fn plan_window(&self, ask: WindowAsk) {
        self.state
            .borrow_mut()
            .planned
            .entry(ask.launch.app().to_owned())
            .or_default()
            .push_back(ask);
    }

    /// Ask the shell for the tab `launch`'s window lands in, which is the window's instance: the
    /// window starts once the shell asks back ([`Self::start_planned`]), so every window knows its
    /// tab from its first frame and what it shows is kept for that tab. A window of its own
    /// (`new_window`) lands beside the tab the app has; a launch into a workspace not in front
    /// goes there without taking focus. False when no shell is behind this session.
    /// # Errors
    /// The shell took no open.
    fn ask_shell(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        new_window: bool,
        reveal: Option<&str>,
    ) -> Result<bool, ApiError> {
        let Some(desktop) = self.desktop.borrow().clone() else {
            return Ok(false);
        };
        let app = launch.app();
        let elsewhere = workspace.is_workspace() && self.active_workspace().id != workspace.id;
        let held = desktop.with_policy(|policy| {
            match new_window
                .then(|| policy.anchor_for(app))
                .flatten()
                .filter(|_| !elsewhere)
            {
                Some((anchor, ..)) => policy.hold(app, Some(anchor), (false, false), None),
                None => policy.hold_open(app, elsewhere),
            }
        });
        if !held {
            return Err(ApiError::Failed(format!("the shell took no open of {app}")));
        }
        let tool_context = self.state.borrow().tool_context.clone();
        self.plan_window(WindowAsk {
            launch: launch.clone(),
            reveal: reveal.map(str::to_owned),
            new_window,
            workspace: elsewhere.then(|| workspace.id.clone()),
            tool_context,
        });
        Ok(true)
    }

    /// What the person asked for in `tab` did not happen, `why` says why: the tab keeps its place
    /// and shows the answer, with the button that asks for the app again. A launch no tab holds is
    /// answered by whoever asked for it.
    fn could_not_open(&self, tab: Option<u64>, asked: &str, why: &str) {
        let Some(tab) = tab else {
            return;
        };
        self.answer_in_tabs(|policy| {
            let outcome = not_opened(policy, asked, why);
            policy.failed_at(tab, asked, outcome);
        });
    }

    /// Change what tabs say through the desktop's policy. The answer is a shell command like any
    /// other: the shell carries it out, and lays its tabs out, on its next pass.
    fn answer_in_tabs(&self, answer: impl FnOnce(&mut tilo_desktop_policy::DesktopPolicy)) {
        let Some(desktop) = self.desktop.borrow().clone() else {
            return;
        };
        desktop.with_policy(answer);
        desktop.with_widgets(WidgetSurface::relayout);
    }

    /// Start `launch` on the workspace in front, which is where what the session itself
    /// opens belongs.
    /// # Errors
    /// The launch starts nothing, or the process could not be started.
    pub fn open(&self, launch: &Launch) -> Result<(), ApiError> {
        let workspace = self.active_workspace();
        self.start(launch, &workspace)
    }

    /// [`Self::open`] for a launch the shell held the tab `instance` for: the window it opens is
    /// that tab. Whether a window is on its way to fill it: one a running process of the app opens
    /// itself is.
    fn start_at(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        instance: Option<u64>,
    ) -> Result<bool, ApiError> {
        self.start_at_place(launch, workspace, instance, None)
    }

    /// [`Self::start_at`] at `place`, the place a service, an app or an agent named: where the window
    /// this launch starts stands — beside the view of the app the asker works in, or beside the
    /// person's without taking their focus. A place is where a window goes, never that there is a
    /// second one: a launch of an app the shell already shows raises that window, whatever place it
    /// was asked at, which is the answer [`Instance::answer`] gives every launch.
    fn start_at_place(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        instance: Option<u64>,
        place: Option<&LaunchPlace>,
    ) -> Result<bool, ApiError> {
        self.admitted(&workspace.id)?;
        match self.starting(launch, workspace, instance, place) {
            Beginning::Raised => Ok(false),
            Beginning::Start { new_window } => {
                self.start_window(launch, workspace, new_window, instance, None)
            }
            Beginning::StartIn { tab } => {
                let started = self.start_window(launch, workspace, false, Some(tab), None);
                // The shell's own tab for this launch holds the place of the
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

    /// What a launch does about the app maybe already showing in `workspace`: start it,
    /// start it into the tab a saved window of it waits in, or raise the window it has or
    /// the place one is on its way to. A launch is decided against the tabs the shell has
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
            desktop.with_widgets(WidgetSurface::relayout);
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
    /// workspace at all when the shell shows none.
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

    /// The workspace the window in `tab` belongs to: the one the shell made its tab in, with what
    /// the person granted apps there. A window's tab is its place, so where the shell stands now is
    /// no part of the answer — a workspace that arrived, or a switch made while the shell was on its
    /// way, would otherwise start the app in another workspace's records and grants, and the shell
    /// would carry the window out of the workspace the person opened it in. The workspace in front
    /// answers only for a window the shell holds no tab for.
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
        launched.devices = self
            .device_answers
            .borrow()
            .as_ref()
            .map(tilo_peer_state::DeviceGrants::allowed_here)
            .unwrap_or_default();
        launched
    }

    /// Hand the apps this session starts the devices the person allowed them through `grants`.
    pub fn answer_devices_with(&self, grants: tilo_peer_state::DeviceGrants) {
        self.device_answers.replace(Some(grants));
    }

    /// `shown` as a launch into it is planned: what the person granted apps there, and the policy
    /// their authority in it is projected from.
    fn launching_into(shown: &tilo_desktop_model::Workspace) -> Workspace {
        let mut launched = Workspace::under(&runtime_dir(), shown.id().to_string());
        launched.grants = shown.launch_grants();
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
    ) -> Result<(), ApiError> {
        // A row is published in one of two shapes: a workspace's `open` of an app
        // (`workspace:<workspace>`, what `application_open_target` builds) and an app's own action
        // (`application:<workspace>:<app>`). Read both, or every app row answers that apps cannot be
        // started.
        let workspace = match application_provider(&target.provider) {
            Some((workspace, _)) => workspace.to_owned(),
            None => provider_workspace(&target.provider)
                .ok_or_else(no_action_provider)?
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
                    // A tab of the window in front is the shell's to arrange: the session opens the
                    // app the way it opens, and the shell puts the tab where the person asked.
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
    pub fn start(&self, launch: &Launch, workspace: &Workspace) -> Result<(), ApiError> {
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
    ) -> Result<(), ApiError> {
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
            Ok(planned) => match self.spawn(&planned, None, &kept.workspace, None, None, None) {
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
    fn agent_plan(&self, kept: &Kept) -> Result<Planned, ApiError> {
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
            };
            Planned::of(&launch, &context)
        };
        let mut planned = planned.map_err(|error| ApiError::Failed(error.to_string()))?;
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

    /// Start `launch` in `workspace` : in the process the app already runs there when that one
    /// opens the app's windows, and otherwise in a process of its own, which holds the app's link
    /// for its workspace until it ends. Whether a window comes for the tab held for this launch.
    fn start_window(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        new_window: bool,
        instance: Option<u64>,
        reveal: Option<&str>,
    ) -> Result<bool, ApiError> {
        self.admitted(&workspace.id)?;
        if self.held_for_reach(launch, workspace, new_window, instance, reveal) {
            return Ok(true);
        }
        self.start_launched_window(launch, workspace, new_window, instance, reveal)
    }

    fn start_launched_window(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        new_window: bool,
        instance: Option<u64>,
        reveal: Option<&str>,
    ) -> Result<bool, ApiError> {
        // The open is timed from here to the app's first presented frame: everything this session
        // does for it is in the launch's share, and the phases below say where that share went.
        let app = launch.app();
        // A tilolang app's window holds none of its data: its instance runs in the tilolang host
        // of its workspace and author, and every window of it opens through its link, which
        // names the instance the window draws.
        let links = self.state.borrow().links.clone();
        let hosted = launch.package().is_some() && self.runs_in_process(app);
        if instance.is_none() {
            window_opens(launch, reveal, hosted).map_err(ApiError::Failed)?;
            if self.ask_shell(launch, workspace, new_window, reveal)? {
                return Ok(true);
            }
        }
        let timed = tilo_startup_metrics::launch_requested(app);
        let failed = |error: String| {
            tilo_startup_metrics::launch_failed(timed);
            ApiError::Failed(error)
        };
        let documents = self.state.borrow().documents.clone();
        let launch = handed(launch, workspace, documents.as_ref())
            .map_err(|error| failed(error.to_string()))?;
        let launch = launch.as_ref();
        let opens = window_opens(launch, reveal, hosted).map_err(failed)?;
        // A window of a hosted app whose tab this session named itself, which happens only in a
        // session with no shell behind it: the tab is the window's instance, and it is the only
        // name the window has.
        let instance = match (&links, hosted) {
            (Some(links), true) => instance.or_else(|| Some(links.next_instance())),
            _ => instance,
        };
        let mut planned = self.planned_or_failed(launch, workspace, new_window, instance, timed)?;
        tilo_startup_metrics::launch_phase(app, "planned");
        self.told_what_to_show(&mut planned, reveal, hosted);
        let asked = launch.id();
        let Some(links) = links else {
            return self
                .spawn(
                    &planned,
                    Some(&asked),
                    &workspace.id,
                    instance,
                    None,
                    Some(timed),
                )
                .map(|_| true);
        };
        // The app's own window, on nothing or on a file or a link, is one its running process
        // opens itself, as it opens any later one; a hosted app's every window is.
        let routable =
            planned.kind == Kind::Package && (hosted || matches!(launch, Launch::Package { .. }));
        let alone: Box<dyn Fn() -> StartAlone> = if hosted {
            self.start_again(launch, workspace, instance, reveal)
        } else {
            Box::new(self.start_alone(&planned, &asked, &workspace.id, instance, timed))
        };
        let reserved = links.route_or_reserve(
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
            // The process is the app's windows alone: its link carries each window's patch
            // connection, and the host of its workspace and author runs their instances.
            if reservation.is_none() || !links.host_instances(&planned.app, &workspace.id) {
                tilo_startup_metrics::launch_failed(timed);
                return Err(ApiError::Failed(format!(
                    "{} has no window link, which its hosted windows open through",
                    planned.app
                )));
            }
            planned
                .args
                .push(tilo_app_launch::tilolang_processes::PROCESS_ARG.to_owned());
        }
        tilo_startup_metrics::launch_phase(app, "routed");
        let link = links.hold(&planned.app, &workspace.id);
        let started = self.spawn(
            &planned,
            Some(&asked),
            &workspace.id,
            instance,
            link,
            Some(timed),
        );
        if started.is_ok()
            && let Some(reservation) = reservation
        {
            // The launch holds its link from its start: the reservation is spent, not given up.
            reservation.launched();
        }
        // A hosted app's first window goes to its host as a later window does.
        if started.is_ok()
            && hosted
            && let Some(instance) = instance
            && let Err(error) =
                links.first_hosted_window(&planned.app, &workspace.id, instance, &opens)
        {
            tracing::error!(%error, app = %planned.app, "a hosted window's instance runs nowhere");
        }
        started.map(|_| true)
    }

    /// Tell the process `planned` starts what its window shows: the item the shell asked for, and
    /// the tool a service asked it to open.
    fn told_what_to_show(&self, planned: &mut Planned, reveal: Option<&str>, hosted: bool) {
        // The window this launch starts is the one the shell asked for: a process has no link
        // open to it yet, so its launch says which item it opens (`ui::window_link::REVEAL_ENV`).
        if let Some(reveal) = reveal.filter(|_| !hosted) {
            planned
                .env
                .insert(ui::window_link::REVEAL_ENV.to_owned(), reveal.to_owned());
        }
        // The tool this launch opens is told what to show: only a package is given it.
        if let (Some(context), Kind::Package) =
            (self.state.borrow().tool_context.clone(), planned.kind)
        {
            planned
                .env
                .insert(runtime_portal::tools::TOOL_CONTEXT_ENV.to_owned(), context);
        }
    }

    /// How `launch` starts in `workspace`: a window of its own when `new_window`, in the tab
    /// `instance` when the shell holds one.
    fn plan(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        new_window: bool,
        instance: Option<u64>,
    ) -> Result<Planned, tilo_app_launch::PlanError> {
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
        };
        Planned::of(launch, &context)
    }

    /// Plan `launch` against `workspace`, reporting the launch's failure for the timing it was
    /// opened for when it cannot be planned.
    fn planned_or_failed(
        &self,
        launch: &Launch,
        workspace: &Workspace,
        new_window: bool,
        instance: Option<u64>,
        timed: u64,
    ) -> Result<Planned, ApiError> {
        self.plan(launch, workspace, new_window, instance)
            .map_err(|error| {
                tilo_startup_metrics::launch_failed(timed);
                ApiError::Failed(refusal_said(&error))
            })
    }

    /// Start package `app` in `workspace` with no window, for an action that shows nothing: its
    /// process takes the app's actions on its records and makes a window only when the shell
    /// asks it for one, through the link it holds as every launch of the app does. A process of
    /// the app running there already takes the action instead, and nothing starts.
    fn start_windowless(&self, launch: &Launch, workspace: &Workspace) -> Result<(), ApiError> {
        if self.runs_in_process(launch.app()) {
            return Err(ApiError::Failed(format!(
                "{}'s windows hold none of its data: it runs in a tilolang host, never without a window",
                launch.app()
            )));
        }
        self.start_launched_windowless(launch, workspace)
    }

    fn start_launched_windowless(
        &self,
        launch: &Launch,
        workspace: &Workspace,
    ) -> Result<(), ApiError> {
        let Some(links) = self.state.borrow().links.clone() else {
            return Err(ApiError::Failed(
                "this session starts no app without a window".into(),
            ));
        };
        let mut planned = self
            .plan(launch, workspace, false, None)
            .map_err(|error| ApiError::Failed(refusal_said(&error)))?;
        planned
            .env
            .insert(ui::window_link::WINDOWLESS_ENV.to_owned(), "1".to_owned());
        let Some(reservation) = links
            .reserve_unless_running(&planned.app, &workspace.id)
            .map_err(ApiError::Failed)?
        else {
            return Ok(());
        };
        let link = links.hold(&planned.app, &workspace.id);
        self.spawn(
            &planned,
            Some(&launch.id()),
            &workspace.id,
            None,
            link,
            None,
        )?;
        // The launch holds its link from its start: the reservation is spent, not given up.
        reservation.launched();
        Ok(())
    }

    /// How a window that must start a process of its own starts it: what a window asked of a
    /// process gone to sleep or built before the link does.
    fn start_alone(
        &self,
        planned: &Planned,
        asked: &str,
        workspace: &str,
        instance: Option<u64>,
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
                if let Err(error) = apps.spawn(
                    &planned,
                    Some(&asked),
                    &workspace,
                    instance,
                    None,
                    Some(timed),
                ) {
                    tracing::warn!(%error, app = %planned.app, "an app did not start");
                }
            })
        }
    }

    /// How a hosted window that must start a process starts it: as a launch of its own again, on
    /// the path every window of the app takes, so it opens through a link of its own and its
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
    if hosted
        && matches!(
            launch,
            Launch::Package { open: Some(_), .. } | Launch::Content { .. }
        )
    {
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
        open: match launch {
            Launch::Package {
                open: Some(tilo_app_launch::Open::File(path)),
                ..
            } => Some(path.display().to_string()),
            Launch::Package {
                open: Some(tilo_app_launch::Open::Uri(uri)),
                ..
            } => Some(uri.clone()),
            _ => None,
        },
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
    ) -> Result<(), ApiError> {
        self.invoke_at(target, arguments, OpenPlace::Focus)
    }

    /// An app's own action, its answer told to `answered`: the broker delivers it as
    /// [`Self::invoke`] does.
    fn invoke_answered(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
        answered: crate::search::Answered,
    ) -> Result<(), ApiError> {
        let workspace = match application_provider(&target.provider) {
            Some((workspace, _)) => workspace.to_owned(),
            None => provider_workspace(&target.provider)
                .ok_or_else(no_action_provider)?
                .to_owned(),
        };
        self.deliver_answered(target, arguments, &workspace, Some(answered))
    }

    /// Tell each delivery's caller its action's answer once the action settled: read from the
    /// broker's state of the invocation, on the pass after what the action wrote woke the loop.
    fn poll_answers(&self) {
        let Some(broker) = self.state.borrow().broker.clone() else {
            return;
        };
        let held = match self.awaited.lock() {
            Ok(mut held) => std::mem::take(&mut *held),
            Err(error) => {
                tracing::warn!(%error, "the deliveries awaiting their answer are poisoned");
                return;
            }
        };
        let mut waiting = Vec::new();
        for awaited in held {
            match broker
                .action_observe(&awaited.caller, &awaited.invocation)
                .map(|state| state.update)
            {
                Some(tilo_actions::InvocationUpdate::Succeeded { output, .. }) => {
                    (awaited.answered)(Ok(output.value));
                }
                Some(tilo_actions::InvocationUpdate::Failed { error, .. }) => {
                    (awaited.answered)(Err(error.message));
                }
                Some(tilo_actions::InvocationUpdate::Cancelled { message, .. }) => {
                    (awaited.answered)(Err(
                        message.unwrap_or_else(|| "it was cancelled".to_owned())
                    ));
                }
                None => (awaited.answered)(Err("the action host forgot it".to_owned())),
                Some(_) => waiting.push(awaited),
            }
        }
        if let Ok(mut held) = self.awaited.lock() {
            held.extend(waiting);
        }
    }

    /// Open `app` where the person asked it, through the same run of the workspace's `open`
    /// as [`Self::invoke`]: `place` is what the modifier they held over the row asked of the window.
    fn open_application(
        &self,
        workspace: &str,
        app: &str,
        place: OpenPlace,
    ) -> Result<(), ApiError> {
        self.invoke_at(
            application_open_target(workspace, app),
            serde_json::Map::new(),
            place,
        )
    }

    /// A window of the app's own, beside the one it has, opening on `item`: the shell holds its
    /// tab, as for the app's own ask for another window. An app with no window to open beside
    /// opens the item where its `open` does, which starts it.
    fn open_beside(
        &self,
        target: RemoteActionTarget,
        arguments: serde_json::Map<String, serde_json::Value>,
        item: &str,
    ) -> Result<(), ApiError> {
        if let Some((_, app)) = application_provider(&target.provider) {
            let launch = Launch::Package {
                app: app.to_owned(),
                open: None,
            };
            if self.ask_window(&launch, None, false, false, Some(item)) {
                return Ok(());
            }
        }
        self.invoke(target, arguments)
    }

    fn cancel(&self, _app: &str, _workspace: &str, _invocation: &str) -> Result<(), ApiError> {
        // A job's row carries the invocation the broker follows, and no row is listed yet
        // ([`Self::jobs`]), so nothing names one to stop.
        Err(no_action_provider())
    }

    /// Open `uri` in the app that takes it: the app this workspace's defaults, the person's own or
    /// this machine's open a file with, or the app that claims the scheme of a link.
    fn open_uri(&self, workspace: &str, uri: &str) -> Result<(), ApiError> {
        let person = self.state.borrow().person.clone();
        let shell = self
            .desktop
            .borrow()
            .clone()
            .map(|desktop| desktop.with_policy(|policy| policy.handle().clone()));
        let launch = match uri.strip_prefix("file://") {
            // A file URI is percent-encoded; the path it names is not.
            Some(path) => crate::documents::openers(
                shell.as_ref(),
                workspace,
                &person,
                Path::new(&ui::uri::decode(path)),
            )
            .into_iter()
            .next(),
            None => crate::documents::opener(shell.as_ref(), workspace, &person, uri),
        }
        .ok_or_else(|| ApiError::Failed(format!("nothing on this device opens {uri}")))?;
        // A Tilo app shows a link through its `open-link`, in the window it has or one the
        // delivery starts: the same action the page hands a link to.
        if let Some((target, arguments)) = crate::documents::link_delivery(workspace, &launch) {
            return self.deliver(target, arguments, workspace);
        }
        let workspace = self.workspace_named(workspace);
        self.start(&launch, &workspace)
    }

    fn start_host_app(&self, desktop_id: &str) -> Result<(), ApiError> {
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
    installed(app).map(|package| package.app_id.to_string())
}

/// The package `app` is installed as: the one under that id, or the one whose short name it is.
fn installed(app: &str) -> Option<app_catalogue::apps::InstalledApp> {
    if let Ok(Some(installed)) = app_catalogue::apps::read_one_from_env(app) {
        return Some(installed);
    }
    if app.contains('.') {
        return None;
    }
    let installed = app_catalogue::apps::read_from_env().ok()?;
    installed.into_iter().find(|package| {
        tilo_app_launch::package_identities(&package.app_id)
            .iter()
            .any(|identity| identity.eq_ignore_ascii_case(app))
    })
}

/// Whether `app`'s installed package declares `service` (`[sandbox.services]`): what its launches
/// are handed, and so what it may ask the session about.
pub(crate) fn declares_service(app: &str, service: &str) -> bool {
    installed(app).is_some_and(|package| package.manifest.sandbox.services.contains_key(service))
}

/// Whether a window of `app` runs only on the device that opened it, as the app's installed
/// package says of its windows (`[entrypoint] window = "device"`): what it shows lives in its
/// process here, so the workspace's other devices hold it as this device's window and open no
/// instance of the app for it. An app this machine holds no package of says nothing.
pub(crate) fn runs_on_its_device(app: &str) -> bool {
    installed(app).is_some_and(|package| package.manifest.window_runs_on_its_device())
}

/// Whether every device of a workspace opens a window of `app` on its own, on the record it shows:
/// the app's installed package does not keep its windows on the device that opened them. Such a
/// window is the workspace's already. An app this machine holds no package of says nothing.
pub(crate) fn opens_on_every_device(app: &str) -> bool {
    installed(app).is_some_and(|package| !package.manifest.window_runs_on_its_device())
}

/// Whether a window of `app` is its page, as the app's installed package says: its windows run
/// only on their device (`[entrypoint] window = "device"`) and the app requires a page engine
/// (`[capabilities] required` names `webview`), so what a window of it shows is the page its
/// engine draws. Such a window is shared as that page and never as pictures of it, from before
/// its app said anything to the session ([`crate::window_share`]). An app this machine holds no
/// package of says nothing.
pub(crate) fn window_is_its_page(app: &str) -> bool {
    installed(app).is_some_and(|package| {
        package.manifest.window_runs_on_its_device()
            && package
                .manifest
                .capabilities
                .required
                .iter()
                .any(|capability| capability == "webview")
    })
}

/// Why an action an app publishes cannot be run here: the provider an app's resources arrive on
/// is not mounted in this session.
fn no_action_provider() -> ApiError {
    ApiError::Unavailable {
        code: "action_provider_unavailable".into(),
        reason: "The session runs without an action provider".into(),
    }
}

/// Where a launch's private files live: the per-user runtime directory.
fn runtime_dir() -> PathBuf {
    tilo_paths::xdg::runtime_dir_or_default()
}

/// `launch` as its package can open it: a file of the person's is granted to the app in
/// `workspace`, and the app opens it at the path its own sandbox holds it at, which the session's
/// documents filesystem serves. A package cannot reach the device's own paths, so a launch naming
/// one would start an app that fails on its first read. A folder is Files' to show where it is.
fn handed<'a>(
    launch: &'a Launch,
    workspace: &Workspace,
    documents: Option<&tilo_documents::Documents>,
) -> Result<std::borrow::Cow<'a, Launch>, tilo_app_launch::PlanError> {
    let Launch::Package {
        app,
        open: Some(tilo_app_launch::Open::File(path)),
    } = launch
    else {
        return Ok(std::borrow::Cow::Borrowed(launch));
    };
    if path.is_dir() {
        return Ok(std::borrow::Cow::Borrowed(launch));
    }
    let refused = |reason: String| tilo_app_launch::PlanError::Grants {
        app: app.clone(),
        source: std::io::Error::other(reason),
    };
    let documents = documents
        .ok_or_else(|| refused("this device does not serve the files its apps open".into()))?;
    let key = tilo_documents::AppKey::new(&workspace.id, app)
        .ok_or_else(|| refused("a file is opened in a workspace's app".into()))?;
    if tilo_documents::host::resolve(documents, &key, path).is_some() {
        // Already one of the app's documents: what its record or history names it by.
        return Ok(std::borrow::Cow::Borrowed(launch));
    }
    let opened = documents
        .grant(&key, path, tilo_documents::Kind::Save)
        .map_err(|error| refused(error.to_string()))?;
    Ok(std::borrow::Cow::Owned(Launch::Package {
        app: app.clone(),
        open: Some(tilo_app_launch::Open::File(opened)),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_app_launch::output::OutputTail;
    use tilo_app_launch::{End, Event};
    use tilo_desktop_policy::Window;

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

    /// A package opening a file of the person's is handed it where its sandbox reaches it: the
    /// path it is given is in its own view of the documents served to it, and reading it there
    /// gives the file's bytes. The device's own path never reaches it.
    #[test]
    fn a_package_opening_a_file_reads_its_bytes_where_its_sandbox_holds_it() {
        use std::io::Read as _;
        let home = tempfile::tempdir().unwrap();
        let agenda = home.path().join("Agenda.docx");
        std::fs::write(&agenda, b"PK agenda").unwrap();
        let workspace = Workspace::new(WORKSPACE, home.path().join("capabilities"));
        let documents = tilo_documents::Documents::new();
        let launch = Launch::Package {
            app: "so.tilo.word-processor".to_owned(),
            open: Some(tilo_app_launch::Open::File(agenda.clone())),
        };

        let given = handed(&launch, &workspace, Some(&documents)).unwrap();
        let Launch::Package {
            open: Some(tilo_app_launch::Open::File(opened)),
            ..
        } = given.as_ref()
        else {
            panic!("the launch still opens a file: {given:?}");
        };
        assert!(
            opened.starts_with(tilo_documents::SANDBOX_DIR),
            "{}",
            opened.display()
        );
        assert_eq!(opened.file_name(), agenda.file_name(), "named as it is");
        let key = tilo_documents::AppKey::new(WORKSPACE, "so.tilo.word-processor").unwrap();
        let target = tilo_documents::host::resolve(&documents, &key, opened).unwrap();
        let mut bytes = Vec::new();
        std::fs::File::from(
            target
                .open(rustix::fs::OFlags::RDONLY, rustix::fs::Mode::empty())
                .unwrap(),
        )
        .read_to_end(&mut bytes)
        .unwrap();
        assert_eq!(bytes, b"PK agenda");
        let other = tilo_documents::AppKey::new(WORKSPACE, "so.tilo.notes").unwrap();
        assert!(
            tilo_documents::host::resolve(&documents, &other, opened).is_none(),
            "handed to the app that opens it alone"
        );

        assert_eq!(
            handed(&given, &workspace, Some(&documents))
                .unwrap()
                .as_ref(),
            given.as_ref(),
            "reopening the app's own document, as its history names it, opens it again"
        );
        assert!(
            handed(&launch, &workspace, None).is_err(),
            "no app is started on a path its sandbox cannot read"
        );
        let folder = Launch::Package {
            app: "so.tilo.file-explorer".to_owned(),
            open: Some(tilo_app_launch::Open::File(home.path().to_path_buf())),
        };
        assert_eq!(handed(&folder, &workspace, None).unwrap().as_ref(), &folder);
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

    tilo_test_env::scenario! {
        fn an_agent_runs_as_one_service_with_the_model_chosen_for_it;

        phase an_agent_is_run_as_a_service_with_its_own_argv;
        // An agent is a service the launcher runs, and the agent's own options follow the entrypoint:
        // the workspace its record names it in, the id its actor works as, and the model it plans
        // with.
        {
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

        phase an_agents_model_is_the_keepers_choice_then_the_devices_own;
        // The keeper's choice on the record is what the agent plans with; with none it follows this
        // device's Settings for it.
        {
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

        phase an_agent_is_started_once_and_not_restarted_after_it_ends;
        // One process per agent per session: a pass that sees the same record starts nothing, a
        // process that ended is not started again, and the record's next change is.
        {
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
    }

    /// The desktop the apps are started from, laid out as wide as an output: what its notice tabs
    /// draw on.
    fn shell() -> ShellDesktop {
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

    /// Record that this session started launch `id` of `app` as process `pid`, `lived` ago, and
    /// how far it got: what [`Apps::spawn`] writes when it starts an app, without a process here.
    fn record_launch(
        apps: &Apps,
        id: u64,
        app: &str,
        pid: Option<u32>,
        instance: Option<u64>,
        lived: std::time::Duration,
        got: Got,
    ) {
        let started = std::time::Instant::now()
            .checked_sub(lived)
            .expect("the clock reads past the launch");
        apps.state.borrow_mut().launched.insert(
            id,
            Launching {
                asked: app.to_owned(),
                app: app.to_owned(),
                workspace: WORKSPACE.to_owned(),
                pid,
                instance,
                started,
                got,
            },
        );
    }

    /// What the desktop shows after `end` ended the app `asked` , which this session started with
    /// a window that came.
    fn showed_after(end: End) -> Vec<String> {
        let apps = Apps::with_launcher(SessionEnvironment::from_process(), None, None);
        let desktop = shell();
        apps.attach(desktop.clone());
        record_launch(
            &apps,
            1,
            "example.Canvas",
            None,
            None,
            std::time::Duration::ZERO,
            Got::Landed,
        );
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

    /// An app that ends cleanly before the window the person asked for came never opened: its tab
    /// says it could not open, as for an app that crashed, instead of waiting on a window that is
    /// not coming. Chat did that on the e2e-ws-branch box and left nothing anywhere.
    #[test]
    fn an_app_that_ends_cleanly_before_its_window_came_could_not_open() {
        let apps = Apps::with_launcher(SessionEnvironment::from_process(), None, None);
        let desktop = shell();
        apps.attach(desktop.clone());
        desktop
            .with_policy(|policy| {
                policy
                    .handle()
                    .dispatch(tilo_desktop_surface::navigation::ChromeEffect::OpenApp {
                        app: "example.Canvas".into(),
                        beside: None,
                        background: None,
                        float: None,
                    })
            })
            .unwrap();
        desktop.with_widgets(WidgetSurface::relayout);
        let asked = desktop.take_launches();
        assert_eq!(asked.len(), 1, "the person asked for Canvas");
        // The launch was asked for the tab the shell held for it, and ended before its window
        // came.
        record_launch(
            &apps,
            1,
            "example.Canvas",
            None,
            asked[0].tab,
            std::time::Duration::ZERO,
            Got::Arriving,
        );
        apps.state.borrow().spawner.report(Event {
            id: 1,
            app: "example.Canvas".to_owned(),
            title: "Canvas".to_owned(),
            end: End::exited(0),
            said: OutputTail::default(),
        });
        apps.follow_ended();
        let shown = desktop.with_widgets(|widgets| {
            widgets.relayout();
            ui::a11y::texts(widgets.host_mut().root_widget())
        });
        assert!(
            shown.iter().any(|text| text.contains("could not open")),
            "the tab says Canvas could not open: {shown:?}"
        );
    }

    /// What a person reads when a launch starts nothing: an app this device does not have says so
    /// under the card's title, and no refusal shows a package id.
    #[test]
    fn a_refusal_names_the_app_as_the_shell_names_it() {
        let activity = tilo_app_launch::PlanError::NotInstalled("so.tilo.activity".into());
        assert_eq!(
            refusal_said(&activity),
            "It is not installed on this machine"
        );
        let content = tilo_app_launch::PlanError::ContentUnavailable {
            app: "so.tilo.activity".into(),
        };
        assert!(
            !refusal_said(&content).contains("so.tilo."),
            "no package id: {}",
            refusal_said(&content)
        );
    }

    /// An app opened with no tab waiting for it (through the applications action, as an agent or
    /// a test opens one) that ends at once without ever showing a window never opened: the person
    /// is told it could not open, with what the app itself said, whether it exited 0 or not.
    /// Chat did that on the e2e-ws-branch box, nineteen times, and left nothing anywhere.
    /// `apps` started the app `example.Canvas` as process `id`, `lived` ago, and it ended as `end`
    /// saying `said` on stderr.
    fn canvas_ended(apps: &Apps, id: u64, lived: std::time::Duration, end: End, said: &str) {
        record_launch(apps, id, "example.Canvas", None, None, lived, Got::Arriving);
        apps.state.borrow().spawner.report(Event {
            id,
            app: "example.Canvas".to_owned(),
            title: "Canvas".to_owned(),
            end,
            said: OutputTail {
                stderr: vec![said.to_owned()],
                dropped: 0,
            },
        });
    }

    /// What the shell shows as text now.
    fn texts(desktop: &ShellDesktop) -> Vec<String> {
        desktop.with_widgets(|widgets| {
            widgets.relayout();
            ui::a11y::texts(widgets.host_mut().root_widget())
        })
    }

    /// An app that ended without ever showing a window never opened, however long it ran first
    /// (Chat joined its workspace's record and went, on the e2e-ws-branch box) and however it
    /// ended: the person is told it could not open, with what it said, though no tab waited.
    #[test]
    fn an_app_that_ends_without_ever_showing_a_window_could_not_open() {
        for (n, end) in [End::exited(0), End::exited(1)].into_iter().enumerate() {
            let apps = Apps::with_launcher(SessionEnvironment::from_process(), None, None);
            let desktop = shell();
            apps.attach(desktop.clone());
            let id = u64::try_from(n).unwrap() + 1;
            let lived = std::time::Duration::from_secs(30);
            canvas_ended(&apps, id, lived, end, "the display refused this app");
            apps.follow_ended();
            let shown = texts(&desktop);
            assert!(
                shown.iter().any(|text| text.contains("could not open")),
                "{n}: the person is told Canvas could not open: {shown:?}"
            );
            assert!(
                shown
                    .iter()
                    .any(|text| text.contains("the display refused this app")),
                "{n}: with what Canvas said: {shown:?}"
            );
        }
    }

    /// An app that showed a window and later ended cleanly opened: its end is no notice, even once
    /// its window is gone from the shell.
    #[test]
    fn an_app_that_showed_a_window_and_ended_is_no_notice() {
        let apps = Apps::with_launcher(SessionEnvironment::from_process(), None, None);
        let desktop = shell();
        apps.attach(desktop.clone());
        let lived = std::time::Duration::from_secs(30);
        // A process no longer running stands for the launch's: only its descent matters here.
        let pid = 4_242;
        record_launch(
            &apps,
            1,
            "example.Canvas",
            Some(pid),
            None,
            lived,
            Got::Arriving,
        );
        let window = Window {
            id: 11,
            app_id: Some("example.Canvas".into()),
            title: Some("Canvas".into()),
            pid: Some(pid),
            ..Window::default()
        };
        desktop
            .with_policy(|policy| policy.map(window))
            .expect("the window maps");
        texts(&desktop);
        // The session's pass reads the window the desktop placed as this launch's own.
        apps.follow_ended();
        assert_eq!(
            apps.launch_state("example.Canvas", WORKSPACE),
            Some(LaunchState::Landed),
            "a window of the launch's own process appeared"
        );
        desktop.with_policy(|policy| policy.unmap(11));
        texts(&desktop);
        apps.state.borrow().spawner.report(Event {
            id: 1,
            app: "example.Canvas".to_owned(),
            title: "Canvas".to_owned(),
            end: End::exited(0),
            said: OutputTail::default(),
        });
        apps.follow_ended();
        let shown = texts(&desktop);
        assert!(
            !shown.iter().any(|text| text.contains("could not open")),
            "Canvas opened, and closing is no notice: {shown:?}"
        );
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
