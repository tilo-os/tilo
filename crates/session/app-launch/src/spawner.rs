//! Starting a planned app, and hearing how it ended.
//!
//! The session runs one loop, so a launch must not wait: [`Spawner::start`]
//! starts the process and returns, and a thread of its own waits for the
//! child and reports how it ended on [`Spawner::events`]. Waiting for every
//! child on one thread would make each launch wait for the one before it to
//! exit, so each child gets a thread — an app is started rarely enough that
//! those are the only threads involved.
//!
//! What a session needs from the report is whether the app *chose* to end
//! ([`End::crashed`]): an app that exited with a failure or was killed left
//! its windows behind, and the person is offered the app again rather than
//! the desktop going down with it.
//!
//! An app that ended because memory ran out is not a crash the app caused:
//! the session's memory watch ended it and remembers why
//! ([`Cause`]), or the kernel's own OOM killer did ([`crate::memory`]). The
//! report carries that with the status ([`End::memory`]), so the person is
//! told the session closed the app rather than that it stopped working.
//!
//! The apps a session started end with it: dropping the [`Spawner`] asks each
//! one still running to end (`SIGTERM`), through a pidfd taken while the
//! child was still unreaped, so the signal can reach no later process that
//! took its pid. Their display was the session's compositor, gone with it;
//! and a test's session, dropped as the test ends or unwinds from a failed
//! assertion, leaves no app behind.

use std::collections::BTreeMap;
use std::os::fd::AsFd as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::output::AppOutput;

use tilo_lock::MutexRecover as _;

use crate::links::HeldLink;
use crate::memory;
use crate::plan::Planned;

/// Why the system ended an app: what the launcher remembers of it.
pub use launcher::memory_pressure::Cause;

/// What wakes the session's loop when an app ends.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// How a launched app ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum End {
    /// It exited with this status.
    Exited {
        code: i32,
        /// Why memory ended it, when memory did.
        memory: Option<Cause>,
    },
    /// This signal ended it.
    Signalled {
        signal: i32,
        /// Why memory ended it, when memory did.
        memory: Option<Cause>,
    },
    /// It never started, and why.
    Failed(String),
}

impl End {
    /// An app that exited with `code` on its own.
    #[must_use]
    pub const fn exited(code: i32) -> Self {
        Self::Exited { code, memory: None }
    }

    /// An app this signal ended.
    #[must_use]
    pub const fn signalled(signal: i32) -> Self {
        Self::Signalled {
            signal,
            memory: None,
        }
    }

    /// An app that never started.
    #[must_use]
    pub fn failed(reason: impl Into<String>) -> Self {
        Self::Failed(reason.into())
    }

    /// The same end, with memory having closed the app (`cause`).
    #[must_use]
    pub fn remembering(mut self, cause: Cause) -> Self {
        match &mut self {
            Self::Exited { memory, .. } | Self::Signalled { memory, .. } => {
                *memory = Some(cause);
            }
            Self::Failed(_) => {}
        }
        self
    }

    /// Why memory ended the app, when it did: `None` for an app that chose to
    /// end, for one that never started, and for one the launcher holds no
    /// memory of.
    #[must_use]
    pub fn memory(&self) -> Option<Cause> {
        match self {
            Self::Exited { memory, .. } | Self::Signalled { memory, .. } => *memory,
            Self::Failed(_) => None,
        }
    }

    /// Whether the app ended without choosing to: anything but a clean exit
    /// is a crash, including one the app itself reports as a failure.
    #[must_use]
    pub fn crashed(&self) -> bool {
        !matches!(self, Self::Exited { code: 0, .. })
    }
}

impl std::fmt::Display for End {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exited { code, .. } => write!(f, "exited with {code}"),
            Self::Signalled { signal, .. } => write!(f, "was killed by signal {signal}"),
            Self::Failed(reason) => write!(f, "did not start: {reason}"),
        }
    }
}

/// A launched app that ended.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Event {
    /// The launch this is about, as [`Spawner::start`] returned it.
    pub id: u64,
    /// The app's package id or desktop-file id.
    pub app: String,
    /// The name the app was shown by.
    pub title: String,
    pub end: End,
    /// The last of what the app wrote, when this session read its streams.
    /// Empty for an ending reported by someone who started nothing.
    pub said: crate::output::OutputTail,
}

/// A launched app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Started {
    pub id: u64,
    pub pid: u32,
}

/// Why an app did not start.
#[derive(Debug, thiserror::Error)]
#[error("{app} did not start: {reason}")]
pub struct SpawnError {
    pub app: String,
    pub reason: String,
}

/// The apps this session started, and what became of them.
pub struct Spawner {
    wake: Option<Wake>,
    events: (mpsc::Sender<Event>, Receiver<Event>),
    next: u64,
    /// The apps still running, by launch id: each one's watcher removes it
    /// once it has ended, and the drop ends the rest.
    running: Running,
    /// Why memory ended an app, by its cgroup, until its end is reported.
    ends: memory::Ends,
    /// The watch over the apps' memory: theirs while they run.
    memory: Option<launcher::memory_monitor::Watch>,
    /// What starts an app behind a keeper, once the session has a desktop to attach keepers to.
    #[cfg(target_os = "linux")]
    keeping: Option<crate::kept::Keeping>,
}

/// What ends a session: a logout ends every app it runs; an in-place restart of the desktop leaves
/// the apps kept behind a keeper running for the run that follows ([`crate::kept`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Teardown {
    /// The login ends, or the session ends for any reason but an in-place restart.
    Logout,
    /// The desktop restarts in place, as its wrapper's marker says.
    InPlaceRestart,
}

/// The apps a spawner started that have not ended yet.
pub(crate) type Running = Arc<Mutex<BTreeMap<u64, App>>>;

pub(crate) fn running(running: &Running) -> std::sync::MutexGuard<'_, BTreeMap<u64, App>> {
    running.lock_recover()
}

impl Default for Spawner {
    fn default() -> Self {
        Self::new(None)
    }
}

impl Spawner {
    /// A spawner that wakes `wake`'s loop whenever an app ends.
    #[must_use]
    pub fn new(wake: Option<Wake>) -> Self {
        let running = Running::default();
        let ends = memory::Ends::default();
        // Started here, so it lives as long as the apps it watches and no
        // longer: a session that ends leaves no watch behind.
        let memory = match launcher::memory_monitor::Watch::spawn(
            "tilo-app-memory",
            Box::new(memory::Source {
                running: Arc::clone(&running),
            }),
            Box::new(memory::Effects {
                ends: Arc::clone(&ends),
            }),
        ) {
            Ok(watch) => Some(watch),
            Err(error) => {
                tracing::warn!(%error, "memory pressure goes unwatched for the apps");
                None
            }
        };
        Self {
            wake,
            events: mpsc::channel(),
            next: 0,
            running,
            ends,
            memory,
            #[cfg(target_os = "linux")]
            keeping: None,
        }
    }

    /// Start the apps that are not Tilo's behind `keeping`'s keeper from now on, attached to its
    /// desktop ([`crate::kept`]).
    #[cfg(target_os = "linux")]
    pub fn keep_behind(&mut self, keeping: crate::kept::Keeping) {
        self.keeping = Some(keeping);
    }

    /// Put every cgroup a launch of this session runs in in its tier:
    /// `on_screen` is the process of each window on an output, `own` the
    /// cgroup this session itself runs in. A launch that is still opening has
    /// the startup tier; an app with no window on screen is in the background
    /// tier and gives memory back first; one whose window the person sees
    /// again is in the foreground tier. `own` keeps the session's tier.
    pub fn tier_by_visibility(&self, on_screen: &[u32], own: Option<&Path>) {
        memory::tier_by_visibility(&self.running, on_screen, own);
    }

    /// Start `planned` with exactly the environment it planned: nothing the
    /// session carries leaks in, and the app gets no terminal.
    ///
    /// # Errors
    /// The program could not be started.
    pub fn start(&mut self, planned: &Planned) -> Result<Started, SpawnError> {
        self.start_with(planned, None, &|_| {}, "", None)
            .map(|(started, _)| started)
    }

    /// [`Self::start`] with the link `link` of the app's workspace, which
    /// the app is handed with its release so it opens the app's later
    /// windows itself.
    ///
    /// The launcher waits at a registration gate until `record` has written
    /// the launch down: an app that connected before its launch was known
    /// would be refused by the session's services. A launch without a link
    /// gets no gate, and its command line is its stdin.
    ///
    /// The link comes back held for as long as the app runs, so the caller
    /// keeps it until the app ends.
    ///
    /// A launch the plan says is kept ([`Planned::kept`]) starts behind a
    /// keeper when the session has one: `workspace` and `asked` are written
    /// down with it, so a later run takes it back as the same launch.
    ///
    /// # Errors
    /// The program could not be started.
    pub fn start_with(
        &mut self,
        planned: &Planned,
        mut link: Option<HeldLink>,
        record: &dyn Fn(u32),
        workspace: &str,
        asked: Option<&str>,
    ) -> Result<(Started, Option<HeldLink>), SpawnError> {
        crate::name_surface(planned);
        let mut command = Command::new(&planned.program);
        command
            .args(&planned.args)
            .env_clear()
            .envs(&planned.env)
            .stdin(Stdio::null())
            // Both streams are read back into the session's log: what an app
            // says is the only account of a launch that failed inside it.
            // Nothing the app writes ever reaches the session's own streams.
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let diagnostics = diagnostics_file(planned, &mut command);
        let output = Arc::new(AppOutput::new(planned.app.clone(), diagnostics));
        let gate = match link {
            Some(_) => match launcher::registration_gate::install(&mut command) {
                Ok(gate) => Some(gate),
                // Without its gate the launch would run unreleased: it starts
                // nothing, as an app that cannot start does.
                Err(error) => {
                    return Err(SpawnError {
                        app: planned.app.clone(),
                        reason: format!("no launch gate: {error}"),
                    });
                }
            },
            None => None,
        };
        let kept = self.kept_launch(planned, workspace, asked, &mut command);
        let mut child = command.spawn().map_err(|error| SpawnError {
            app: planned.app.clone(),
            reason: error.to_string(),
        })?;
        drop(command);
        // Both pipes are read on threads of their own, so an app that writes
        // more than a pipe holds never waits for the session, and a session
        // that has stopped reading never holds the app.
        if let Some(stdout) = child.stdout.take() {
            output.relay_stdout(stdout);
        }
        if let Some(stderr) = child.stderr.take() {
            output.relay_stderr(stderr);
        }
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        let started = Started {
            id,
            pid: child.id(),
        };
        // Taken before the watcher can reap the child, so it names this
        // process and no later one with its pid.
        let app_of = self.app_of(started.pid, &planned.app, kept.as_ref());
        running(&self.running).insert(id, app_of);
        record(started.pid);
        if let Some(gate) = gate {
            // The launch is written down: the app may connect.
            let served = link.as_mut().and_then(|link| link.serve(started.pid));
            let released = match &served {
                Some(theirs) => gate.release_with_window_link(theirs.as_fd()),
                None => gate.release(),
            };
            if let Err(error) = released {
                tracing::warn!(app = %planned.app, %error, "the launch was not released");
            }
        }
        let app = planned.app.clone();
        let title = planned.title.clone();
        let events = self.events.0.clone();
        let wake = self.wake.clone();
        let still_running = Arc::clone(&self.running);
        let ends = Arc::clone(&self.ends);
        std::thread::Builder::new()
            .name(format!("app-{}", planned.app))
            .spawn(move || {
                let end = ended(child, &ends);
                // A kept launch the session let go of for a restart is the next run's to report.
                #[cfg(target_os = "linux")]
                if let Some(kept) = &kept {
                    if kept.released() {
                        return;
                    }
                    kept.remove();
                }
                // What the app said before it ended, which the session keeps
                // for the report it makes of a launch that never opened: both
                // pipes are read to their end first, so the tail is all of it.
                output.finish();
                let said = output.tail();
                running(&still_running).remove(&id);
                // A session that has stopped listening is going away; its
                // apps' endings are nobody's business then.
                let _ = events.send(Event {
                    id,
                    app,
                    title,
                    end,
                    said,
                });
                if let Some(wake) = wake {
                    wake();
                }
            })
            .map_err(|error| SpawnError {
                app: planned.app.clone(),
                reason: format!("no thread to watch it: {error}"),
            })?;
        Ok((started, link))
    }

    /// The apps that ended since the last call, oldest first.
    pub fn events(&self) -> Vec<Event> {
        self.events.1.try_iter().collect()
    }

    /// Report `event` as if the app it names had ended: what the session
    /// hears of a launch it did not start itself.
    #[cfg(feature = "test-support")]
    pub fn report(&self, event: Event) {
        let _ = self.events.0.send(event);
    }
}

impl Spawner {
    /// The session is going away, and its apps with it: ask each one still
    /// running to end. Its watcher reaps it, and reports how it ended, as it
    /// would any other. An in-place restart leaves running what the run
    /// that follows takes back ([`App::stays_for`]), untouched.
    pub fn end_running(&self, teardown: Teardown) {
        let apps = std::mem::take(&mut *running(&self.running));
        for app in apps.into_values() {
            #[cfg(target_os = "linux")]
            if let Some(kept) = app.stays_for(teardown) {
                tracing::info!(app = %app.name, pid = app.pid, "kept across restart");
                kept.release();
                continue;
            }
            #[cfg(not(target_os = "linux"))]
            let _ = teardown;
            app.end();
        }
    }
}

impl Drop for Spawner {
    fn drop(&mut self) {
        self.end_running(Teardown::Logout);
        // The watch over the apps' memory ends with the session running them.
        self.memory = None;
    }
}

/// A kept app a run took back after an in-place restart: the launch it is, under the id this
/// spawner reports it by, and the keeper's process its windows name.
#[cfg(target_os = "linux")]
#[derive(Clone, Debug)]
pub struct Adopted {
    pub id: u64,
    /// The keeper's process: the window's process, and the launch's root.
    pub pid: u32,
    pub launch: crate::kept::KeptLaunch,
    /// The app ids its windows are shown with now.
    pub windows: Vec<String>,
}

#[cfg(target_os = "linux")]
impl Spawner {
    /// The keeper `planned` starts behind, when it is kept and the session has keepers: its
    /// directory made and the launcher told to start it there.
    fn kept_launch(
        &self,
        planned: &Planned,
        workspace: &str,
        asked: Option<&str>,
        command: &mut Command,
    ) -> Option<Arc<crate::kept::Kept>> {
        let keeping = self.keeping.as_ref().filter(|_| planned.kept())?;
        let launch = crate::kept::KeptLaunch {
            version: crate::kept::LAUNCH_VERSION,
            app: planned.app.clone(),
            title: planned.title.clone(),
            workspace: workspace.to_owned(),
            asked: asked.map(str::to_owned),
            window_identities: planned.window_identities.clone(),
        };
        match crate::kept::new_dir(&keeping.root, &launch) {
            Ok(dir) => {
                command
                    .arg("--keeper")
                    .arg(&keeping.keeper)
                    .arg("--keep-in")
                    .arg(&dir)
                    .arg("--keep-as")
                    .arg(launch.encode());
                Some(crate::kept::Kept::new(dir))
            }
            Err(error) => {
                tracing::warn!(app = %planned.app, %error, "the app starts without a keeper: it ends if the desktop restarts");
                None
            }
        }
    }

    /// The app `pid` runs, behind `kept` when it is kept: shown on the desktop once its keeper
    /// answers.
    fn app_of(&self, pid: u32, name: &str, kept: Option<&Arc<crate::kept::Kept>>) -> App {
        let mut app = App::of(pid, name);
        if let (Some(kept), Some(keeping)) = (kept, &self.keeping) {
            app.kept = Some(Arc::clone(kept));
            watch_started(Arc::clone(kept), pid, Arc::clone(&keeping.desktop));
        }
        app
    }

    /// Take back the apps the run before an in-place restart left running behind keepers: each
    /// becomes a launch of this spawner, reported and ended as one it started. A keeper whose app
    /// ended while no desktop was there is reported as that end; one at another interface version
    /// is left running untouched; a process that is not the one the keeper started is never taken.
    /// Their windows come back once [`Self::attach_kept`] hands them the desktop.
    pub fn adopt_kept(&mut self) -> Vec<Adopted> {
        let Some(keeping) = self.keeping.clone() else {
            return Vec::new();
        };
        let mut adopted = Vec::new();
        for dir in crate::kept::dirs(&keeping.root) {
            match crate::kept::greet(&dir) {
                Ok(greeted) => {
                    if let Some(taken) = self.adopt_one(&dir, greeted) {
                        adopted.push(taken);
                    }
                }
                Err(crate::kept::Refused::Version(version)) => {
                    tracing::warn!(
                        dir = %dir.display(),
                        version,
                        "not adopted: the kept app runs an older keeper and needs a restart to finish the update"
                    );
                }
                Err(crate::kept::Refused::Gone) => {
                    self.report_gone(&dir, None);
                }
                Err(crate::kept::Refused::Failed(error)) => {
                    tracing::warn!(dir = %dir.display(), %error, "not adopted: the keeper did not answer");
                }
            }
        }
        adopted
    }

    fn adopt_one(&mut self, dir: &Path, greeted: crate::kept::Greeted) -> Option<Adopted> {
        let crate::kept::Greeted {
            control,
            keeper,
            description,
            inbox,
        } = greeted;
        let launch = crate::kept::KeptLaunch::decode(&description.launch)
            .or_else(|| crate::kept::written_launch(dir));
        let Some(launch) = launch else {
            tracing::warn!(dir = %dir.display(), "not adopted: the launch it keeps is not one this session reads");
            return None;
        };
        if let Some(end) = description.ended {
            drop(control);
            self.report_gone(dir, Some(crate::kept::end_of(end)));
            return None;
        }
        if !crate::kept::same_process(&description) {
            tracing::warn!(app = %launch.app, dir = %dir.display(), "not adopted: its process is not the one the keeper started");
            return None;
        }
        let kept = crate::kept::Kept::new(dir.to_path_buf());
        if let Err(error) = kept.greeted(&control, keeper) {
            tracing::warn!(app = %launch.app, %error, "not adopted: its keeper cannot be written to");
            return None;
        }
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        let mut app = App::of(keeper, &launch.app);
        // It opened long ago: it takes the tier of what the person sees at once.
        app.started = Instant::now()
            .checked_sub(launcher::priority::STARTUP_WINDOW)
            .unwrap_or(app.started);
        app.kept = Some(Arc::clone(&kept));
        let cgroup = app.cgroup_dir();
        running(&self.running).insert(id, app);
        tracing::info!(app = %launch.app, keeper, pid = ?description.app_pid, "adopted");
        let windows = description
            .windows
            .iter()
            .filter_map(|window| window.app_id.clone())
            .collect();
        let (events, wake, still_running, ends) = (
            self.events.0.clone(),
            self.wake.clone(),
            Arc::clone(&self.running),
            Arc::clone(&self.ends),
        );
        let (app, title) = (launch.app.clone(), launch.title.clone());
        let spawned = std::thread::Builder::new()
            .name(format!("kept-{app}"))
            .spawn(move || {
                let heard = kept.listen(&control, inbox);
                if kept.released() {
                    return;
                }
                // A keeper gone without a word was killed with its app.
                let end = heard.unwrap_or_else(|| End::signalled(libc::SIGKILL));
                let end = match cgroup.and_then(|dir| memory::take_end(&ends, &dir)) {
                    Some(cause) if end.crashed() => end.remembering(cause),
                    _ => end,
                };
                running(&still_running).remove(&id);
                kept.remove();
                let _ = events.send(Event {
                    id,
                    app,
                    title,
                    end,
                    said: crate::output::OutputTail::default(),
                });
                if let Some(wake) = wake {
                    wake();
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "a kept app's end goes unheard");
        }
        Some(Adopted {
            id,
            pid: keeper,
            launch,
            windows,
        })
    }

    /// Report the kept app in `dir`, which ended while no desktop was there, and remove its
    /// directory.
    fn report_gone(&mut self, dir: &Path, end: Option<End>) {
        let launch = crate::kept::written_launch(dir);
        let end = end.or_else(|| crate::kept::recorded_end(dir));
        if let Err(error) = std::fs::remove_dir_all(dir) {
            tracing::warn!(dir = %dir.display(), %error, "a kept app's directory stays");
        }
        let (Some(launch), Some(end)) = (launch, end) else {
            tracing::info!(dir = %dir.display(), "not adopted: its keeper is gone");
            return;
        };
        tracing::info!(app = %launch.app, %end, "not adopted: it ended while the desktop was away");
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        let _ = self.events.0.send(Event {
            id,
            app: launch.app,
            title: launch.title,
            end,
            said: crate::output::OutputTail::default(),
        });
    }

    /// Show the windows of every kept app this spawner holds and has not shown in this run.
    pub fn attach_kept(&self) {
        let Some(keeping) = &self.keeping else {
            return;
        };
        let kept: Vec<Arc<crate::kept::Kept>> = running(&self.running)
            .values()
            .filter_map(|app| app.kept.clone())
            .collect();
        for kept in kept {
            kept.attach(&keeping.desktop);
        }
    }

    /// End what a run that did not end in a restart left behind keepers, and remove their
    /// directories: a start that follows no in-place restart takes nothing back.
    pub fn clear_kept(&self) {
        let Some(keeping) = &self.keeping else {
            return;
        };
        for dir in crate::kept::dirs(&keeping.root) {
            crate::kept::end_leftover(&dir);
        }
    }
}

/// Wait for the keeper the launcher `launcher` starts for `kept`, greet it and show its app on
/// `desktop`, then drain what it says until it goes: the launch's end is the launcher's to tell.
#[cfg(target_os = "linux")]
fn watch_started(kept: Arc<crate::kept::Kept>, launcher: u32, desktop: crate::kept::Desktop) {
    let Some(pidfd) = crate::kept::pidfd(launcher) else {
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("kept-attach".into())
        .spawn(move || {
            if !crate::kept::wait_for_control(&kept.dir, &pidfd) {
                return;
            }
            match crate::kept::greet(&kept.dir) {
                Ok(greeted) => {
                    if let Err(error) = kept.greeted(&greeted.control, greeted.keeper) {
                        tracing::warn!(%error, "a kept app's keeper cannot be written to");
                        return;
                    }
                    kept.attach(&desktop);
                    let _ = kept.listen(&greeted.control, greeted.inbox);
                }
                Err(error) => {
                    tracing::warn!(dir = %kept.dir.display(), ?error, "a kept app's keeper did not answer");
                }
            }
        });
    if let Err(error) = spawned {
        tracing::warn!(%error, "a kept app is not shown");
    }
}

#[cfg(not(target_os = "linux"))]
impl Spawner {
    /// Only Linux keeps apps across a restart.
    #[expect(clippy::unused_self, reason = "the Linux spawner's signature")]
    fn kept_launch(&self, _: &Planned, _: &str, _: Option<&str>, _: &mut Command) -> Option<()> {
        None
    }

    #[expect(clippy::unused_self, reason = "the Linux spawner's signature")]
    fn app_of(&self, pid: u32, name: &str, _: Option<&()>) -> App {
        App::of(pid, name)
    }
}

/// A started app the spawner can still end: a pidfd of it, where the kernel
/// has them.
pub(crate) struct App {
    /// The launch's process, which the launcher moved into the app's cgroup.
    pub(crate) pid: u32,
    /// The app's package id or desktop-file id.
    pub(crate) name: String,
    /// When this launch started: a launch is served before everything else
    /// while it opens ([`crate::memory`]).
    started: Instant,
    #[cfg(target_os = "linux")]
    pidfd: Option<std::os::fd::OwnedFd>,
    /// The keeper this launch runs behind, when it is kept.
    #[cfg(target_os = "linux")]
    pub(crate) kept: Option<Arc<crate::kept::Kept>>,
}

impl App {
    /// The app the child `pid` runs, which must not have been reaped yet.
    #[cfg(target_os = "linux")]
    fn of(pid: u32, app: &str) -> Self {
        use std::os::fd::FromRawFd as _;
        let app = app.to_owned();
        let Ok(child) = libc::pid_t::try_from(pid) else {
            return Self {
                pid,
                name: app,
                started: Instant::now(),
                pidfd: None,
                kept: None,
            };
        };
        // SAFETY: pidfd_open takes a pid and flags and returns a new
        // descriptor this process owns, or -1.
        let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, child, 0) };
        let pidfd = match i32::try_from(fd) {
            // SAFETY: the kernel just returned this descriptor, and nothing
            // else owns it.
            Ok(fd) if fd >= 0 => Some(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) }),
            _ => {
                tracing::warn!(
                    error = %std::io::Error::last_os_error(),
                    "no pidfd for an app: it can outlive its session"
                );
                None
            }
        };
        Self {
            pid,
            name: app,
            started: Instant::now(),
            pidfd,
            kept: None,
        }
    }

    /// The keeper of a launch `teardown` leaves running: only an in-place restart, and only a launch
    /// whose keeper answered in this run. Everything else ends.
    #[cfg(target_os = "linux")]
    fn stays_for(&self, teardown: Teardown) -> Option<&Arc<crate::kept::Kept>> {
        match teardown {
            Teardown::Logout => None,
            Teardown::InPlaceRestart => self.kept.as_ref().filter(|kept| kept.answered()),
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn of(pid: u32, app: &str) -> Self {
        Self {
            pid,
            name: app.to_owned(),
            started: Instant::now(),
        }
    }

    /// Whether this launch is still opening: inside the window in which it is
    /// served before every other app and the shell
    /// ([`launcher::priority::STARTUP_WINDOW`]).
    pub(crate) fn opening(&self) -> bool {
        self.started.elapsed() < launcher::priority::STARTUP_WINDOW
    }

    /// The cgroup directory the launcher put this app in, while it is there.
    pub(crate) fn cgroup_dir(&self) -> Option<PathBuf> {
        #[cfg(target_os = "linux")]
        {
            let cgroup = std::fs::read_to_string(format!("/proc/{}/cgroup", self.pid)).ok()?;
            let cgroup = launcher::app_cgroup::app_cgroup_at(&cgroup)?;
            Some(launcher::app_cgroup::fs_path(&cgroup))
        }
        #[cfg(not(target_os = "linux"))]
        {
            None
        }
    }

    /// End the app; one that has ended already is left as it is. Its whole
    /// cgroup goes, so a program it started and left behind ends too; a
    /// launcher that never got a scope of its own (no user manager) is only
    /// asked to end.
    fn end(&self) {
        #[cfg(target_os = "linux")]
        if let Some(kept) = &self.kept {
            kept.remove();
        }
        if let Some(dir) = self.cgroup_dir() {
            if let Err(error) = launcher::app_cgroup::kill(&dir) {
                tracing::warn!(app = %self.name, dir = %dir.display(), %error, "the app did not end");
            }
            return;
        }
        #[cfg(target_os = "linux")]
        if let Some(pidfd) = &self.pidfd {
            use std::os::fd::AsRawFd as _;
            // SAFETY: the descriptor is a live pidfd this value owns, and
            // the signal's info may be null.
            let _ = unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    pidfd.as_raw_fd(),
                    libc::SIGTERM,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            };
        }
    }
}

/// The file an app's own diagnostics go to when the session's shell names a directory for them
/// (`TILO_APP_STDERR_DIR`), with the app told where to write its startup timeline.
fn diagnostics_file(planned: &Planned, command: &mut Command) -> Option<std::fs::File> {
    // Where an app's own diagnostics go when the session's shell names a
    // directory for them (`TILO_APP_STDERR_DIR`): a headless acceptance
    // reads them there, and the log keeps them too.
    std::env::var_os("TILO_APP_STDERR_DIR").and_then(|dir| {
        let path = std::path::Path::new(&dir).join(format!("{}.log", planned.app));
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(file) = std::fs::File::create(&path) {
            // The app's own startup timeline, one file per app: which of
            // its steps (loader, fonts, records, first layout) the open
            // was spent in, and the launcher's marks before it. It is
            // written inside the app's own directory, the one place the
            // sandbox lets it write; a measured session reads it from
            // there. An app started any other way writes none.
            let timeline =
                std::path::Path::new(launcher::appdir::SANDBOX_APP_DIR).join("startup.json");
            command.env("TILO_STARTUP_METRICS_PATH", timeline);
            return Some(file);
        }
        None
    })
}

/// How `child` ended: its exit status, or the signal that killed it, with
/// why memory ended it when memory did.
fn ended(mut child: Child, ends: &memory::Ends) -> End {
    // Asked before the child is reaped, when its cgroup can still be read:
    // the launcher moved it there before it started anything.
    let cgroup = app_cgroup(child.id());
    let end = match child.wait() {
        Ok(status) => {
            if let Some(code) = status.code() {
                End::exited(code)
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt as _;
                    match status.signal() {
                        Some(signal) => End::signalled(signal),
                        None => End::failed("it ended without a status"),
                    }
                }
                #[cfg(not(unix))]
                End::failed("it ended without a status")
            }
        }
        Err(error) => End::failed(error.to_string()),
    };
    if !end.crashed() {
        // The app ended as it meant to: memory did not close it.
        return end;
    }
    match cgroup.and_then(|dir| memory::take_end(ends, &dir)) {
        Some(cause) => end.remembering(cause),
        None => end,
    }
}

/// The cgroup of the app `pid` runs in, when the launcher put it in one: the
/// app's own scope.
#[cfg(target_os = "linux")]
fn app_cgroup(pid: u32) -> Option<PathBuf> {
    let cgroup = std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok()?;
    launcher::app_cgroup::app_cgroup_at(&cgroup)
        .map(|cgroup| launcher::app_cgroup::fs_path(&cgroup))
}

#[cfg(not(target_os = "linux"))]
fn app_cgroup(_pid: u32) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    use crate::plan::Kind;

    fn shell(script: &str, env: BTreeMap<String, String>) -> Planned {
        Planned {
            app: "org.example.Probe".into(),
            title: "Probe".into(),
            kind: Kind::System,
            program: PathBuf::from("/bin/sh"),
            args: vec!["-c".into(), script.into()],
            env,
            window_identities: Vec::new(),
        }
    }

    /// Wait for `count` events, failing the test rather than hanging.
    fn events(spawner: &Spawner, count: usize) -> Vec<Event> {
        let started = Instant::now();
        let mut collected = Vec::new();
        while collected.len() < count {
            collected.extend(spawner.events());
            if collected.len() >= count {
                break;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "the app never reported how it ended"
            );
            std::thread::park_timeout(Duration::from_millis(5));
        }
        collected
    }

    tilo_test_env::scenario! {
        fn a_started_app_is_reported_as_it_ended;

        phase an_app_starts_with_exactly_the_environment_it_planned;
        {
            let directory = tempfile::tempdir().expect("a directory");
            let written = directory.path().join("environment");
            let script = format!(
                "printf '%s\\n' \"$WAYLAND_DISPLAY\" \"$TILO_USER_ID\" \"$LEAK\" > {}",
                written.display()
            );
            let planned = shell(
                &script,
                BTreeMap::from([
                    ("WAYLAND_DISPLAY".to_owned(), "wayland-6".to_owned()),
                    ("TILO_USER_ID".to_owned(), "person-4".to_owned()),
                ]),
            );
            let mut spawner = Spawner::default();
            let started = spawner.start(&planned).expect("starts");
            assert_ne!(started.pid, 0);
            let event = events(&spawner, 1).remove(0);
            assert_eq!(event.app, "org.example.Probe");
            assert_eq!(event.title, "Probe");
            assert_eq!(event.end, End::exited(0));
            assert_eq!(event.end.memory(), None, "a clean exit is nobody's doing");
            assert!(!event.end.crashed(), "a clean exit is not a crash");
            assert_eq!(
                std::fs::read_to_string(&written).expect("the probe's environment"),
                "wayland-6\nperson-4\n\n",
                "the planned variables arrive and nothing else in the process's environment does"
            );
        }

        phase an_app_that_fails_or_is_killed_is_reported_as_a_crash;
        {
            let mut spawner = Spawner::default();
            let failing = spawner
                .start(&shell("exit 3", BTreeMap::new()))
                .expect("starts");
            let killed = spawner
                .start(&shell("kill -9 $$", BTreeMap::new()))
                .expect("starts");
            assert_ne!(failing.id, killed.id, "each launch is reported on its own");
            let mut events = events(&spawner, 2);
            events.sort_by_key(|event| event.id);
            assert_eq!(events[0].end, End::exited(3));
            assert!(events[0].end.crashed());
            assert_eq!(events[1].end, End::signalled(9));
            assert!(events[1].end.crashed());
            assert_eq!(events[1].end.to_string(), "was killed by signal 9");
            assert!(
                events.iter().all(|event| event.end.memory().is_none()),
                "a launcher that keeps no app tree remembers nothing"
            );
        }

        phase an_app_that_cannot_start_says_so_instead_of_starting_something_else;
        {
            let mut spawner = Spawner::default();
            let missing = Planned {
                program: PathBuf::from("/nonexistent/tilo-app"),
                ..shell("true", BTreeMap::new())
            };
            let error = spawner.start(&missing).expect_err("nothing starts");
            assert_eq!(error.app, "org.example.Probe");
            assert!(error.reason.contains("No such file"), "{error}");
            assert!(
                spawner.events().is_empty(),
                "no process, no ending to report"
            );
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn an_app_still_running_ends_with_the_spawner_that_started_it() {
        let mut spawner = Spawner::default();
        let running = spawner
            .start(&shell("exec sleep infinity", BTreeMap::new()))
            .expect("starts");
        let ended = spawner
            .start(&shell("exit 0", BTreeMap::new()))
            .expect("starts");
        // The one that ended is reaped and forgotten: the drop signals only
        // what is still running.
        assert_eq!(events(&spawner, 1)[0].id, ended.id);

        drop(spawner);
        tilo_test_env::exited("the app its session no longer runs", running.pid);
    }
}
