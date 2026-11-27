//! The Windows compatibility service as this session serves it: what only the session knows,
//! handed to the service `compat_service` assembles.
//!
//! - **Who may connect.** A `tilo-launcher` process this session started (its child, running the
//!   launcher's executable) for the Windows Apps manager in the workspace its handoff names, while
//!   that launch is still alive ([`Launches`]). Each launch hands off once. The service refuses any
//!   other app id, any process the session did not start, and a second connection of the same
//!   launch.
//! - **What a program may reach.** The asking workspace's sandbox, read at every request from the
//!   seat's own shell on the loop's pass, or from this device's engine for a workspace the shell
//!   does not show ([`WorkspaceSandboxes`]).
//! - **Where it shows its windows.** The display the session's apps start on, once the compositor
//!   has bound it ([`Display`]): asked when a program starts, never when the service starts, so a
//!   program never gets a display that does not exist yet.
//! - **How a program stays open.** Where the session keeps apps through an in-place restart, each
//!   Windows program starts as one of the session's own launches, behind an app keeper whose
//!   display it shows on ([`Kept`]): the keeper and the program live in a scope of the launch's
//!   own, so memory tiers, a force quit, a crash notice and a logout treat it as any app. The
//!   session that follows a restart takes the program back and hands it to its service again.
//!   A window of the Windows appliance (a `RemoteApp`, or the SPICE viewer) starts the same way.
//!
//! The service works on threads of its own; nothing here blocks the session's loop.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};

use compat_service::{
    Assembly, CapabilityAuthority, CompatSessionHandle, EntryDirectory, ExecutionError,
    ExecutionOutcome, KeptDisplay, KeptProgram, LaunchDisplay, LaunchDisplays, ReopenAnswer,
    ReopenAsk, Reopenings, SandboxCommand, SessionSide, StartedProgram, TakenBack,
    TrustedLauncherRegistry,
};
use tilo_lock::MutexRecover as _;
use tilo_session_core::desktop::state::DesktopHandle;

use crate::Launches;

/// The display the session's apps start on, as the compositor bound it, and the GPU it draws
/// with: written on the loop as they become known, read by the service as a program starts.
#[derive(Default)]
pub struct Display {
    shown: Mutex<Shown>,
    kept: Arc<Kept>,
    /// Which entries the service can open now, and what the session asks it as itself when a
    /// saved tab names one (`Reopen` and `ReopenAsk`): both known once the service is up.
    entries: EntryDirectory,
    reopenings: Mutex<Option<Reopenings>>,
    /// The window class of each Windows program's window, by workspace: what a saved tab of it
    /// reopens with. Read once, written through as programs show their windows.
    identities: Mutex<Option<BTreeMap<(String, String), SavedEntry>>>,
    /// A saved tab's program the service would not open, for the restore to say why in the tab.
    refused: Mutex<Vec<Refused>>,
    /// What each kept program's network proxy was last told to allow, by its control socket, so
    /// only a change is sent and a grant taken back cuts the connections it allowed.
    networks: Mutex<std::collections::BTreeMap<PathBuf, SentPolicy>>,
    /// The filtered egress this desktop serves for the Windows appliance presentations it
    /// adopted: their listener lived in the desktop that started them.
    vm_networks: compat_service::VmNetworkLeases,
    /// What the adopted presentations' egress was last served with, so a pass that changes
    /// nothing neither reads their files nor touches their listeners.
    vm_networks_served: Mutex<Vec<(PathBuf, Vec<String>)>>,
}

/// How a saved tab's Windows program is found: ready to start, still waiting for the service to
/// read what it can open, or gone from it.
pub(crate) enum Reopen {
    Ready,
    Waiting,
    Gone,
}

/// A saved tab's program the service did not open: the tab to say so in, and what to call it.
pub(crate) struct Refused {
    pub tab: u64,
    pub app: String,
    pub name: String,
}

/// What a Windows program's window shows under, by workspace and window class: the entry whose
/// tab it stood in, as the session saw it. A class in one workspace says nothing of another.
const IDENTITIES_FILE: &str = "tilo/windows-compat/window-identities.json";

#[derive(serde::Serialize, serde::Deserialize)]
struct SeenWindow {
    workspace: String,
    window: String,
    entry: String,
    /// The program was a setup or an uninstaller: it ran once, and its tab opens nothing again.
    #[serde(default)]
    setup: bool,
}

/// The entry a saved tab of a Windows program's window opens, and whether that program was a
/// setup, which no later desktop opens again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SavedEntry {
    pub(crate) entry: String,
    pub(crate) setup: bool,
}

/// Where the windows seen by class are kept: the login's data directory, beside the capsules.
fn identities_path() -> Option<PathBuf> {
    Some(tilo_paths::xdg::data_home()?.join(IDENTITIES_FILE))
}

/// The entry a tab of the window class `app` in `workspace` reopens: the program whose window
/// the person saw there, as this computer last knew it.
pub(crate) fn saved_entry(workspace: &str, app: &str) -> Option<SavedEntry> {
    read_identities()?
        .get(&(workspace.to_owned(), app.to_owned()))
        .cloned()
}

fn read_identities() -> Option<BTreeMap<(String, String), SavedEntry>> {
    let path = identities_path()?;
    let text = std::fs::read_to_string(path).ok()?;
    let seen: Vec<SeenWindow> = serde_json::from_str(&text)
        .inspect_err(|error| tracing::warn!(%error, "the Windows programs' windows are unreadable"))
        .ok()?;
    Some(
        seen.into_iter()
            .map(|seen| {
                let entry = SavedEntry {
                    entry: seen.entry,
                    setup: seen.setup,
                };
                ((seen.workspace, seen.window), entry)
            })
            .collect(),
    )
}

fn write_identities(identities: &BTreeMap<(String, String), SavedEntry>) {
    let Some(path) = identities_path() else {
        return;
    };
    let seen: Vec<SeenWindow> = identities
        .iter()
        .map(|((workspace, window), entry)| SeenWindow {
            workspace: workspace.clone(),
            window: window.clone(),
            entry: entry.entry.clone(),
            setup: entry.setup,
        })
        .collect();
    let Ok(text) = serde_json::to_string(&seen) else {
        return;
    };
    if let Err(error) = tilo_files::native::atomic::write_creating_parent(&path, text.as_bytes()) {
        tracing::warn!(%error, "the Windows programs' windows are not saved");
    }
}

/// One kept program's network policy: what its proxy was last told to allow, and when a policy
/// it did not take was last tried.
#[derive(Default)]
struct SentPolicy {
    allowed: Vec<String>,
    tried: Option<std::time::Instant>,
}

#[derive(Default)]
struct Shown {
    wayland_socket: Option<PathBuf>,
    render_node: Option<PathBuf>,
}

impl Display {
    /// The compositor bound its Wayland socket at `socket`.
    pub(crate) fn bound(&self, socket: PathBuf) {
        self.shown.lock_recover().wayland_socket = Some(socket);
    }

    /// The compositor draws on the GPU whose render node is `node`.
    pub(crate) fn drawn_on(&self, node: Option<PathBuf>) {
        self.shown.lock_recover().render_node = node;
    }

    /// Keep each Windows program from now on behind a keeper in a directory `dirs` makes, waking
    /// the session's loop with `wake` when the service asks it something.
    pub(crate) fn keep_in(
        &self,
        dirs: tilo_app_launch::KeptDirs,
        wake: Option<tilo_service::Wake>,
    ) {
        let mut asked = self.kept.asked.lock_recover();
        asked.dirs = Some(dirs);
        asked.wake = wake;
    }

    /// Set each kept program's filtered network to `wanted`, by its proxy's control socket: the
    /// grant spellings the workspace allows it now. A policy that changed cuts the connections
    /// the old one allowed; one that did not is not sent again. Sockets of launches that are gone
    /// are forgotten.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "whether a policy is sent is decided and recorded under one hold, so two passes never both send it"
    )]
    pub(crate) fn apply_networks(&self, wanted: Vec<(PathBuf, Vec<String>)>) {
        // How long a policy a proxy did not take is left before it is tried again: a proxy that
        // is gone never holds the loop for a second attempt a frame.
        const RETRY: std::time::Duration = std::time::Duration::from_secs(2);
        let mut pending = Vec::new();
        {
            let mut sent = self.networks.lock_recover();
            sent.retain(|control, _| wanted.iter().any(|(at, _)| at == control));
            for (control, allowed) in wanted {
                let sent_here = sent.entry(control.clone()).or_default();
                if sent_here.allowed == allowed && sent_here.tried.is_none() {
                    continue;
                }
                if sent_here.tried.is_some_and(|tried| tried.elapsed() < RETRY) {
                    continue;
                }
                pending.push((control, allowed));
            }
        }
        for (control, allowed) in pending {
            let policy = launcher::egress_proxy::EgressPolicy::from_grants(
                allowed.iter().map(String::as_str),
            );
            match launcher::netproxy::set_policy(&control, &policy) {
                Ok(()) => self.policy_sent(&control, Some(allowed)),
                Err(error) => {
                    self.policy_sent(&control, None);
                    tracing::warn!(
                        control = %control.display(),
                        %error,
                        "a kept program's network policy was not set"
                    );
                }
            }
        }
    }

    /// Serve the filtered egress of the adopted Windows appliance presentations in `wanted` —
    /// each launch's keeper directory and the names its workspace grants it now — and of nothing
    /// else: a launch that is gone, or a grant taken back, leaves its guest offline rather than
    /// unfiltered.
    pub(crate) fn adopt_vm_networks(&self, wanted: Vec<(PathBuf, Vec<String>)>) {
        {
            let mut served = self.vm_networks_served.lock_recover();
            if *served == wanted {
                return;
            }
            served.clone_from(&wanted);
        }
        self.vm_networks.adopt(wanted);
    }

    /// Keep what `control`'s proxy was last told: the policy it took, or that one it did not.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the entry borrows the hold it is written through; dropping it before the write is not possible"
    )]
    fn policy_sent(&self, control: &Path, allowed: Option<Vec<String>>) {
        let mut sent = self.networks.lock_recover();
        let sent_here = sent.entry(control.to_path_buf()).or_default();
        match allowed {
            Some(allowed) => {
                sent_here.allowed = allowed;
                sent_here.tried = None;
            }
            None => sent_here.tried = Some(std::time::Instant::now()),
        }
    }

    /// The window class `app` a Windows program's window showed in `workspace` is its entry
    /// `entry`, a setup's when `setup` says so: what a tab of that class there reopens. Written
    /// through, so a start after a reboot finds it.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the map is read and written under one hold, and the file written after it goes"
    )]
    pub(crate) fn seen_window(&self, workspace: &str, app: &str, entry: &str, setup: bool) {
        let written = {
            let mut seen = self.identities.lock_recover();
            let seen = seen.get_or_insert_with(|| read_identities().unwrap_or_default());
            let key = (workspace.to_owned(), app.to_owned());
            let entry = SavedEntry {
                entry: entry.to_owned(),
                setup,
            };
            if seen.get(&key) == Some(&entry) {
                return;
            }
            seen.insert(key, entry);
            seen.clone()
        };
        // Written outside the lock: the file is not a pass's to wait for.
        write_identities(&written);
    }

    /// Whether the saved tab's entry `entry` can be opened again: ready, waiting for the service
    /// to read what it can open, or gone from it.
    pub(crate) fn reopened(&self, entry: &str) -> Reopen {
        if self.reopenings.lock_recover().is_none() {
            // No service of this session's starts Windows programs: none can open it.
            return Reopen::Gone;
        }
        match self.entries.entries() {
            None => Reopen::Waiting,
            Some(entries) if entries.contains(entry) => Reopen::Ready,
            Some(_) => Reopen::Gone,
        }
    }

    /// Ask the service to open the saved tab's entry `entry` in `workspace`, waking the session's
    /// loop with `wake` when it answers. `None` when no service of this session's starts Windows
    /// programs.
    pub(crate) fn ask_reopen(
        &self,
        entry: &str,
        workspace: &str,
        wake: Option<tilo_service::Wake>,
    ) -> Option<ReopenAnswer> {
        let reopenings = self.reopenings.lock_recover().as_ref().cloned()?;
        let answer = ReopenAnswer::default();
        if let Some(wake) = wake {
            answer.waking(move || wake());
        }
        reopenings.ask(ReopenAsk {
            entry_id: entry.to_owned(),
            workspace_id: workspace.to_owned(),
            answer: answer.clone(),
        });
        Some(answer)
    }

    /// The saved tab's program the service would not open, for the restore to say so.
    pub(crate) fn refused(&self, tab: u64, app: String, name: String) {
        self.refused.lock_recover().push(Refused { tab, app, name });
    }

    /// The refusals since the last pass.
    pub(crate) fn take_refused(&self) -> Vec<Refused> {
        std::mem::take(&mut *self.refused.lock_recover())
    }

    /// What the service asked the session's loop since the last pass.
    pub(crate) fn take_asks(&self) -> Vec<Ask> {
        std::mem::take(&mut self.kept.asked.lock_recover().asks)
    }

    /// This desktop took up what the keepers held, `following_restart` an in-place restart or
    /// not.
    pub(crate) fn took_up_kept(&self, following_restart: bool) {
        self.kept.asked.lock_recover().started_fresh = Some(!following_restart);
    }

    /// The launch `id` is the program the service waits on through `end`.
    pub(crate) fn started(&self, id: u64, end: Arc<Ending>) {
        self.kept.asked.lock_recover().ends.insert(id, end);
    }

    /// The launch `event` names ended: a program the service waits on is told how, with what it
    /// said last.
    pub(crate) fn ended(&self, event: &tilo_app_launch::Event) {
        let ending = self.kept.asked.lock_recover().ends.remove(&event.id);
        if let Some(ending) = ending {
            ending.give(outcome_of(event));
        }
    }

    /// The launch `id`, which the desktop before an in-place restart started for the service as
    /// `launch` asked, is this desktop's: the service takes it back, open, or ended as `end` says
    /// when it ended while no desktop was there. Any other launch is passed over.
    pub(crate) fn take_back(
        &self,
        id: u64,
        launch: &tilo_app_launch::kept::KeptLaunch,
        end: Option<&tilo_app_launch::Event>,
    ) {
        let Some((entry_id, _)) = launch.asked.as_deref().and_then(asked_entry) else {
            return;
        };
        let ending = Arc::new(Ending::default());
        match end {
            Some(end) => ending.give(outcome_of(end)),
            None => self.started(id, Arc::clone(&ending)),
        }
        self.kept.asked.lock_recover().taken.push(TakenBack {
            entry_id: entry_id.to_owned(),
            workspace_id: launch.workspace.clone(),
            display_name: launch.title.clone(),
            program: Box::new(Program {
                id,
                ending,
                kept: Arc::clone(&self.kept),
            }),
        });
    }
}

/// What a launch of a Windows program is asked as: the prefix, then its entry in the manager's
/// list. A later desktop reads the entry back from it.
pub(crate) const ASKED_PREFIX: &str = "windows:";

/// What a launch of a capsule's setup or uninstaller is asked as, in place of [`ASKED_PREFIX`]:
/// it is done once it ends, so a tab its window stood in opens nothing again.
pub(crate) const SETUP_PREFIX: &str = "windows-setup:";

/// The entry a launch of a Windows program was asked as, and whether it is a setup: `None` for
/// a launch of anything else.
pub(crate) fn asked_entry(asked: &str) -> Option<(&str, bool)> {
    asked
        .strip_prefix(SETUP_PREFIX)
        .map(|entry| (entry, true))
        .or_else(|| asked.strip_prefix(ASKED_PREFIX).map(|entry| (entry, false)))
}

/// How the service reads an app's end: a program that stopped by itself in failure carries the
/// last of what it, or its sandbox, wrote on its error stream, which reached the keeper's log
/// rather than the service. A program the person ended did not fail, whatever the machine's
/// graphics stack warned about while it ran.
fn outcome_of(event: &tilo_app_launch::Event) -> ExecutionOutcome {
    let end = &event.end;
    let exit_code = match end {
        tilo_app_launch::End::Exited { code, .. } => Some(*code),
        tilo_app_launch::End::Signalled { .. }
        | tilo_app_launch::End::Failed(_)
        | tilo_app_launch::End::KeeperStopped => None,
    };
    ExecutionOutcome::ended(
        exit_code,
        end.crashed() && !event.asked_to_end,
        &event.said.stderr,
        &event.said.stdout,
    )
}

impl LaunchDisplays for Display {
    fn display_for(&self, _workspace_id: &str) -> Result<LaunchDisplay, String> {
        let (wayland_socket, render_node) = {
            let shown = self.shown.lock_recover();
            (shown.wayland_socket.clone(), shown.render_node.clone())
        };
        let wayland_socket = wayland_socket
            .ok_or_else(|| "this session has not bound its display yet".to_owned())?;
        // A display's primary node (`card*`), which the compositor names where the machine has no
        // render node, is no GPU to give a program: it draws in software then.
        let render_node = render_node.filter(|node| compat_service::is_render_node(node));
        Ok(LaunchDisplay {
            uid: rustix::process::geteuid().as_raw(),
            runtime_dir: tilo_paths::xdg::runtime_dir_or_default(),
            wayland_socket,
            render_node,
        })
    }

    fn kept_display(
        &self,
        program: &KeptProgram<'_>,
    ) -> Option<Result<Box<dyn KeptDisplay>, String>> {
        let dirs = self.kept.asked.lock_recover().dirs.clone()?;
        let session = match self.display_for(program.workspace_id) {
            Ok(display) => display,
            Err(error) => return Some(Err(error)),
        };
        let dir = match dirs.reserve() {
            Ok(dir) => dir,
            Err(error) => return Some(Err(format!("no keeper's directory: {error}"))),
        };
        Some(Ok(Box::new(KeptStart {
            display: LaunchDisplay {
                runtime_dir: dir.clone(),
                wayland_socket: dir.join("wayland-0"),
                ..session
            },
            dir: Some(dir),
            dirs,
            launch: Launch {
                app_id: program.app_id.to_owned(),
                title: program.display_name.to_owned(),
                workspace: program.workspace_id.to_owned(),
                asked: format!(
                    "{}{}",
                    if program.setup {
                        SETUP_PREFIX
                    } else {
                        ASKED_PREFIX
                    },
                    program.entry_id
                ),
                x11: program.x11,
            },
            kept: Arc::clone(&self.kept),
        })))
    }

    fn taken_back(&self) -> Vec<TakenBack> {
        std::mem::take(&mut self.kept.asked.lock_recover().taken)
    }

    fn started_fresh(&self) -> Option<bool> {
        self.kept.asked.lock_recover().started_fresh
    }
}

/// The service's side of the programs the session keeps: what it asked the session's loop, and
/// the launches it waits on.
#[derive(Default)]
pub(crate) struct Kept {
    asked: Mutex<Asked>,
}

#[derive(Default)]
struct Asked {
    dirs: Option<tilo_app_launch::KeptDirs>,
    wake: Option<tilo_service::Wake>,
    asks: Vec<Ask>,
    /// The program of each launch, by launch id, while the service waits on its end.
    ends: BTreeMap<u64, Arc<Ending>>,
    /// The programs taken back after a restart, until the service takes them.
    taken: Vec<TakenBack>,
    /// Whether this desktop started fresh, once it took up what the keepers held.
    started_fresh: Option<bool>,
}

impl Kept {
    fn ask(&self, ask: Ask) {
        let wake = {
            let mut asked = self.asked.lock_recover();
            asked.asks.push(ask);
            asked.wake.clone()
        };
        if let Some(wake) = wake {
            wake();
        }
    }
}

/// What the service asks the session's loop.
pub(crate) enum Ask {
    /// Start `launch` as one of the session's own launches, `handed` the program's command, and
    /// answer the launch's id; its end is told to `ending`.
    Start {
        launch: Launch,
        handed: tilo_app_launch::Handed,
        ending: Arc<Ending>,
        reply: Reply,
    },
    /// End the launch.
    Stop(u64),
}

/// A Windows program's launch, as the session starts it.
pub(crate) struct Launch {
    pub(crate) app_id: String,
    pub(crate) title: String,
    pub(crate) workspace: String,
    /// What it is asked as ([`ASKED_PREFIX`], or [`SETUP_PREFIX`] for a setup).
    pub(crate) asked: String,
    /// It shows its windows through X11, on a display its keeper serves.
    pub(crate) x11: bool,
}

/// A keeper's display reserved for one program, until the program starts behind it.
struct KeptStart {
    display: LaunchDisplay,
    /// Taken once the program started.
    dir: Option<PathBuf>,
    dirs: tilo_app_launch::KeptDirs,
    launch: Launch,
    kept: Arc<Kept>,
}

impl KeptDisplay for KeptStart {
    fn display(&self) -> &LaunchDisplay {
        &self.display
    }

    fn start(
        mut self: Box<Self>,
        command: SandboxCommand,
    ) -> Result<Box<dyn StartedProgram>, ExecutionError> {
        let dir = self
            .dir
            .take()
            .ok_or_else(|| ExecutionError::Io(std::io::Error::other("started twice")))?;
        let mut argv = vec![command.program.into_os_string()];
        argv.extend(command.args);
        let answer = Arc::new(Awaited::default());
        let launch = std::mem::replace(
            &mut self.launch,
            Launch {
                app_id: String::new(),
                title: String::new(),
                workspace: String::new(),
                asked: String::new(),
                x11: false,
            },
        );
        let ending = Arc::new(Ending::default());
        self.kept.ask(Ask::Start {
            launch,
            ending: Arc::clone(&ending),
            handed: tilo_app_launch::Handed {
                dir,
                command: argv,
                inherited: command.inherited,
                latency_sensitive: command.latency_sensitive,
            },
            reply: Reply(Arc::clone(&answer)),
        });
        // The session's loop answers on its next pass; a session that went away dropped the ask,
        // which answers that.
        let id = answer.wait().map_err(std::io::Error::other)?;
        Ok(Box::new(Program {
            id,
            ending,
            kept: Arc::clone(&self.kept),
        }))
    }
}

impl Drop for KeptStart {
    fn drop(&mut self) {
        if let Some(dir) = self.dir.take() {
            self.dirs.give_back(&dir);
        }
    }
}

/// A value one thread waits for and another gives, once.
pub(crate) struct Awaited<T> {
    value: Mutex<Option<T>>,
    came: Condvar,
}

impl<T> Default for Awaited<T> {
    fn default() -> Self {
        Self {
            value: Mutex::new(None),
            came: Condvar::new(),
        }
    }
}

impl<T: Clone> Awaited<T> {
    /// Give `value`, unless one was given.
    fn give(&self, value: T) {
        let given = {
            let mut held = self.value.lock_recover();
            let empty = held.is_none();
            if empty {
                *held = Some(value);
            }
            empty
        };
        if given {
            self.came.notify_all();
        }
    }

    /// Wait for the value.
    fn wait(&self) -> T {
        let mut held = self.value.lock_recover();
        loop {
            if let Some(value) = held.clone() {
                return value;
            }
            held = self
                .came
                .wait(held)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
    }
}

/// How a kept program ended, once it did.
pub(crate) type Ending = Awaited<ExecutionOutcome>;

/// The answer to a start the service asked for: a launch's id, or why it did not start. An ask
/// dropped unanswered (the session went away) answers that.
pub(crate) struct Reply(Arc<Awaited<Result<u64, String>>>);

impl Reply {
    pub(crate) fn send(&self, answer: Result<u64, String>) {
        self.0.give(answer);
    }
}

impl Drop for Reply {
    fn drop(&mut self) {
        self.0.give(Err("the session ended".to_owned()));
    }
}

/// A Windows program running as one of the session's launches.
struct Program {
    id: u64,
    ending: Arc<Ending>,
    kept: Arc<Kept>,
}

impl StartedProgram for Program {
    fn wait(&self) -> Result<ExecutionOutcome, ExecutionError> {
        Ok(self.ending.wait())
    }

    fn stop(&self) {
        self.kept.ask(Ask::Stop(self.id));
    }
}

/// The launches this session started, as the service asks about a connecting process.
struct SessionLaunches(Arc<Launches>);

impl TrustedLauncherRegistry for SessionLaunches {
    fn is_launch(
        &self,
        pid: u32,
        process_start_time: u64,
        app_id: &str,
        workspace_id: &str,
    ) -> bool {
        self.0.started_at(pid, app_id, workspace_id) == Some(process_start_time)
    }
}

/// A workspace's sandbox capabilities, whole: restrictions included, as the service filters them to
/// the program's own app id.
type Sandbox = Vec<tilo_permissions::caps::CapOverride>;

/// How long the service waits for the loop to read a sandbox: the loop answers on its next pass, so
/// this only bounds a frozen session.
const SANDBOX_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// Each workspace's sandbox as the service asks it from its own threads, answered on the loop's
/// pass ([`Self::answer`]): the sandbox the seat's shell shows for the workspace, or for one the
/// shell does not show, such as the person's own, the one this device's engine keeps for it.
struct WorkspaceSandboxes {
    asks: Mutex<Vec<(String, tilo_actions::remote::Completer<Option<Sandbox>>)>>,
    wake: tilo_service::Wake,
}

impl WorkspaceSandboxes {
    /// Answer what the service asked since the last pass, from the shell `handle` shows and the
    /// workspaces the device's daemon keeps, as `view` reads them. On the loop's pass only.
    fn answer(&self, handle: &DesktopHandle, view: Option<&crate::workspace_view::WorkspaceView>) {
        let asks = std::mem::take(&mut *self.asks.lock_recover());
        for (workspace, done) in asks {
            let shown = handle.read(|tree| {
                tilo_broker_core::services::workspace_in(tree, &workspace)
                    .map(|shown| shown.sandbox.clone())
            });
            done.complete(shown.or_else(|| view.and_then(|view| kept(view, &workspace))));
        }
    }
}

/// The sandbox the device's daemon keeps for `workspace`, a workspace this device was let into.
/// `None` for one it does not keep.
fn kept(view: &crate::workspace_view::WorkspaceView, workspace: &str) -> Option<Sandbox> {
    let id = tilo_desktop_model::WorkspaceId::parse(workspace)?;
    view.reading(id, |reading| reading.sandbox.clone())
        .flatten()
}

impl CapabilityAuthority for WorkspaceSandboxes {
    fn capabilities_for(&self, workspace_id: &str, _app_id: &str) -> Result<Sandbox, String> {
        let (done, answered) = tilo_actions::remote::pending();
        self.asks
            .lock_recover()
            .push((workspace_id.to_owned(), done));
        (self.wake)();
        answered
            .wait_timeout(SANDBOX_WAIT)
            .map_err(|_| "the session did not answer".to_owned())?
            .flatten()
            .ok_or_else(|| "the workspace is no longer in this session".to_owned())
    }
}

/// The compatibility service while this session serves it.
pub(crate) struct Served {
    /// Filled once the service is up: loading the catalogues and the capsules reads files, so it
    /// starts on a thread of its own rather than the session's loop.
    _service: Arc<Mutex<Option<CompatSessionHandle>>>,
    sandboxes: Arc<WorkspaceSandboxes>,
}

impl Served {
    /// Answer the sandboxes the service asked for since the last pass, from the shell `handle`
    /// shows and the workspaces the device's daemon keeps. On the loop's pass only.
    pub(crate) fn answer_sandboxes(
        &self,
        handle: &DesktopHandle,
        view: Option<&crate::workspace_view::WorkspaceView>,
    ) {
        self.sandboxes.answer(handle, view);
    }
}

/// What the service table serves Windows programs with: the session's launches, the launcher its
/// apps start through, the display they show on, and how the service is put together.
pub(crate) struct Serving {
    launches: Arc<Launches>,
    tilo_launcher: PathBuf,
    display: Arc<Display>,
    assembly: Assembly,
    manager: compat_service::ManagerOut,
}

/// What a session started with `options` serves Windows programs with: nothing when it starts no
/// apps, as no manager can start then.
pub(crate) fn serving(options: &mut crate::Options) -> Option<Serving> {
    let apps = options.launch.as_ref()?;
    Some(Serving {
        launches: Arc::clone(&options.launches),
        tilo_launcher: apps.launcher()?,
        display: apps.windows_display(),
        assembly: std::mem::take(&mut options.windows),
        manager: options.windows_manager.clone(),
    })
}

/// Serve the compatibility service at the launcher's socket for it, asking each workspace's sandbox
/// of the loop, which `wake` wakes ([`Served::answer_sandboxes`]).
pub(crate) fn serve(serving: Serving, wake: tilo_service::Wake) -> Result<Served, String> {
    let Serving {
        launches,
        tilo_launcher,
        display,
        assembly,
        manager,
    } = serving;
    let capsules = tilo_paths::xdg::data_home()
        .ok_or_else(|| "this login has no data directory".to_owned())?
        .join("tilo/windows-compat");
    let sandboxes = Arc::new(WorkspaceSandboxes {
        asks: Mutex::new(Vec::new()),
        wake,
    });
    let reopenings = Reopenings::default();
    *display.reopenings.lock_recover() = Some(reopenings.clone());
    let entries = display.entries.clone();
    let side = SessionSide {
        socket: launcher::services::socket_for(launcher::services::COMPAT_SERVICE),
        launcher: tilo_launcher,
        parent_pid: std::process::id(),
        launches: Arc::new(SessionLaunches(launches)),
        sandboxes: Arc::clone(&sandboxes) as _,
        displays: display,
        capsules,
        entries: entries.clone(),
        reopenings,
        manager,
        administrator: Arc::new(compat_service::account_administers_computer),
    };
    let service = Arc::new(Mutex::new(None));
    let started = Arc::clone(&service);
    std::thread::Builder::new()
        .name("tilo-windows-start".to_owned())
        .spawn(move || {
            let served = launcher::services::ensure_services_root()
                .map_err(|error| error.to_string())
                .and_then(|_| assembly.start(side));
            match served {
                Ok(served) => {
                    tracing::info!("serving Windows programs to the Windows Apps manager");
                    *started.lock_recover() = Some(served);
                }
                Err(error) => {
                    // No Windows program can open here: a saved tab of one says so rather than
                    // waiting for a service that never comes.
                    entries.replace(std::collections::BTreeSet::new());
                    tracing::warn!(%error, "the Windows compatibility service is unavailable");
                }
            }
        })
        .map_err(|error| error.to_string())?;
    Ok(Served {
        _service: service,
        sandboxes,
    })
}

#[cfg(test)]
mod tests {
    /// A setup's launch is asked as one, and both a setup's and a program's launch give back the
    /// entry they were asked for; a launch of anything else names no entry.
    #[test]
    fn a_launch_says_its_entry_and_whether_it_is_a_setup() {
        let setup = format!("{}windows-1", super::SETUP_PREFIX);
        let program = format!("{}windows-2", super::ASKED_PREFIX);
        assert_eq!(super::asked_entry(&setup), Some(("windows-1", true)));
        assert_eq!(super::asked_entry(&program), Some(("windows-2", false)));
        assert_eq!(super::asked_entry("so.tilo.notes"), None);
    }
}
