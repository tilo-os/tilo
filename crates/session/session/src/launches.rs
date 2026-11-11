//! The apps this session started: what a connection to the session's services
//! is admitted by.
//!
//! An app reaches its session's socket with the launcher's handoff, which
//! names the app and the workspace it was launched into. The handoff alone grants
//! nothing: the broker admits the connection only for a launch its host
//! recorded ([`LauncherServices`]), and the record is here — made when the
//! session started the app, keyed by the process it started. The process's
//! start time is part of the record, so a later process that happened to
//! reuse the pid is not taken for the launch.
//!
//! An app's later window opens in the process the session started for the
//! first, and the session makes that window's connections itself. Those
//! connections are recorded apart from the launch they belong to: a window's
//! own is attested as that window, spent by the one connection naming it,
//! and only while the launch it belongs to runs.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::sync::mpsc::SyncSender;
use std::time::Duration;

use tilo_action_broker::services::LauncherServices;
use tilo_app_launch::doors::ancestry;
use tilo_broker_core::services::{
    AuthenticatedApplicationLaunch, LaunchPlace, LaunchableApplication,
};
use tilo_desktop_model::Workspace;
use tilo_lock::MutexRecover as _;

/// How long a service, an app or an agent waits for the session's loop to answer a launch it asked
/// for.
const LAUNCH_ANSWER_WAIT: Duration = Duration::from_secs(10);
/// The most launches one pass may find waiting: more than any person's apps ask for at once, and
/// enough to keep a runaway asker from growing the queue without bound.
const MAX_WAITING_LAUNCHES: usize = 64;

/// One launch the session made: which app it started, in which workspace,
/// the process that runs it, and whether the launch bound the documents store
/// into it.
#[derive(Clone)]
struct Launch {
    app: String,
    workspace: String,
    /// The process's start time, in clock ticks since boot.
    start: u64,
    /// The launch bound the session's documents store into the app, where the app opens the files
    /// the person chose for it: the launcher does that for a package, whose sandbox holds nothing
    /// else of the host, and for no other kind.
    sandboxed: bool,
    /// The launch runs with no window: it takes the app's actions on its records and is never
    /// handed an `open`. An app the session started so shows a window once the shell asks its
    /// process for one ([`Launches::window`]).
    windowless: bool,
    /// The launch serves every workspace its connections name: an endpoint of the app's records
    /// the login runs beside the session ([`Launches::endpoint`]).
    every_workspace: bool,
}

/// One window of a running app the session is connecting: the window's own instance, and the launch
/// whose process it opens in.
struct Window {
    app: String,
    workspace: String,
    instance: u64,
    /// The process of the launch it belongs to, while that runs.
    launch: Option<u32>,
}

/// A launch a service, an app or an agent asked this session for, waiting for the pass that starts
/// it: only the loop starts processes, so whoever asks hands the launch over here and is told how
/// it went.
pub(crate) struct LaunchRequest {
    /// What to start: a package or a host application, at the place it is shown and with what it
    /// opens.
    pub launch: tilo_app_launch::Launch,
    /// The workspace it belongs to, as the asker named it; empty for a launch into no workspace.
    pub workspace: String,
    /// Where the window this launch starts stands, when it starts one: beside the view the asker
    /// works in, or beside the person's without taking their focus. An app the desk already shows
    /// has its window raised instead, whatever place the asker named.
    pub place: LaunchPlace,
    /// What a tool was asked to show, for the tool the launch opens (`TILO_TOOL_CONTEXT`): a
    /// terminal at a folder, a settings route.
    pub context: Option<String>,
    /// The app is to run with no window, for an action that shows nothing
    /// ([`LauncherServices::windowless`]).
    pub windowless: bool,
    /// Told once the launch was made, or why it was not.
    reply: Box<dyn FnOnce(Result<(), String>) + Send>,
}

impl LaunchRequest {
    /// Tell the asker how the launch went.
    pub(crate) fn answer(self, result: Result<(), String>) {
        (self.reply)(result);
    }
}

/// The launches this session made, and the window connections it is making for them.
#[derive(Default)]
pub struct Launches {
    /// What each window this session runs shows, read from the broker every pass: a window's
    /// instance is its tab's id, which the session tells the app when it starts it
    /// ([`crate::continuations`]).
    notes: std::sync::Arc<crate::continuations::Continuations>,
    started: Mutex<BTreeMap<u32, Launch>>,
    windows: Mutex<Vec<Window>>,
    /// The launch each window's shell-ui connection belongs to, and the tab the window takes, by
    /// app, while the connection is being made: the shell-ui service reads it for the connection
    /// from the session's own process.
    shell_ui: Mutex<Vec<(String, u32, u64)>>,
    /// This device's apps, as the session's lists last read them: what a service asking the
    /// launcher is told of every app it could open.
    applications: Mutex<Option<crate::lists::Lists>>,
    /// The launches asked for since the last pass of the session's loop: the one path everything
    /// that is not the desk itself starts apps through.
    waiting: Mutex<Vec<LaunchRequest>>,
    /// What wakes that loop when a launch is waiting for it.
    wake: Mutex<Option<tilo_service::Wake>>,
    /// The apps whose models this session runs itself, by the app: the resource each is served at.
    /// A plain `open` of one places that resource rather than awaiting a window process that never
    /// publishes (`docs/designs/app-language/one-app-model.md` §3).
    served: Mutex<BTreeMap<String, String>>,
}

impl Launches {
    /// What the windows this session runs show, kept with its launches: one store for the desk, the
    /// layout record and the arrangement.
    pub(crate) fn notes(&self) -> std::sync::Arc<crate::continuations::Continuations> {
        std::sync::Arc::clone(&self.notes)
    }

    /// Wake the session's loop with `wake` when a launch is waiting for it: the loop is the only
    /// thread that starts a process.
    pub fn waking(&self, wake: tilo_service::Wake) {
        *self.wake.lock_recover() = Some(wake);
    }

    /// Record that this session runs `app`'s model itself, serving its record at `root`: its plain
    /// `open` places that root instead of awaiting a window process
    /// (`docs/designs/app-language/one-app-model.md` §3). Each pass that reads the services
    /// publishes it again, so the record moving is followed.
    pub fn serves(&self, app: &str, root: &str) {
        let mut served = self.served.lock_recover();
        if served.get(app).map(String::as_str) == Some(root) {
            return;
        }
        served.insert(app.to_owned(), root.to_owned());
    }

    /// Ask the session's loop to make `launch` in `workspace` at `place`,
    /// opening the tool `context` names when the launch opens one, and wait
    /// for it: the answer is the same one the desk's own launches get. The
    /// place says where the window this launch starts stands; a launch of an
    /// app the desk already shows brings that window forward instead of
    /// starting one, and the place has nothing to say about it.
    ///
    /// # Errors
    /// The loop did not answer in time, more launches are waiting than a
    /// person's apps ask for, or the launch did not start.
    pub fn ask(
        &self,
        launch: tilo_app_launch::Launch,
        workspace: &str,
        place: LaunchPlace,
        context: Option<String>,
    ) -> Result<(), String> {
        let (reply, answered): (SyncSender<Result<(), String>>, _) =
            std::sync::mpsc::sync_channel(1);
        self.hand_over(LaunchRequest {
            launch,
            workspace: workspace.to_owned(),
            place,
            context,
            windowless: false,
            reply: Box::new(move |result| {
                let _ = reply.send(result);
            }),
        })?;
        match answered.recv_timeout(LAUNCH_ANSWER_WAIT) {
            Ok(result) => result,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                Err("the session did not answer the launch in time".into())
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                Err("the session gave the launch up".into())
            }
        }
    }

    /// Hand `request` to the session's loop, the only thread that starts a process, and wake it.
    ///
    /// # Errors
    /// More launches are waiting than a person's apps ask for.
    fn hand_over(&self, request: LaunchRequest) -> Result<(), String> {
        {
            let mut waiting = self.waiting.lock_recover();
            if waiting.len() >= MAX_WAITING_LAUNCHES {
                return Err("too many launches are waiting for this session".into());
            }
            waiting.push(request);
        }
        let wake = self.wake.lock_recover().clone();
        if let Some(wake) = wake {
            wake();
        }
        Ok(())
    }

    /// The launches waiting for this pass, oldest first: taken once, by the loop that starts them.
    pub(crate) fn take_waiting(&self) -> Vec<LaunchRequest> {
        std::mem::take(&mut *self.waiting.lock_recover())
    }

    /// Whether a launch waits for the loop: what a pass that starts nothing else still has to
    /// settle.
    pub(crate) fn waiting(&self) -> bool {
        !self.waiting.lock_recover().is_empty()
    }

    /// The apps this device has, as `lists` last read them: what a service
    /// asking the launcher is told of every app it could open — its name,
    /// what it offers while it is not running, and the capabilities its
    /// signed manifest declares, which decide the reach the device's
    /// settings app has. Nothing is offered until a session lends them.
    pub fn watching(&self, lists: crate::lists::Lists) {
        *self.applications.lock_recover() = Some(lists);
    }

    /// Record that `pid` runs `app` in `workspace` , as this session started it: the app's
    /// connection is admitted from here. A pid whose process cannot be read is recorded by nothing
    /// and starts an app that reaches no service.
    /// `sandboxed` is whether the launch bound the session's documents store into it, which only a
    /// package's sandbox is given.
    pub fn started(&self, pid: u32, app: &str, workspace: &str, sandboxed: bool) {
        self.record(pid, app, workspace, sandboxed, false);
    }

    /// [`Self::started`] for a package launch with no window
    /// ([`LauncherServices::windowless`]): its connection takes the app's actions on its records
    /// and is never handed an `open`, until the shell asks its process for a window.
    pub fn started_windowless(&self, pid: u32, app: &str, workspace: &str) {
        self.record(pid, app, workspace, true, true);
    }

    fn record(&self, pid: u32, app: &str, workspace: &str, sandboxed: bool, windowless: bool) {
        let Some(start) = start_time(pid) else {
            tracing::warn!(
                pid,
                %app,
                "a started app's process could not be read: its connection reaches no service"
            );
            return;
        };
        self.started.lock_recover().insert(
            pid,
            Launch {
                app: app.to_owned(),
                workspace: workspace.to_owned(),
                start,
                sandboxed,
                windowless,
                every_workspace: false,
            },
        );
    }

    /// Record that `pid`, a process this session did not start, runs `app` in `workspace` with no
    /// window: an endpoint of the app's records the login runs beside this session, such as the
    /// Files daemon. Its connections are admitted as the app's and take its actions on its records,
    /// for every workspace it names, since the endpoint serves them all; an `open` is never handed
    /// to it. A pid whose process cannot be read is recorded by nothing.
    pub fn endpoint(&self, pid: u32, app: &str, workspace: &str) {
        self.record(pid, app, workspace, false, true);
        if let Some(launch) = self.started.lock_recover().get_mut(&pid) {
            launch.every_workspace = true;
        }
    }

    /// Record that the session is about to connect window `instance` of `app` in `workspace` , a
    /// window of the launch whose process is `launch` while it runs. A window has one record:
    /// recording it again replaces the one it had. Only the session records windows, and only the
    /// connection naming the instance spends the record.
    pub fn window(&self, app: &str, workspace: &str, instance: u64, launch: Option<u32>) {
        // A launch that ran with no window shows one from here on.
        if let Some(pid) = launch
            && let Some(launched) = self.started.lock_recover().get_mut(&pid)
        {
            launched.windowless = false;
        }
        let mut windows = self.windows.lock_recover();
        windows.retain(|window| !window.is(app, workspace, instance));
        windows.push(Window {
            app: app.to_owned(),
            workspace: workspace.to_owned(),
            instance,
            launch,
        });
    }

    /// The session gave the connection it recorded up before the broker could attest it: its record
    /// ends.
    pub fn window_withdrawn(&self, app: &str, workspace: &str, instance: u64) {
        self.windows
            .lock_recover()
            .retain(|window| !window.is(app, workspace, instance));
    }

    /// Record that the session is about to connect the shell-ui socket for window `instance` of
    /// `app`, which the launch in `launcher` runs: the shell-ui service reads it for a connection
    /// from the session's own process.
    pub fn window_shell_ui(&self, app: &str, launcher: u32, instance: u64) {
        self.shell_ui
            .lock_recover()
            .push((app.to_owned(), launcher, instance));
    }

    /// The launch a connection from the session's own process for a window of `app` belongs to,
    /// and the tab that window takes: the one it recorded while making the connection.
    pub fn take_window_shell_ui(&self, app: &str) -> Option<(u32, u64)> {
        let mut pending = self.shell_ui.lock_recover();
        let index = pending.iter().position(|(asked, ..)| asked == app)?;
        let (_, launcher, instance) = pending.remove(index);
        drop(pending);
        Some((launcher, instance))
    }

    /// The apps this session started with the documents store bound into them, and that still run:
    /// the ones a portal answer can name a store path for.
    #[must_use]
    pub fn sandboxed_apps(&self) -> Vec<String> {
        let mut apps: Vec<String> = self
            .started
            .lock_recover()
            .iter()
            .filter(|(pid, launch)| launch.sandboxed && start_time(**pid) == Some(launch.start))
            .map(|(_, launch)| launch.app.clone())
            .collect();
        apps.sort();
        apps.dedup();
        apps
    }

    /// The apps this session started that still run, one entry per app: what an application-opened
    /// trigger follows, so an app the person starts again after closing it shows up again.
    #[must_use]
    pub fn running_apps(&self) -> std::collections::BTreeSet<String> {
        self.started
            .lock_recover()
            .iter()
            .filter(|(pid, launch)| !launch.windowless && start_time(**pid) == Some(launch.start))
            .map(|(_, launch)| launch.app.clone())
            .collect()
    }

    /// The workspace of the launch that `pid` is or was started by, when it runs `app` : the app a
    /// connection came from, as the window it names.
    pub fn workspace_of(&self, pid: u32, app: &str) -> Option<String> {
        self.ancestor(pid, |launch| launch.app == app)
            .map(|(_, launch)| launch.workspace)
    }

    /// The launch `pid` is or descends from, while that launch still runs: the app window a process
    /// belongs to, so two windows of one app are told apart rather than the app being asked about
    /// as a whole.
    #[must_use]
    pub fn launch_of(&self, pid: u32) -> Option<u32> {
        self.ancestor(pid, |_| true).map(|(launched, _)| launched)
    }

    /// The app this session launched that `pid` descends from, while that launch still runs: the
    /// app a program of the login is running in, such as the terminal that started it. `None` when
    /// no launch is an ancestor of `pid` .
    #[must_use]
    pub fn app_of_descendant(&self, pid: u32) -> Option<String> {
        self.ancestor(pid, |_| true).map(|(_, launch)| launch.app)
    }

    /// The first launch `pid` is or descends from that `wanted` takes, while it still runs.
    fn ancestor(&self, pid: u32, wanted: impl Fn(&Launch) -> bool) -> Option<(u32, Launch)> {
        let lineage = ancestry(pid);
        let (launched, launch) = self
            .started
            .lock_recover()
            .iter()
            .find(|(launched, launch)| wanted(launch) && lineage.contains(launched))
            .map(|(launched, launch)| (*launched, launch.clone()))?;
        (start_time(launched) == Some(launch.start)).then_some((launched, launch))
    }

    /// The start time of the process `pid` started and whether that launch runs with no window,
    /// when that process is the launch of `app` and still runs: in `workspace` for a window, and
    /// for whichever workspace its connection names when it is the app's windowless endpoint,
    /// which serves every workspace the session holds.
    fn live_launch(&self, pid: u32, app: &str, workspace: &str) -> Option<(u64, bool)> {
        let (start, windowless) = match self.started.lock_recover().get(&pid) {
            Some(launch)
                if launch.app == app
                    && (launch.every_workspace || launch.workspace == workspace) =>
            {
                (launch.start, launch.windowless)
            }
            _ => return None,
        };
        (start_time(pid) == Some(start)).then_some((start, windowless))
    }

    /// The start time of the process `pid` started, when that process is the launch of `app` in
    /// `workspace` and still runs.
    fn live(&self, pid: u32, app: &str, workspace: &str) -> Option<u64> {
        self.live_launch(pid, app, workspace)
            .map(|(start, _)| start)
    }

    /// The window record a connection from the session's own process naming `app` in `workspace`
    /// and `instance` spends, while the launch it belongs to runs. Nothing else spends it, and it
    /// is spent once.
    fn take_window(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
        instance: Option<u64>,
    ) -> Option<AuthenticatedApplicationLaunch> {
        if pid != std::process::id() {
            return None;
        }
        let instance = instance.filter(|instance| *instance != 0)?;
        let record = {
            let mut windows = self.windows.lock_recover();
            let index = windows
                .iter()
                .position(|window| window.is(app, workspace, instance))?;
            windows.remove(index)
        };
        if let Some(launch) = record.launch
            && self.live(launch, app, workspace).is_none()
        {
            // The window's launch is over: its windows went with it.
            return None;
        }
        Some(AuthenticatedApplicationLaunch {
            creation: None,
            continuation_instance: Some(instance),
            launch_ticket: None,
            windowless: false,
        })
    }
}

impl Window {
    fn is(&self, app: &str, workspace: &str, instance: u64) -> bool {
        self.app == app && self.workspace == workspace && self.instance == instance
    }
}

impl LauncherServices for Launches {
    /// This device's apps, as its lists last read them: what an app or a service asking the action
    /// door may open here, and what each app declared it may do.
    fn applications(&self) -> Vec<LaunchableApplication> {
        self.applications
            .lock_recover()
            .as_ref()
            .map(|lists| lists.latest().launchable.clone())
            .unwrap_or_default()
    }

    /// Start `app` in `workspace` at `place`, opening `record` of it when the
    /// broker named one: the launch is made by the session's loop, on the
    /// same path the desk's own launches take, and the answer here is that
    /// pass's.
    ///
    /// # Errors
    /// `app` names nothing this session starts, or the launch did not start.
    fn launch(
        &self,
        app: &str,
        workspace: &Workspace,
        record: Option<&str>,
        place: &LaunchPlace,
    ) -> Result<(), String> {
        let launch = if let Some(record) = record {
            tilo_app_launch::Launch::Record {
                app: app.to_owned(),
                record: record.to_owned(),
            }
        } else {
            match tilo_app_launch::Launch::parse(app) {
                Some(launch) => launch,
                None => return Err(format!("{app} is not an application")),
            }
        };
        if matches!(
            launch,
            tilo_app_launch::Launch::Window { .. } | tilo_app_launch::Launch::Crashed { .. }
        ) {
            return Err(format!("{app} starts no process"));
        }
        self.ask(launch, &workspace.id().to_string(), place.clone(), None)
    }

    fn attest_process(&self, pid: u32, app: &str, workspace: &str) -> bool {
        self.live(pid, app, workspace).is_some()
    }

    /// Whether this session runs `app`'s model itself, so it has no window process for a launch
    /// to bring up: the resource the session serves the app at, when one does.
    fn served(&self, app: &str) -> Option<String> {
        self.served.lock_recover().get(app).cloned()
    }

    /// Have package `app` run with no window in `workspace`, for an action that shows nothing:
    /// the session's loop starts it unless a process of it runs there already, and the start's
    /// answer is the one returned. An app this session runs itself, or one that is no package,
    /// has no such instance.
    ///
    /// # Errors
    /// More launches are waiting than a person's apps ask for.
    fn windowless(
        &self,
        app: &str,
        workspace: &str,
    ) -> Result<Option<tilo_broker_core::services::WindowlessStart>, String> {
        if self.served(app).is_some() {
            return Ok(None);
        }
        let Some(launch @ tilo_app_launch::Launch::Package { open: None, .. }) =
            tilo_app_launch::Launch::parse(app)
        else {
            return Ok(None);
        };
        let (reply, started) = tokio::sync::oneshot::channel();
        self.hand_over(LaunchRequest {
            launch,
            workspace: workspace.to_owned(),
            place: LaunchPlace::Aside,
            context: None,
            windowless: true,
            reply: Box::new(move |result| {
                let _ = reply.send(result);
            }),
        })?;
        Ok(Some(started))
    }

    /// The connection proved the process the session started is the one holding it (the kernel's
    /// credentials and the challenge answered on them): the launch stands only while both name it.
    fn attest_executing_process(
        &self,
        launcher_pid: u32,
        peer: &runtime_portal::process_binding::ExecutingPeer,
        app: &str,
        workspace: &str,
    ) -> bool {
        self.live(launcher_pid, app, workspace)
            .is_some_and(|start| peer.belongs_to(launcher_pid, start))
    }

    /// A launched app's record stays for its whole life; the session's own
    /// record for a window of it is spent by the one connection naming the
    /// window's instance.
    ///
    /// # Errors
    /// Reading the connection's creation evidence failed.
    fn authenticated_launch(
        &self,
        pid: u32,
        app: &str,
        workspace: &str,
        window_instance: Option<u64>,
    ) -> Result<Option<AuthenticatedApplicationLaunch>, String> {
        if let Some((_, windowless)) = self.live_launch(pid, app, workspace) {
            return Ok(Some(AuthenticatedApplicationLaunch {
                creation: self.application_creation_evidence(pid, app, workspace)?,
                continuation_instance: None,
                launch_ticket: None,
                windowless,
            }));
        }
        Ok(self.take_window(pid, app, workspace, window_instance))
    }
}

/// A process's start time, in clock ticks since boot, read from `/proc/<pid>/stat` : what tells a
/// process from a later one that reused its pid.
fn start_time(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // comm is parenthesised and may hold spaces or parentheses: index from the last ')'. Field 3
    // (state) comes first, so starttime is field 19.
    let rest = stat.rsplit_once(") ")?.1;
    let fields: Vec<&str> = rest.split_whitespace().collect();
    // ignored-failure: a stat field that is no number names no process start
    fields.get(19)?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(target_os = "linux")]
    use tilo_test_env::SpawnUnderEnvLock as _;

    /// A window's own connection is spent by the one connection naming its app, workspace and
    /// instance, and only the session's own process makes one: nothing else is admitted by it, and
    /// nothing is left for a second.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_window_connection_is_spent_once_by_the_window_it_names() {
        let launches = Launches::default();
        let app = "so.tilo.code.window";
        let workspace = "workspace-window";
        let session = std::process::id();
        launches.window(app, workspace, 42, None);

        assert!(
            launches
                .authenticated_launch(session, app, "other-workspace", Some(42))
                .unwrap()
                .is_none(),
            "a window of another workspace"
        );
        assert!(
            launches
                .authenticated_launch(session, app, workspace, Some(41))
                .unwrap()
                .is_none(),
            "another window"
        );
        assert!(
            launches
                .authenticated_launch(u32::MAX, app, workspace, Some(42))
                .unwrap()
                .is_none(),
            "only the session connects a window"
        );

        let bound = launches
            .authenticated_launch(session, app, workspace, Some(42))
            .unwrap()
            .expect("the window's own connection");
        assert_eq!(bound.continuation_instance, Some(42));
        assert!(
            launches
                .authenticated_launch(session, app, workspace, Some(42))
                .unwrap()
                .is_none(),
            "spent by its connection"
        );
    }

    /// An app started with no window is admitted as such in its own workspace alone, is no app
    /// the person opened, and shows a window once the shell asks its process for one.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_windowless_launch_is_admitted_as_one_until_it_shows_a_window() {
        let launches = Launches::default();
        let (app, workspace) = ("so.tilo.chat", "workspace-windowless");
        let process = std::process::id();
        launches.started_windowless(process, app, workspace);

        let admitted = |workspace| launches.authenticated_launch(process, app, workspace, None);
        assert!(admitted(workspace).unwrap().expect("its launch").windowless);
        assert!(admitted("another-workspace").unwrap().is_none());
        assert!(
            launches.running_apps().is_empty(),
            "no app the person opened"
        );

        launches.window(app, workspace, 7, Some(process));
        assert!(!admitted(workspace).unwrap().expect("its launch").windowless);
        assert!(launches.running_apps().contains(app));
    }

    /// The session gave the connection up before the broker saw it: the record ends with it, and
    /// nothing is admitted for that window.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_withdrawn_window_connection_admits_nothing() {
        let launches = Launches::default();
        let app = "so.tilo.code.withdrawn";
        let workspace = "workspace-withdrawn";
        let session = std::process::id();
        launches.window(app, workspace, 9, None);
        launches.window_withdrawn(app, workspace, 9);
        assert!(
            launches
                .authenticated_launch(session, app, workspace, Some(9))
                .unwrap()
                .is_none()
        );
    }

    /// A window of a launch runs in that launch's process: once the process is over its windows are
    /// too, and their connections are nobody's.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_window_of_an_ended_launch_is_not_admitted() {
        let launches = Launches::default();
        let app = "so.tilo.code.ended";
        let workspace = "workspace-ended";
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn_locked()
            .expect("a process to launch");
        launches.started(child.id(), app, workspace, true);
        launches.window(app, workspace, 7, Some(child.id()));
        child.kill().ok();
        child.wait().ok();

        assert!(
            launches
                .authenticated_launch(std::process::id(), app, workspace, Some(7))
                .unwrap()
                .is_none(),
            "the launch that made the window is over"
        );
    }

    /// Only a launch that bound the store into its app, and still runs,
    /// makes that app one whose files are answered with store paths: an app
    /// the launch bound no store into has none, and neither has one whose
    /// sandbox is over.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_sandboxed_app_is_one_whose_sandboxed_launch_runs() {
        let launches = Launches::default();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn_locked()
            .expect("a process to launch");
        launches.started(child.id(), "so.tilo.sandboxed", "workspace-sandboxed", true);
        launches.started(
            std::process::id(),
            "so.tilo.host",
            "workspace-sandboxed",
            false,
        );

        assert_eq!(launches.sandboxed_apps(), ["so.tilo.sandboxed".to_owned()]);

        child.kill().ok();
        child.wait().ok();
        assert!(
            launches.sandboxed_apps().is_empty(),
            "the sandbox that was bound is over"
        );
    }

    /// A program's app is the launch it descends from, while that launch runs: the terminal that
    /// started it, not any other launch of the session.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_program_s_app_is_the_launch_it_descends_from() {
        let launches = Launches::default();
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn_locked()
            .expect("a process to launch");
        assert_eq!(launches.app_of_descendant(child.id()), None, "no launch");

        launches.started(
            std::process::id(),
            "so.tilo.terminal",
            "workspace-desk",
            false,
        );
        assert_eq!(
            launches.app_of_descendant(std::process::id()).as_deref(),
            Some("so.tilo.terminal"),
            "the app of a process the session started"
        );
        assert_eq!(
            launches.app_of_descendant(child.id()).as_deref(),
            Some("so.tilo.terminal"),
            "the app of a program running under it"
        );

        child.kill().ok();
        child.wait().ok();
        assert_eq!(
            launches.app_of_descendant(std::process::id()).as_deref(),
            Some("so.tilo.terminal"),
            "the launch still runs"
        );
    }

    /// An endpoint the login runs with no window is admitted as its app in its workspace, and says
    /// so: the broker hands it the app's actions on its records and never an `open`. It is neither
    /// another app's, another workspace's, nor a later process that reused its pid.
    #[test]
    #[cfg(target_os = "linux")]
    fn a_windowless_endpoint_is_admitted_as_its_app_and_says_so() {
        let launches = Launches::default();
        let app = "so.tilo.file-explorer";
        let workspace = "workspace-files";
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn_locked()
            .expect("a process to launch");
        launches.endpoint(child.id(), app, workspace);

        assert!(
            launches
                .authenticated_launch(std::process::id(), app, workspace, None)
                .unwrap()
                .is_none(),
            "another process is not the endpoint"
        );
        assert!(
            launches
                .authenticated_launch(child.id(), "so.tilo.notes", workspace, None)
                .unwrap()
                .is_none(),
            "another app"
        );
        assert!(
            launches
                .authenticated_launch(child.id(), app, "another-workspace", None)
                .unwrap()
                .is_some(),
            "the endpoint serves every workspace the session holds"
        );
        let admitted = launches
            .authenticated_launch(child.id(), app, workspace, None)
            .unwrap()
            .expect("the endpoint's connection");
        assert!(admitted.windowless, "it takes actions and never an open");
        assert!(
            launches
                .authenticated_launch(child.id(), app, workspace, None)
                .unwrap()
                .is_some(),
            "an endpoint's record stays for its whole life, as a launch's does"
        );

        child.kill().ok();
        child.wait().ok();
        assert!(
            launches
                .authenticated_launch(child.id(), app, workspace, None)
                .unwrap()
                .is_none(),
            "the endpoint is over"
        );
    }

    /// The shell-ui service learns which launch a window connection from the session's own process
    /// belongs to, once for each window it connects.
    #[test]
    fn a_window_s_shell_ui_connection_names_its_launch_once() {
        let launches = Launches::default();
        launches.window_shell_ui("so.tilo.notes", 1234, 99);
        assert_eq!(
            launches.take_window_shell_ui("so.tilo.notes"),
            Some((1234, 99))
        );
        assert_eq!(
            launches.take_window_shell_ui("so.tilo.notes"),
            None,
            "spent by its connection"
        );
        assert_eq!(launches.take_window_shell_ui("so.tilo.code"), None);
    }
}
