//! The sessions a workspace VM's daemon runs ([`tilo_action_services::environment_window`]).
//!
//! A hosted workspace's VM runs one daemon, a device of the person that reaches that one
//! workspace. It runs the VM's two sessions as its shells, each handed its link as any shell the
//! daemon starts: Browser's, as the image's browser account, and the Linux apps', as the
//! workspace's tenant. Each is an OS account of the VM, not a device: what a session does, it does
//! through this daemon. Its entry ([`BROWSER_ENTRY`], [`APPS_ENTRY`]) is started as root and
//! drops to its own account, keeping the link.
//!
//! The host launches into a session through the workspace's dispatch ([`wire::LAUNCH`]), which
//! starts it on its first launch, and reads what the sessions show ([`wire::WINDOW`]). The session
//! takes its launches from here ([`wire::NEXT_LAUNCH`]) and says when its window is up and in use
//! ([`wire::REPORT`]). A session unused for [`Limits::idle`], or alive for [`Limits::lifetime`],
//! is ended; once the last one ended, the daemon ends, and the VM it ran in sleeps.

use std::io;
use std::process::Child;
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde_json::Value;
use tilo_action_services::environment_device::{self, ItemUse, ItemUsed, WindowShare};
use tilo_action_services::environment_window::{
    self as wire, Holds, ItemLent, Launch, Report, Session, Share, Shown, Window,
};
use tilo_action_services::linux_apps::{AppLaunch, UP_PROVIDER};
use tilo_actions::api::PlacedAction;
use tilo_actions::contract::ActionKind;
use tilo_actions::remote::{
    ActionCallError, Admitted, Caller, InvokeArgument, InvokeRequest, Outcome, Pending,
    ReceiptStatus,
};
use tilo_lock::MutexRecover as _;

use super::confined::{argument, descriptor, output, refused};

/// The entry of the session holding a workspace's Browser, as the guest image ships it.
pub const BROWSER_ENTRY: &str = "/usr/libexec/tilo/browser-session";
/// The entry of the session holding a workspace's Linux apps, as the guest image ships it.
pub const APPS_ENTRY: &str = "/usr/libexec/tilo/apps-session";

/// The app whose window the Browser session holds: an action run on it is that session in use.
const BROWSER_APP: &str = "so.tilo.browser";
/// How long a session's ask for its next launch waits for one before it answers none.
const NEXT_WAIT: Duration = Duration::from_secs(30);
/// How often the sessions are looked at for their limits.
const LOOK: Duration = Duration::from_secs(15);
/// How long a session is given to end once asked (Browser writes its profile back) before it is
/// killed.
const GRACE: Duration = Duration::from_secs(15);

/// How long a session runs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    /// Ended once unused this long.
    pub idle: Duration,
    /// Ended once alive this long, used or not.
    pub lifetime: Duration,
}

impl Default for Limits {
    /// Ten minutes idle, two hours alive.
    fn default() -> Self {
        Self {
            idle: Duration::from_secs(600),
            lifetime: Duration::from_secs(2 * 60 * 60),
        }
    }
}

/// What starts the session `holds` in the workspace it names, as one of this daemon's shells.
pub(crate) type Starts = Arc<dyn Fn(Holds, &str) -> io::Result<Child> + Send + Sync>;

/// The VM's sessions, as its daemon runs them.
pub(crate) struct VmEnvironment {
    state: Mutex<State>,
    starts: Starts,
    /// Told when a launch is queued for a session.
    queued: tokio::sync::Notify,
    limits: Limits,
    /// Told once a session ran and none runs any more: the daemon's end.
    ended: Box<dyn Fn() + Send + Sync>,
}

#[derive(Default)]
struct State {
    browser: Running,
    apps: Running,
    /// Whether any session ran yet.
    ran: bool,
}

impl State {
    const fn of(&mut self, holds: Holds) -> &mut Running {
        match holds {
            Holds::Browser => &mut self.browser,
            Holds::Apps => &mut self.apps,
        }
    }
}

/// One session as the daemon runs it.
#[derive(Default)]
struct Running {
    child: Option<Child>,
    started_ms: u64,
    provider: Option<String>,
    used_ms: u64,
    /// What was launched for it and it has not taken yet.
    queued: Vec<Launch>,
}

impl Running {
    /// Whether its process runs; one that ended is forgotten, with what it showed.
    fn running(&mut self) -> bool {
        let ended = match &mut self.child {
            None => return false,
            Some(child) => !matches!(child.try_wait(), Ok(None)),
        };
        if ended {
            *self = Self::default();
        }
        !ended
    }

    fn shown(&mut self) -> Shown {
        Shown {
            running: self.running(),
            provider: self.provider.clone(),
            used_ms: self.used_ms,
        }
    }

    /// Ask its process to end, and kill it once [`GRACE`] is over, on a thread of its own.
    fn end(&mut self) {
        if let Some(child) = self.child.take() {
            end_child(child);
        }
        *self = Self::default();
    }
}

/// Ask `child` to end with `SIGTERM`, and kill it once [`GRACE`] is over.
fn end_child(mut child: Child) {
    if let Ok(pid) = i32::try_from(child.id())
        && let Some(pid) = rustix::process::Pid::from_raw(pid)
    {
        let _ = rustix::process::kill_process(pid, rustix::process::Signal::TERM);
    }
    let spawned = std::thread::Builder::new()
        .name("vm-session-end".into())
        .spawn(move || {
            let began = std::time::Instant::now();
            while began.elapsed() < GRACE {
                if !matches!(child.try_wait(), Ok(None)) {
                    return;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let _ = child.kill();
            let _ = child.wait();
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "a VM session's end is not waited for");
    }
}

impl VmEnvironment {
    /// The sessions `starts` starts, ended at `limits`, telling `ended` once the last one ended.
    pub(crate) fn new(
        starts: Starts,
        limits: Limits,
        ended: Box<dyn Fn() + Send + Sync>,
    ) -> Arc<Self> {
        let environment = Arc::new(Self {
            state: Mutex::default(),
            starts,
            queued: tokio::sync::Notify::new(),
            limits,
            ended,
        });
        let watched = Arc::downgrade(&environment);
        let spawned = std::thread::Builder::new()
            .name("vm-sessions".into())
            .spawn(move || watch(&watched));
        if let Err(error) = spawned {
            tracing::warn!(%error, "the VM's sessions are not ended at their limits");
        }
        environment
    }

    /// Open `launch`, starting its session when none runs.
    fn launch(&self, launch: Launch) -> Result<Shown, String> {
        if !launch.is_whole() {
            return Err("a launch names a record for Browser, or an app for the Linux apps".into());
        }
        if let Some(app) = &launch.app {
            AppLaunch::new(&app.desktop_id)?;
        }
        let now = tilo_clock::unix_now_ms();
        let mut state = self.state.lock_recover();
        state.ran = true;
        let running = state.of(launch.holds);
        if !running.running() {
            let child = (self.starts)(launch.holds, &launch.workspace)
                .map_err(|error| format!("the session did not start: {error}"))?;
            tracing::info!(holds = ?launch.holds, pid = child.id(), "a VM session started");
            *running = Running {
                child: Some(child),
                started_ms: now,
                used_ms: now,
                ..Running::default()
            };
        }
        running.used_ms = now;
        if launch.holds == Holds::Apps {
            running.provider = Some(UP_PROVIDER.to_owned());
        }
        running.queued.push(launch);
        let shown = running.shown();
        drop(state);
        self.queued.notify_waiters();
        Ok(shown)
    }

    fn window(&self) -> Window {
        let mut state = self.state.lock_recover();
        Window {
            browser: state.browser.shown(),
            apps: state.apps.shown(),
        }
    }

    fn report(&self, report: Report) {
        let now = tilo_clock::unix_now_ms();
        let mut state = self.state.lock_recover();
        match report {
            Report::Up { holds, provider } => {
                let running = state.of(holds);
                running.provider = Some(provider);
                running.used_ms = now;
            }
            Report::Used { holds } => state.of(holds).used_ms = now,
        }
    }

    fn stop(&self, holds: Holds) {
        self.state.lock_recover().of(holds).end();
    }

    /// What was launched for `holds` and it has not taken yet.
    fn take(&self, holds: Holds) -> Vec<Launch> {
        std::mem::take(&mut self.state.lock_recover().of(holds).queued)
    }

    /// What was launched for `holds`, once something was or [`NEXT_WAIT`] is over.
    fn next_launch(self: &Arc<Self>, holds: Holds) -> Pending<Outcome> {
        let (done, answer) = tilo_actions::remote::pending();
        let environment = Arc::clone(self);
        tilo_rt::app().spawn(async move {
            let waited = tokio::time::timeout(NEXT_WAIT, async {
                loop {
                    // Followed before it is looked at, so a launch between the two still wakes it.
                    let queued = environment.queued.notified();
                    let taken = environment.take(holds);
                    if !taken.is_empty() {
                        return taken;
                    }
                    queued.await;
                }
            })
            .await;
            let taken = waited.unwrap_or_else(|_| environment.take(holds));
            done.complete(output(&taken));
        });
        answer
    }

    /// End what is past its limits, and tell the daemon to end once nothing runs after something
    /// did: whether it was told.
    fn keep_limits(&self, now_ms: u64) -> bool {
        let millis = |duration: Duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        let mut state = self.state.lock_recover();
        for holds in [Holds::Browser, Holds::Apps] {
            let running = state.of(holds);
            if !running.running() {
                continue;
            }
            let idle = now_ms.saturating_sub(running.used_ms) >= millis(self.limits.idle);
            let old = now_ms.saturating_sub(running.started_ms) >= millis(self.limits.lifetime);
            if idle || old {
                tracing::info!(?holds, idle, old, "a VM session ended at its limits");
                running.end();
            }
        }
        let over = state.ran && !state.browser.running() && !state.apps.running();
        drop(state);
        if over {
            (self.ended)();
        }
        over
    }
}

impl Drop for VmEnvironment {
    fn drop(&mut self) {
        let mut state = self.state.lock_recover();
        state.browser.end();
        state.apps.end();
    }
}

/// Keep `environment`'s sessions to their limits while it lives, until the daemon is told to end.
fn watch(environment: &Weak<VmEnvironment>) {
    loop {
        std::thread::sleep(LOOK);
        let Some(environment) = environment.upgrade() else {
            return;
        };
        if environment.keep_limits(tilo_clock::unix_now_ms()) {
            return;
        }
    }
}

/// The host's asks: what it opens in the VM, reads of it, and ends there. Every device of the
/// workspace reaches them, as the workspace grants it.
pub(crate) struct Hosted(pub(crate) Arc<VmEnvironment>);

impl tilo_actions::remote::Admits for Hosted {
    const PROVIDER: &'static str = wire::PROVIDER;
    const APP: &'static str = wire::PROVIDER;

    fn rows(&self) -> Vec<PlacedAction> {
        tilo_actions::remote::admitted::top_level(vec![
            descriptor(
                wire::LAUNCH,
                "Open in the workspace's machine",
                ActionKind::Outside,
            ),
            descriptor(
                wire::WINDOW,
                "What the workspace's machine shows",
                ActionKind::Read,
            ),
            descriptor(
                wire::STOP,
                "End a session of the workspace's machine",
                ActionKind::Outside,
            ),
        ])
    }

    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        _admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        let address = &request.target.address;
        if address.resource != wire::RESOURCE {
            return Err(format!(
                "this machine publishes no resource `{}`",
                address.resource
            ));
        }
        let answered = match address.action.as_str() {
            wire::LAUNCH => {
                let launch: Launch = argument(request)?;
                if caller
                    .workspace()
                    .is_some_and(|asked| asked != launch.workspace)
                {
                    return Err("a launch opens in the workspace it was asked in".to_owned());
                }
                self.0.launch(launch).map(|shown| output(&shown))?
            }
            wire::WINDOW => output(&self.0.window()),
            wire::STOP => {
                let Session { holds } = argument(request)?;
                self.0.stop(holds);
                output(&serde_json::json!({}))
            }
            other => return Err(format!("this machine has no action `{other}`")),
        };
        Ok(Pending::ready(answered))
    }
}

/// The sessions' own asks: what was launched for one, what it tells of its window, and what it
/// asks of the host through this device. Only a shell on this device reaches them, as the
/// person's own shell calls (`window_follow` alike).
pub(crate) struct Sessions(pub(crate) Arc<VmEnvironment>, pub(crate) HostAsks);

impl tilo_actions::remote::Admits for Sessions {
    const PROVIDER: &'static str = wire::SESSIONS;
    const APP: &'static str = tilo_app_id::apps::SHELL;

    fn rows(&self) -> Vec<PlacedAction> {
        tilo_actions::remote::admitted::top_level(vec![
            descriptor(
                wire::NEXT_LAUNCH,
                "What a session opens next",
                ActionKind::Read,
            ),
            descriptor(
                wire::REPORT,
                "A session's window is up or in use",
                ActionKind::Outside,
            ),
            descriptor(
                wire::USE_ITEM,
                "A vault item's text for one fill of a page",
                ActionKind::Read,
            ),
            descriptor(
                wire::SHARE_WINDOW,
                "Share a window of this machine, or end its share",
                ActionKind::Outside,
            ),
        ])
    }

    fn start(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
        _admitted: &Admitted,
    ) -> Result<Pending<Outcome>, String> {
        let acting = caller.acting();
        if !acting.is_person()
            || acting.app().is_some()
            || caller.is_session_delegated()
            || caller.is_foreign_device()
        {
            return Err("only a session of this machine asks this".to_owned());
        }
        let address = &request.target.address;
        if address.resource != wire::RESOURCE {
            return Err(format!(
                "this machine publishes no resource `{}`",
                address.resource
            ));
        }
        match address.action.as_str() {
            wire::NEXT_LAUNCH => {
                let Session { holds } = argument(request)?;
                Ok(self.0.next_launch(holds))
            }
            wire::REPORT => {
                self.0.report(argument(request)?);
                Ok(Pending::ready(output(&serde_json::json!({}))))
            }
            wire::USE_ITEM => Ok(self.1.use_item(workspace_of(caller)?, argument(request)?)),
            wire::SHARE_WINDOW => Ok(self.1.share(workspace_of(caller)?, argument(request)?)),
            other => Err(format!("this machine has no action `{other}`")),
        }
    }
}

/// The workspace a session asks in: the only one it asks the host about.
fn workspace_of(caller: &Caller) -> Result<&str, String> {
    caller
        .workspace()
        .ok_or_else(|| "a session asks in the workspace it serves".to_owned())
}

/// What a session is told when the host gave it nothing for a fill.
const NOTHING_LENT: &str =
    "The workspace's server answered nothing for the item, so nothing was filled.";
/// What a session is told when what the host answered does not open here: it names nothing of
/// what it holds.
const NOT_SEALED_FOR_THIS: &str = "What the workspace's server answered was not sealed for \
                                   this fill of this page on this machine, so nothing was filled.";

/// How the sessions ask the workspace's host for what this machine holds no keys for
/// ([`environment_device`]): as this device, through its dispatch, which hands the ask to the
/// host the workspace is placed on, and opening what the host sealed to this device's key. A
/// session holds no key, so the text and keys it is lent are opened here and handed to it over
/// its link.
#[derive(Clone)]
pub(crate) struct HostAsks {
    providers: tilo_workspace_service::workspaces::providers::Providers,
    /// This device, as the host knows it.
    device: Caller,
    key: ed25519_dalek::SigningKey,
}

impl HostAsks {
    /// Asking the host through `daemon`'s dispatch, as its device.
    pub(crate) fn of(daemon: &super::services::Daemon) -> Self {
        Self {
            providers: daemon.workspaces().providers(),
            device: tilo_workspace_service::service::local_caller(
                daemon.workspaces().device_cert(),
            ),
            key: daemon.key().clone(),
        }
    }

    /// Ask the host for `action` with `argument` in `workspace`: what it answered, or why it did
    /// not, in the host's own words where it gave any.
    fn ask(
        &self,
        workspace: &str,
        action: &str,
        argument: InvokeArgument,
    ) -> Pending<Result<Option<Value>, String>> {
        let mut request =
            InvokeRequest::plain(environment_device::target(action), serde_json::Map::new());
        request.argument = argument;
        let caller = self.device.clone().in_workspace(workspace.to_owned());
        let Some(asked) = self.providers.invoke(&caller, request) else {
            return Pending::ready(Err(
                "This machine cannot reach its workspace's server.".to_owned()
            ));
        };
        asked.map(|answered| {
            let reason = |error: ActionCallError| match error {
                ActionCallError::Refused { reason } => reason,
                other => other.to_string(),
            };
            match answered.map_err(reason)?.status {
                ReceiptStatus::Completed { output } => Ok(output.value),
                ReceiptStatus::Rejected { error } => Err(reason(error)),
                ReceiptStatus::Failed { error } => Err(error.message),
                ReceiptStatus::Cancelled { message } => {
                    Err(message
                        .unwrap_or_else(|| "The workspace's server stopped the ask.".to_owned()))
                }
                ReceiptStatus::Accepted => {
                    Err("The workspace's server has not decided yet.".to_owned())
                }
            }
        })
    }

    /// The text of the vault item `used` names for its fill, opened with this device's key, or
    /// that its person is asked first, or why nothing was released: always an answer, which the
    /// session hands Browser.
    fn use_item(&self, workspace: &str, used: ItemUse) -> Pending<Outcome> {
        self.lend(workspace, used).map(|lent| output(&lent))
    }

    /// The text of the vault item `used` names for its fill, opened with this device's key, or
    /// that its person is asked first, or why nothing was released.
    pub(crate) fn lend(&self, workspace: &str, used: ItemUse) -> Pending<ItemLent> {
        let (key, workspace_id) = (self.key.clone(), workspace.to_owned());
        self.ask(
            workspace,
            environment_device::USE_ITEM,
            wire::argument(&used),
        )
        .map(move |answered| item_lent(&workspace_id, &key, &used, answered))
    }

    /// Have the host share one of this device's windows, or end its share.
    fn share(&self, workspace: &str, share: Share) -> Pending<Outcome> {
        let action = if share.shared {
            environment_device::SHARE_WINDOW
        } else {
            environment_device::END_WINDOW_SHARE
        };
        let window = WindowShare {
            app: share.app,
            tab: share.tab,
        };
        self.ask(workspace, action, wire::argument(&window))
            .map(|answered| match answered {
                Ok(_) => output(&serde_json::json!({})),
                Err(reason) => Err(Box::new(refused(reason))),
            })
    }
}

/// What a session is answered for `used`, from what the host answered this device: a text it
/// sealed to `key` for this workspace, this page action and this site is opened, a question's
/// words and a refusal's reason are carried, and a box sealed for anything else opens for nothing.
/// No failure here says anything of the text.
fn item_lent(
    workspace: &str,
    key: &ed25519_dalek::SigningKey,
    used: &ItemUse,
    answered: Result<Option<Value>, String>,
) -> ItemLent {
    let refused = |reason: &str| ItemLent::Refused {
        reason: reason.to_owned(),
    };
    let answer = match answered {
        Ok(Some(answer)) => answer,
        Ok(None) => return refused(NOTHING_LENT),
        Err(reason) => return ItemLent::Refused { reason },
    };
    match serde_json::from_value::<ItemUsed>(answer) {
        Ok(ItemUsed::Asked { words }) => ItemLent::Asked { words },
        Ok(ItemUsed::Released { sealed, into }) => {
            let Ok(sealed) = hex::decode(sealed) else {
                return refused(NOT_SEALED_FOR_THIS);
            };
            let bound = key_group::ReleasedFor {
                workspace,
                invocation: *used.invocation.0.as_bytes(),
                origin: &used.origin,
            };
            match key_group::open_released_text(key, &bound, &sealed) {
                Ok(text) => ItemLent::Text {
                    text: text.as_str().to_owned(),
                    into,
                },
                Err(_) => refused(NOT_SEALED_FOR_THIS),
            }
        }
        Err(_) => refused(
            "The workspace's server answered something that is no item's use, so nothing was \
             filled.",
        ),
    }
}

/// Answer `environment`'s actions on `daemon`'s dispatch: the host's for every device of the
/// workspace, the sessions' for this device's shells alone.
pub(crate) fn provide(daemon: &super::services::Daemon, environment: &Arc<VmEnvironment>) {
    daemon.provide(wire::PROVIDER, Arc::new(Hosted(Arc::clone(environment))));
    daemon.asks_its_host(HostAsks::of(daemon));
    daemon.workspaces().providers().provide(
        wire::SESSIONS,
        tilo_app_id::apps::SHELL,
        tilo_workspace_service::workspaces::providers::Reach::ThisDevice,
        Arc::new(Sessions(Arc::clone(environment), HostAsks::of(daemon))),
    );
}

/// Answer the VM's sessions' actions on `daemon`'s dispatch, when this daemon is a workspace VM's
/// ([`wire::MARK`]), starting each session as one of its shells, and telling `stop` once the last
/// one ended.
#[cfg(target_os = "linux")]
pub(crate) fn serve(daemon: &Arc<super::services::Daemon>, stop: &super::serve::Stop) {
    if std::env::var_os(wire::MARK).is_none() {
        return;
    }
    let shells = Arc::downgrade(daemon);
    let starts: Starts = Arc::new(move |holds, workspace| {
        let daemon = shells
            .upgrade()
            .ok_or_else(|| io::Error::other("this machine's daemon is stopping"))?;
        daemon.start_shell(session_command(holds, workspace), None)
    });
    let stop = stop.clone();
    let environment = VmEnvironment::new(
        starts,
        Limits::default(),
        Box::new(move || {
            tracing::info!("no session of this machine runs any more: its daemon ends");
            stop.tell();
        }),
    );
    provide(daemon, &environment);
    count_page_actions(daemon, &environment);
}

/// Count every action run on Browser's window as its session in use, as somebody viewing the page
/// is: a page action an agent runs is admitted by this daemon's own dispatch, so the daemon counts
/// it, and an agent working a page keeps its session within the idle limit.
#[cfg(target_os = "linux")]
fn count_page_actions(daemon: &super::services::Daemon, environment: &Arc<VmEnvironment>) {
    use tilo_app_host::collab_app_broker::ExecutionStatus;
    use tokio::sync::broadcast::error::RecvError;
    let Some(mut activity) = daemon.execution_activity() else {
        tracing::warn!("this machine's daemon runs no broker: a page action keeps no session");
        return;
    };
    let environment = Arc::downgrade(environment);
    tilo_rt::app().handle().spawn(async move {
        loop {
            match activity.recv().await {
                Ok(ran) => {
                    if ran.route.app_id != BROWSER_APP || ran.status != ExecutionStatus::Accepted {
                        continue;
                    }
                    let Some(environment) = environment.upgrade() else {
                        return;
                    };
                    environment.report(Report::Used {
                        holds: Holds::Browser,
                    });
                }
                Err(RecvError::Lagged(_)) => {}
                Err(RecvError::Closed) => return,
            }
        }
    });
}

/// The session `holds` as its entry starts it for `workspace`: as root, which the entry drops to
/// the session's own account, with nothing of the daemon's environment but what places it.
#[cfg(target_os = "linux")]
fn session_command(holds: Holds, workspace: &str) -> std::process::Command {
    let mut command = std::process::Command::new(match holds {
        Holds::Browser => BROWSER_ENTRY,
        Holds::Apps => APPS_ENTRY,
    });
    command
        .env_clear()
        .envs(std::env::vars_os().filter(|(name, _)| {
            [
                "PATH",
                "LANG",
                "RUST_LOG",
                "TILO_SESSION_ARGS",
                "TILO_HEADLESS_SIZE",
            ]
            .iter()
            .any(|kept| name == kept)
        }))
        .env(holds.mark(), "1")
        .env(wire::WORKSPACE, workspace)
        .stdin(std::process::Stdio::null());
    // The tenant's commands find the daemon's tenant socket as any client of a daemon whose
    // sockets are in a named profile does, and Browser is handed no path to it at all.
    if holds == Holds::Apps
        && let Some(tenant) = std::env::var_os(crate::daemon::TENANT_SOCKETS_ENV)
    {
        command.env(tilo_machine::DAEMON_PROFILE_VAR, tenant);
    }
    command
}

#[cfg(test)]
#[path = "vm_environment_tests.rs"]
mod tests;
