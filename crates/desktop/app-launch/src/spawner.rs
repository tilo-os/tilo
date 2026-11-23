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
#[cfg(target_os = "linux")]
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::output::AppOutput;

use tilo_lock::MutexRecover as _;

use crate::links::HeldLink;
use crate::memory;
use crate::plan::{Kind, Planned};

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
    /// Its keeper stopped without a word (killed, or crashed), and the app ended with it.
    KeeperStopped,
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
            Self::Failed(_) | Self::KeeperStopped => {}
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
            Self::Failed(_) | Self::KeeperStopped => None,
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
            Self::KeeperStopped => write!(f, "closed because its keeper stopped"),
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
    /// Its keeper was killed while no desktop was there, taking the app with it: the app's saved
    /// tab says it stopped working rather than a notice opening here.
    pub killed: bool,
    /// The session asked this app to end, as the person's own Quit does: the end is theirs, so no
    /// notice says it stopped working.
    pub asked_to_end: bool,
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
    /// What notes the kernel's OOM kills in the apps' cgroups as they happen.
    kills: Option<memory::KernelKills>,
    /// What starts an app behind a keeper, once the session has a desktop to attach keepers to.
    #[cfg(target_os = "linux")]
    keeping: Option<crate::kept::Keeping>,
    /// The watch over the keepers' registry, and whether it saw a change not looked at yet.
    #[cfg(target_os = "linux")]
    registry: Option<(
        tilo_files::native::ChangeWatch,
        Arc<std::sync::atomic::AtomicBool>,
    )>,
    /// Keeper directories made for launches that have not started yet ([`KeptDirs`]).
    #[cfg(target_os = "linux")]
    reserved: Reserved,
    /// The launch each kept app that ended while no desktop was there was, by the id its end is
    /// reported under, until [`Self::ended_away`] takes it.
    #[cfg(target_os = "linux")]
    away: BTreeMap<u64, tilo_app_keeper::registry::KeptLaunch>,
    /// The launch each kept app whose keeper was killed outright while no desktop was there was:
    /// the app never ended, so its saved tab stays to say it stopped working
    /// ([`Self::stopped_away`]).
    #[cfg(target_os = "linux")]
    stopped: BTreeMap<u64, tilo_app_keeper::registry::KeptLaunch>,
}

#[cfg(target_os = "linux")]
type Reserved = Arc<Mutex<std::collections::BTreeSet<PathBuf>>>;

/// Makes keeper directories for launches whose display must be known before they start (a
/// sandbox binds the keeper's socket), on a thread other than the spawner's: a directory made
/// is the spawner's own, never a keeper it adopts, until [`Spawner::start_handed`] starts its
/// launch or [`Self::give_back`] removes it.
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct KeptDirs {
    root: PathBuf,
    reserved: Reserved,
}

#[cfg(target_os = "linux")]
impl KeptDirs {
    /// A new keeper directory.
    ///
    /// # Errors
    /// The directory could not be made.
    pub fn reserve(&self) -> std::io::Result<PathBuf> {
        let dir = tilo_app_keeper::registry::make_dir(&self.root)?;
        self.reserved.lock_recover().insert(dir.clone());
        Ok(dir)
    }

    /// Remove `dir`, whose launch did not start.
    pub fn give_back(&self, dir: &Path) {
        if self.reserved.lock_recover().remove(dir)
            && let Err(error) = std::fs::remove_dir_all(dir)
        {
            tracing::warn!(dir = %dir.display(), %error, "an unused keeper directory stays");
        }
    }
}

/// What a launch through [`Spawner::start_handed`] hands the launcher beyond its plan: a launch
/// started behind a keeper whose directory was made before it ([`KeptDirs`]), with the command the
/// launcher starts behind the keeper and the descriptors that command inherits.
#[cfg(target_os = "linux")]
pub struct Handed {
    /// The keeper's directory, from [`KeptDirs::reserve`].
    pub dir: PathBuf,
    /// The command the keeper starts, after the launcher's own arguments and `--`.
    pub command: Vec<std::ffi::OsString>,
    /// Each descriptor with the number the command finds it at.
    pub inherited: Vec<(std::os::fd::OwnedFd, i32)>,
    /// The launch starts with the game performance hints.
    pub latency_sensitive: bool,
}

/// A kept launch whose filtered network a desktop serves: the control socket a sandboxed
/// program's proxy answers on, or the file a Windows appliance presentation keeps for the
/// desktop that adopts it, and the app and workspace whose grants decide what it reaches.
#[cfg(target_os = "linux")]
pub struct KeptNetwork {
    /// The launch's keeper directory, where its network lives.
    pub dir: PathBuf,
    /// The control socket of a sandboxed program's proxy, when its launch serves one.
    pub control: Option<PathBuf>,
    /// The launch keeps a Windows appliance presentation's egress: its listener lived in the
    /// desktop that started it, and a desktop that adopts it serves it again.
    pub appliance: bool,
    /// This session took the launch back after a restart.
    pub adopted: bool,
    pub app: String,
    pub workspace: String,
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
        let kills = memory::KernelKills::spawn(Arc::clone(&ends))
            .map_err(|error| tracing::warn!(%error, "the kernel's OOM kills go unnoted"))
            .ok();
        Self {
            wake,
            events: mpsc::channel(),
            next: 0,
            running,
            ends,
            memory,
            kills,
            #[cfg(target_os = "linux")]
            keeping: None,
            #[cfg(target_os = "linux")]
            registry: None,
            #[cfg(target_os = "linux")]
            reserved: Reserved::default(),
            #[cfg(target_os = "linux")]
            away: BTreeMap::new(),
            #[cfg(target_os = "linux")]
            stopped: BTreeMap::new(),
        }
    }

    /// The X11 display of each kept launch that serves one, with the cookie file that admits a
    /// client, in display order: what the session's input method serves as XIM.
    #[cfg(target_os = "linux")]
    pub fn kept_x11(&self) -> Vec<(String, PathBuf)> {
        let mut displays: Vec<(String, PathBuf)> = running(&self.running)
            .values()
            .filter_map(|app| app.kept.as_ref().and_then(|kept| kept.x11()))
            .collect();
        displays.sort();
        displays
    }

    /// The network of each kept launch that has one, with the app and the workspace whose grants
    /// decide the policy it enforces.
    #[cfg(target_os = "linux")]
    pub fn kept_networks(&self) -> Vec<KeptNetwork> {
        running(&self.running)
            .values()
            .filter_map(|app| {
                let kept = app.kept.as_ref()?;
                let control = kept.network_control();
                let appliance = kept
                    .dir
                    .join(launcher::egress_proxy::APPLIANCE_NETWORK)
                    .is_file();
                if control.is_none() && !appliance {
                    return None;
                }
                let launch = crate::kept::written_launch(&kept.dir)?;
                Some(KeptNetwork {
                    dir: kept.dir.clone(),
                    control,
                    appliance,
                    adopted: kept.adopted(),
                    app: launch.app,
                    workspace: launch.workspace,
                })
            })
            .collect()
    }

    /// Start the apps that are not Tilo's behind `keeping`'s keeper from now on, attached to its
    /// desktop ([`crate::kept`]).
    ///
    /// The registry is watched from now on: a keeper that appears there later (a service's) is
    /// taken up by [`Self::adopt_appeared`], and this spawner's loop woken for it.
    #[cfg(target_os = "linux")]
    pub fn keep_behind(&mut self, keeping: crate::kept::Keeping) {
        let changed = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let seen = Arc::clone(&changed);
        let wake = self.wake.clone();
        let watched = std::fs::create_dir_all(&keeping.root).and_then(|()| {
            tilo_files::native::watch_tree_changes(
                &keeping.root,
                |_| true,
                move || {
                    seen.store(true, std::sync::atomic::Ordering::Release);
                    if let Some(wake) = &wake {
                        wake();
                    }
                },
            )
        });
        match watched {
            Ok(watch) => self.registry = Some((watch, changed)),
            Err(error) => {
                tracing::warn!(%error, "the keepers' registry is not watched: a keeper a service starts later is not shown");
            }
        }
        self.keeping = Some(keeping);
    }

    /// Put every cgroup a launch of this session runs in in its tier:
    /// `on_screen` is the process of each window on an output, `own` the
    /// cgroup this session itself runs in. A launch that is still opening has
    /// the startup tier; an app with no window on screen is in the background
    /// tier and gives memory back first; one whose window the person sees
    /// again is in the foreground tier. `own` keeps the session's tier.
    pub fn tier_by_visibility(&self, on_screen: &[u32], own: Option<&Path>) {
        memory::tier_by_visibility(&self.running, on_screen, own, self.kills.as_ref());
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
    /// The launcher waits at a launch hold until `write_down` has written
    /// the launch down: an app that connected before its launch was known
    /// would be refused by the session's services. A launch without a link
    /// gets no hold, and its command line is its stdin.
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
        link: Option<HeldLink>,
        write_down: &dyn Fn(u32),
        workspace: &str,
        asked: Option<&str>,
    ) -> Result<(Started, Option<HeldLink>), SpawnError> {
        self.start_launch(planned, link, write_down, workspace, asked, None)
    }

    /// [`Self::start_with`] for a kept launch whose keeper directory was made before it, handing
    /// the launcher `handed`'s command and descriptors.
    ///
    /// # Errors
    /// The program could not be started, or the session keeps no apps.
    #[cfg(target_os = "linux")]
    pub fn start_handed(
        &mut self,
        planned: &Planned,
        write_down: &dyn Fn(u32),
        workspace: &str,
        asked: Option<&str>,
        handed: Handed,
    ) -> Result<Started, SpawnError> {
        self.start_launch(planned, None, write_down, workspace, asked, Some(handed))
            .map(|(started, _)| started)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one ordered launch: the command, its hold, its keeper and what it writes, each set before the process starts"
    )]
    fn start_launch(
        &mut self,
        planned: &Planned,
        mut link: Option<HeldLink>,
        write_down: &dyn Fn(u32),
        workspace: &str,
        asked: Option<&str>,
        #[cfg(target_os = "linux")] handed: Option<Handed>,
        #[cfg(not(target_os = "linux"))] handed: Option<()>,
    ) -> Result<(Started, Option<HeldLink>), SpawnError> {
        crate::name_surface(planned);
        let mut command = Command::new(&planned.program);
        // A package's launch that hands nothing down marks the session's descriptors
        // close-on-exec here, just before it starts, and adds no step to the child: a command
        // without one starts with posix_spawn, which shares the session's memory until the exec,
        // rather than a fork that copies the page tables of the whole session first (tens of
        // milliseconds of every open, more on a busy machine). A descriptor some other thread
        // opens between the two without close-on-exec reaches only the launcher, whose sandbox
        // shim closes every descriptor it is not handed before the app runs
        // (`launcher::landlock_shim`). An app the launcher does not confine, and a launch that
        // hands descriptors down, mark them in the child, after the fork.
        let marked_here = planned.kind == Kind::Package && handed.is_none();
        if !marked_here {
            close_inherited(&mut command);
        }
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
        let diagnostics = diagnostics_file(planned);
        measure_startup(&mut command);
        let output = Arc::new(AppOutput::new(planned.app.clone(), diagnostics));
        let hold = match link {
            Some(_) => match launcher::launch_hold::install(&mut command) {
                Ok(hold) => Some(hold),
                // Without its hold the launch would run unreleased: it starts
                // nothing, as an app that cannot start does.
                Err(error) => {
                    return Err(SpawnError {
                        app: planned.app.clone(),
                        reason: format!("no launch hold: {error}"),
                    });
                }
            },
            None => None,
        };
        let (kept, handed_down) =
            self.kept_launch(planned, workspace, asked, &mut command, handed)?;
        // A kept launch's keeper writes the same bytes it passes on here: what this desktop reads is
        // kept beside them, and the desktop that follows reads on rather than logging it again.
        #[cfg(target_os = "linux")]
        if let Some(kept) = &kept {
            output.keep_position_in(&kept.dir);
            // What the launcher said on its stderr for a launch it did not keep: a kept launch's
            // streams carry its app's output alone, so the log is told here instead.
            tracing::info!(app = %planned.app, "a machine app is unconfined, kept across a desktop restart");
        }
        if marked_here && let Err(error) = mark_inherited_close_on_exec() {
            self.abandon(kept.as_ref());
            return Err(SpawnError {
                app: planned.app.clone(),
                reason: format!("the session's descriptors could not be kept from it: {error}"),
            });
        }
        let spawned = command.spawn();
        drop((command, handed_down));
        let mut child = match spawned {
            Ok(child) => child,
            Err(error) => {
                self.abandon(kept.as_ref());
                return Err(SpawnError {
                    app: planned.app.clone(),
                    reason: error.to_string(),
                });
            }
        };
        // Both pipes are read on threads of their own, so an app that writes
        // more than a pipe holds never waits for the session, and a session
        // that has stopped reading never holds the app.
        if let Some(stdout) = child.stdout.take() {
            output.keep_stdout(stdout);
        }
        if let Some(stderr) = child.stderr.take() {
            output.keep_stderr(stderr);
        }
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        let started = Started {
            id,
            pid: child.id(),
        };
        // Taken before the watcher can reap the child, so it names this
        // process and no later one with its pid.
        let (app_of, heard) = self.app_of(started.pid, &planned.app, kept.as_ref());
        let asked_to_end = Arc::clone(&app_of.asked_to_end);
        running(&self.running).insert(id, app_of);
        write_down(started.pid);
        if let Some(hold) = hold {
            // The launch is written down: the app may connect.
            let served = link.as_mut().and_then(|link| link.serve(started.pid));
            let released = match &served {
                Some(theirs) => hold.release_with_window_link(theirs.as_fd()),
                None => hold.release(),
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
                let Some(end) = kept_end(kept.as_deref(), heard, end) else {
                    return;
                };
                #[cfg(not(target_os = "linux"))]
                drop(heard);
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
                    killed: false,
                    asked_to_end: asked_to_end.load(Ordering::Acquire),
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
    /// that follows takes back ([`App::stays_for`]), untouched. Every keeper
    /// is asked before any app is ended, so what one app's end does to the
    /// machine does not decide another's.
    pub fn end_running(&self, teardown: Teardown) {
        let apps = std::mem::take(&mut *running(&self.running));
        let mut decisions = Vec::with_capacity(apps.len());
        for app in apps.into_values() {
            #[cfg(target_os = "linux")]
            let stays = app.stays_for(teardown).is_some();
            #[cfg(not(target_os = "linux"))]
            let stays = false;
            decisions.push((app, stays));
        }
        for (app, stays) in decisions {
            #[cfg(target_os = "linux")]
            if stays {
                let Some(kept) = &app.kept else {
                    continue;
                };
                tracing::info!(target: tilo_startup_metrics::KEPT_APPS_TARGET, app = %app.name, pid = app.pid, "kept across restart");
                kept.release();
                continue;
            }
            #[cfg(not(target_os = "linux"))]
            let _ = stays;
            #[cfg(target_os = "linux")]
            if teardown == Teardown::InPlaceRestart && app.kept.is_some() {
                tracing::info!(target: tilo_startup_metrics::KEPT_APPS_TARGET, app = %app.name, pid = app.pid, "not kept across restart: its keeper is gone");
            }
            #[cfg(not(target_os = "linux"))]
            let _ = teardown;
            // The session ends: nothing of it is left to clear a keeper's files later.
            app.end(true);
        }
    }
}

impl Drop for Spawner {
    fn drop(&mut self) {
        self.end_running(Teardown::Logout);
        // The watch over the apps' memory ends with the session running them.
        self.memory = None;
        self.kills = None;
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
    pub launch: tilo_app_keeper::registry::KeptLaunch,
    /// Its windows, oldest first, each with the place the run before last told its keeper.
    pub windows: Vec<crate::kept::Window>,
    /// Its keeper is of a build before an update that changed how the session speaks to it: the
    /// app goes on with unsaved work, and this session can show none of it, only end it
    /// ([`Spawner::restart_outdated`]). Its windows are not known.
    pub outdated: bool,
}

#[cfg(target_os = "linux")]
impl Spawner {
    /// The keeper `planned` starts behind, when it is kept and the session has keepers: its
    /// directory made and the launcher told to start it there.
    ///
    /// A `handed` launch is started in the directory made for it, with its command after the
    /// launcher's arguments and its descriptors handed down; what this returns beside the keeper
    /// holds those descriptors until the launcher is started. It is refused without a keeper.
    fn kept_launch(
        &self,
        planned: &Planned,
        workspace: &str,
        asked: Option<&str>,
        command: &mut Command,
        handed: Option<Handed>,
    ) -> Result<(Option<Arc<crate::kept::Kept>>, Vec<std::os::fd::OwnedFd>), SpawnError> {
        let refused = |reason: String| SpawnError {
            app: planned.app.clone(),
            reason,
        };
        let Some(keeping) = self.keeping.as_ref().filter(|_| planned.kept()) else {
            return match handed {
                Some(handed) => {
                    self.kept_dirs_unchecked().give_back(&handed.dir);
                    Err(refused("this session keeps no apps".to_owned()))
                }
                None => Ok((None, Vec::new())),
            };
        };
        let launch = tilo_app_keeper::registry::KeptLaunch {
            version: tilo_app_keeper::registry::LAUNCH_VERSION,
            app: planned.app.clone(),
            title: planned.title.clone(),
            workspace: workspace.to_owned(),
            asked: asked.map(str::to_owned),
            window_identities: planned.window_identities.clone(),
            service: false,
        };
        let Some(handed) = handed else {
            return match tilo_app_keeper::registry::new_dir(&keeping.root, &launch) {
                Ok(dir) => {
                    keep_in(command, &keeping.keeper, &dir, &launch);
                    Ok((Some(crate::kept::Kept::new(dir, false)), Vec::new()))
                }
                Err(error) => {
                    tracing::warn!(app = %planned.app, %error, "the app starts without a keeper: it ends if the desktop restarts");
                    Ok((None, Vec::new()))
                }
            };
        };
        if let Err(error) = tilo_app_keeper::registry::write_launch(&handed.dir, &launch) {
            self.kept_dirs_unchecked().give_back(&handed.dir);
            return Err(refused(format!("its keeper's directory: {error}")));
        }
        keep_in(command, &keeping.keeper, &handed.dir, &launch);
        command.arg("--").args(&handed.command);
        let handed_down: Vec<_> = handed
            .inherited
            .iter()
            .map(|(fd, target)| (std::os::fd::AsRawFd::as_raw_fd(fd), *target))
            .collect();
        launcher::inherit::hand_down(command, handed_down, handed.latency_sensitive);
        Ok((
            Some(crate::kept::Kept::new(handed.dir, false)),
            handed.inherited.into_iter().map(|(fd, _)| fd).collect(),
        ))
    }

    /// The launch whose keeper is `kept` did not start: a directory made for it is given back.
    fn abandon(&self, kept: Option<&Arc<crate::kept::Kept>>) {
        if let Some(kept) = kept {
            self.kept_dirs_unchecked().give_back(&kept.dir);
        }
    }

    /// What makes keeper directories for launches started later with [`Self::start_handed`]:
    /// none while the session keeps no apps.
    #[must_use]
    pub fn kept_dirs(&self) -> Option<KeptDirs> {
        self.keeping.as_ref().map(|_| self.kept_dirs_unchecked())
    }

    fn kept_dirs_unchecked(&self) -> KeptDirs {
        KeptDirs {
            root: self
                .keeping
                .as_ref()
                .map_or_else(tilo_app_keeper::registry::root, |keeping| {
                    keeping.root.clone()
                }),
            reserved: Arc::clone(&self.reserved),
        }
    }

    /// End the launch `id` as a logout would; its end is reported as any other.
    pub fn end_one(&self, id: u64) {
        if let Some(app) = running(&self.running).get(&id) {
            app.end(false);
        }
    }

    /// End the launch whose app runs as the process `pid`, as the person's own quit does: the end
    /// is theirs, so no notice says it stopped working. Whether a launch of this session runs it:
    /// a sandboxed app is a child of the launcher this session started, so the launch is found up
    /// the process tree, the nearest one first.
    pub fn end_pid(&self, pid: u32) -> bool {
        let launched: Vec<(u64, u32)> = running(&self.running)
            .iter()
            .map(|(id, app)| (*id, app.pid))
            .collect();
        let found = crate::links::ancestry(pid).into_iter().find_map(|process| {
            launched
                .iter()
                .find(|(_, launch)| *launch == process)
                .map(|(id, _)| *id)
        });
        let Some(id) = found else {
            return false;
        };
        self.end_one(id);
        true
    }

    /// The launch of the kept app `id`, which ended while no desktop was there: one that ended by
    /// itself or one whose keeper was killed outright. Taken once.
    pub fn ended_away(&mut self, id: u64) -> Option<tilo_app_keeper::registry::KeptLaunch> {
        self.away.remove(&id).or_else(|| self.stopped.remove(&id))
    }

    /// The launches of the kept apps that ended while no desktop was there and whose ends are
    /// not taken yet: their saved tabs are windows those apps closed, not ones to open again.
    #[must_use]
    pub fn away(&self) -> Vec<tilo_app_keeper::registry::KeptLaunch> {
        self.away.values().cloned().collect()
    }

    /// The launches of the kept apps whose keeper was killed outright while no desktop was
    /// there, and whose tabs are not put to saying so yet: they lost what they held without
    /// ending, so their tab says they stopped working and offers to reopen them.
    #[must_use]
    pub fn stopped_away(&self) -> Vec<tilo_app_keeper::registry::KeptLaunch> {
        self.stopped.values().cloned().collect()
    }

    /// The app `pid` runs, behind `kept` when it is kept: shown on the desktop once its keeper
    /// answers, and with what tells how the keeper said its app ended.
    fn app_of(
        &self,
        pid: u32,
        name: &str,
        kept: Option<&Arc<crate::kept::Kept>>,
    ) -> (App, Option<Heard>) {
        let mut app = App::of(pid, name);
        if let Some(kept) = kept {
            // Its launch started: the directory is a keeper's from now on, not one held for later.
            self.reserved.lock_recover().remove(&kept.dir);
        }
        let mut heard = None;
        if let (Some(kept), Some(keeping)) = (kept, &self.keeping) {
            app.kept = Some(Arc::clone(kept));
            heard = watch_started(Arc::clone(kept), pid, Arc::clone(&keeping.desktop));
        }
        (app, heard)
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
            match crate::kept::greet(&dir, crate::kept::ANSWER) {
                Ok(greeted) => {
                    if let Some(taken) = self.adopt_one(&dir, greeted) {
                        adopted.push(taken);
                    }
                }
                Err(crate::kept::Refused::Version(outdated)) => {
                    if let Some(taken) = self.adopt_outdated(&dir, outdated) {
                        adopted.push(taken);
                    }
                }
                Err(crate::kept::Refused::Gone) => {
                    if !crate::kept::written_launch(&dir).is_some_and(|launch| launch.service) {
                        self.report_gone(&dir, None);
                    } else if dir.join("control").exists() {
                        // A service's keeper that listened once and is gone (the service was
                        // stopped with it) leaves only its directory; one that does not listen yet
                        // is taken up once it does.
                        crate::kept::Kept::new(dir, false).remove();
                    }
                }
                Err(crate::kept::Refused::Failed(error)) => {
                    tracing::warn!(target: tilo_startup_metrics::KEPT_APPS_TARGET, dir = %dir.display(), %error, "not adopted: the keeper did not answer");
                }
            }
        }
        adopted
    }

    /// Take up the keepers that appeared in the registry since the last look, and show them at
    /// once: a service's keeper started while this run goes on. Keepers of this spawner's own
    /// launches, and ones that already answered or were refused, are passed over.
    pub fn adopt_appeared(&mut self) -> Vec<Adopted> {
        let Some(keeping) = self.keeping.clone() else {
            return Vec::new();
        };
        let changed = self
            .registry
            .as_ref()
            .is_some_and(|(_, changed)| changed.swap(false, std::sync::atomic::Ordering::AcqRel));
        if !changed {
            return Vec::new();
        }
        let mut known: Vec<PathBuf> = running(&self.running)
            .values()
            .filter_map(|app| app.kept.as_ref().map(|kept| kept.dir.clone()))
            .collect();
        known.extend(self.reserved.lock_recover().iter().cloned());
        let mut adopted = Vec::new();
        for dir in crate::kept::dirs(&keeping.root) {
            if known.contains(&dir)
                || !crate::kept::written_launch(&dir).is_some_and(|launch| launch.service)
            {
                continue;
            }
            // A keeper that does not listen yet is looked at again when its socket appears.
            let Ok(greeted) = crate::kept::greet(&dir, crate::kept::ANSWER) else {
                continue;
            };
            if let Some(taken) = self.adopt_one(&dir, greeted) {
                let kept = running(&self.running)
                    .get(&taken.id)
                    .and_then(|app| app.kept.clone());
                if let Some(kept) = kept {
                    kept.attach(&keeping.desktop);
                }
                adopted.push(taken);
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
        let launch = tilo_app_keeper::registry::KeptLaunch::decode(&description.launch)
            .or_else(|| crate::kept::written_launch(dir));
        let Some(launch) = launch else {
            tracing::warn!(target: tilo_startup_metrics::KEPT_APPS_TARGET, dir = %dir.display(), "not adopted: the launch it keeps is not one this session reads");
            return None;
        };
        if let Some(end) = description.ended {
            drop(control);
            if !launch.service {
                self.report_gone(dir, Some(crate::kept::end_of(end)));
            }
            return None;
        }
        // A keeper still waiting for its first desktop has started nothing to mistake.
        if description.app_pid.is_some() && !crate::kept::same_process(&description) {
            tracing::warn!(target: tilo_startup_metrics::KEPT_APPS_TARGET, app = %launch.app, dir = %dir.display(), "not adopted: its process is not the one the keeper started");
            return None;
        }
        let kept = crate::kept::Kept::new(dir.to_path_buf(), launch.service);
        kept.take_back();
        if let Err(error) = kept.greeted(&control, keeper) {
            tracing::warn!(target: tilo_startup_metrics::KEPT_APPS_TARGET, app = %launch.app, %error, "not adopted: its keeper cannot be written to");
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
        let asked_to_end = Arc::clone(&app.asked_to_end);
        running(&self.running).insert(id, app);
        // What the app wrote reached the desktop that started it; from here it is read from the
        // keeper's bounded file, from its start, and followed while the app has windows.
        let output = Arc::new(AppOutput::new(launch.app.clone(), None));
        output.follow(dir);
        tracing::info!(target: tilo_startup_metrics::KEPT_APPS_TARGET, app = %launch.app, keeper, pid = ?description.app_pid, "adopted; what it wrote is read from its keeper's directory");
        let windows = description.windows;
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
                // The keeper has said how the app ended: what it wrote last is in the file it
                // kept, and the readers stop once they have read it.
                output.finish();
                let said = output.tail();
                if kept.released() {
                    return;
                }
                if kept.service {
                    // The service restarts what it runs; nothing ended for the person.
                    running(&still_running).remove(&id);
                    return;
                }
                // A keeper gone without a word took its app with it: the signal that ended the
                // app was the keeper's doing, not the app's.
                let end = heard.unwrap_or(End::KeeperStopped);
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
                    killed: false,
                    asked_to_end: asked_to_end.load(Ordering::Acquire),
                    said,
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
            outdated: false,
        })
    }

    /// Take up the kept app in `dir` whose keeper speaks another version: a launch of this session
    /// like any other for tiers, the memory watch and the logout that ends it, which shows nothing
    /// and is reported once its app ended. Its keeper holds the app for this session while it is
    /// connected, when it is of a build that does.
    fn adopt_outdated(&mut self, dir: &Path, outdated: crate::kept::Outdated) -> Option<Adopted> {
        let Some(launch) = crate::kept::written_launch(dir) else {
            tracing::warn!(target: tilo_startup_metrics::KEPT_APPS_TARGET, dir = %dir.display(), version = outdated.version, "not adopted: an outdated keeper's launch is not one this session reads");
            return None;
        };
        let keeper = outdated.keeper;
        let Some(process) = crate::kept::kept_app(dir, keeper) else {
            // A service's keeper is the service's to clear away.
            if !launch.service {
                drop(outdated);
                self.report_gone(dir, None);
            }
            return None;
        };
        let ended = crate::kept::pidfd(process.pid)?;
        tracing::warn!(
            target: tilo_startup_metrics::KEPT_APPS_TARGET,
            app = %launch.app,
            version = outdated.version,
            holding = outdated.holding.is_some(),
            "a kept app is behind a keeper from before the update: it shows only once restarted"
        );
        let kept = crate::kept::Kept::outdated(dir.to_path_buf(), launch.service, outdated);
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        let mut app = App::of(keeper, &launch.app);
        app.started = Instant::now()
            .checked_sub(launcher::priority::STARTUP_WINDOW)
            .unwrap_or(app.started);
        app.kept = Some(Arc::clone(&kept));
        let asked_to_end = Arc::clone(&app.asked_to_end);
        running(&self.running).insert(id, app);
        let (events, wake, still_running) = (
            self.events.0.clone(),
            self.wake.clone(),
            Arc::clone(&self.running),
        );
        let (name, title) = (launch.app.clone(), launch.title.clone());
        let spawned = std::thread::Builder::new()
            .name(format!("outdated-{name}"))
            .spawn(move || {
                while !crate::kept::ended_within(&ended, std::time::Duration::from_secs(3600)) {}
                if kept.released() {
                    return;
                }
                running(&still_running).remove(&id);
                let end =
                    crate::kept::recorded_end(&kept.dir).unwrap_or(End::signalled(libc::SIGTERM));
                kept.remove();
                let _ = events.send(Event {
                    id,
                    app: name,
                    title,
                    end,
                    killed: false,
                    asked_to_end: asked_to_end.load(Ordering::Acquire),
                    said: crate::output::OutputTail::default(),
                });
                if let Some(wake) = wake {
                    wake();
                }
            });
        if let Err(error) = spawned {
            tracing::warn!(%error, "an outdated kept app's end goes unheard");
        }
        Some(Adopted {
            id,
            pid: keeper,
            launch,
            windows: Vec::new(),
            outdated: true,
        })
    }

    /// End the outdated kept app `id` ([`Adopted::outdated`]) so it can start again on this
    /// desktop: asked to end, then its launch's scope ended, away from the caller's thread. Its
    /// end is reported as any launch's. False when there is no such launch.
    pub fn restart_outdated(&self, id: u64) -> bool {
        let kept = running(&self.running)
            .get(&id)
            .and_then(|app| app.kept.clone())
            .filter(|kept| kept.is_outdated());
        let Some(kept) = kept else {
            return false;
        };
        let spawned = std::thread::Builder::new()
            .name("outdated-restart".into())
            .spawn(move || kept.end_outdated());
        spawned.is_ok()
    }

    /// Report the kept app in `dir`, which ended while no desktop was there, and remove its
    /// directory. A keeper that left no word of how its app ended was killed outright and took
    /// its app with it: the app never ended, so its tab stays and says it stopped working.
    fn report_gone(&mut self, dir: &Path, end: Option<End>) {
        let launch = crate::kept::written_launch(dir);
        let heard = end.or_else(|| crate::kept::recorded_end(dir));
        let killed = heard.is_none();
        let end = heard.unwrap_or(End::KeeperStopped);
        // What it wrote while no desktop read it is in its keeper's files, which go with the
        // directory: its end carries their tail, as one this desktop saw would.
        let said = launch
            .as_ref()
            .map_or_else(crate::output::OutputTail::default, |launch| {
                let output = Arc::new(AppOutput::new(launch.app.clone(), None));
                output.follow_whole(dir);
                output.finish();
                output.tail()
            });
        if let Err(error) = crate::kept::remove_dir(dir, None, crate::kept::Await::Aside) {
            tracing::warn!(dir = %dir.display(), %error, "a kept app's directory stays");
        }
        let Some(launch) = launch else {
            tracing::info!(target: tilo_startup_metrics::KEPT_APPS_TARGET, dir = %dir.display(), "not adopted: its keeper is gone");
            return;
        };
        let id = self.next;
        self.next = self.next.wrapping_add(1);
        if killed {
            tracing::warn!(target: tilo_startup_metrics::KEPT_APPS_TARGET, app = %launch.app, "not adopted: its keeper was killed while the desktop was away");
            self.stopped.insert(id, launch.clone());
        } else {
            tracing::info!(target: tilo_startup_metrics::KEPT_APPS_TARGET, app = %launch.app, %end, "not adopted: it ended while the desktop was away");
            self.away.insert(id, launch.clone());
        }
        let _ = self.events.0.send(Event {
            id,
            app: launch.app,
            title: launch.title,
            end,
            killed,
            asked_to_end: false,
            said,
        });
    }

    /// Show the windows of the kept app whose keeper is `keeper`, unless they are shown in this
    /// run already.
    pub fn attach_kept(&self, keeper: u32) {
        let Some(keeping) = &self.keeping else {
            return;
        };
        let kept = running(&self.running)
            .values()
            .filter_map(|app| app.kept.clone())
            .find(|kept| kept.keeper() == keeper);
        if let Some(kept) = kept {
            kept.attach(&keeping.desktop);
        }
    }

    /// Tell the keeper `keeper` that its window `tag` stands at `placement` now, unless that is
    /// what it was last told: what the run after an in-place restart reads back.
    pub fn place_kept(&self, keeper: u32, tag: &str, placement: &str) {
        let kept = running(&self.running)
            .values()
            .filter_map(|app| app.kept.clone())
            .find(|kept| kept.keeper() == keeper);
        if let Some(kept) = kept {
            kept.place(tag, placement);
        }
    }

    /// End what a run that did not end in a restart left behind keepers, and remove their
    /// directories: a start that follows no in-place restart takes nothing back but the keepers
    /// of services, which [`Self::adopt_kept`] takes up after.
    pub fn clear_kept(&self) {
        let Some(keeping) = &self.keeping else {
            return;
        };
        for dir in crate::kept::dirs(&keeping.root) {
            if crate::kept::written_launch(&dir).is_some_and(|launch| launch.service) {
                continue;
            }
            crate::kept::end_leftover(&dir);
        }
    }
}

/// Mark every descriptor this session holds beyond its standard streams close-on-exec in the
/// process `command` starts, before it becomes the app: a launch gets only the descriptors it is
/// deliberately handed (`launcher::inherit::hand_down` clears the flag on each of those).
///
/// The session's own descriptors are not the app's: a compositor's GPU node reached through a
/// driver that left its descriptor open, a saved document, a socket — every one of them would otherwise
/// reach every app the session starts, as an inherited handle on what the session holds. Added
/// before the launcher's own `pre_exec` steps, which hand the app what it is given after this.
#[cfg(target_os = "linux")]
fn close_inherited(command: &mut Command) {
    // SAFETY: `mark_inherited_close_on_exec` makes only async-signal-safe calls (close_range,
    // fcntl and sysconf) and allocates nothing.
    unsafe {
        command.pre_exec(mark_inherited_close_on_exec);
    }
}

#[cfg(not(target_os = "linux"))]
fn close_inherited(_: &mut Command) {}

/// Mark every descriptor of this process beyond its standard streams close-on-exec: in a child
/// between fork and exec ([`close_inherited`]), or in the session itself just before a launch
/// that is started without a fork. Async-signal-safe: `close_range`, or `fcntl` and `sysconf` on a
/// kernel without it; nothing is allocated.
#[cfg(target_os = "linux")]
fn mark_inherited_close_on_exec() -> std::io::Result<()> {
    // SAFETY: plain syscalls on this process's own descriptor table.
    unsafe {
        let closed = libc::syscall(
            libc::SYS_close_range,
            3,
            libc::c_uint::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        );
        if closed == 0 {
            return Ok(());
        }
        // A kernel without close_range: the same flag, one descriptor at a time.
        let limit = libc::sysconf(libc::_SC_OPEN_MAX);
        let limit = i32::try_from(limit).unwrap_or(1024).max(3);
        for fd in 3..limit {
            if libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) == -1 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::EBADF) {
                    return Err(error);
                }
            }
        }
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
#[expect(clippy::unnecessary_wraps, reason = "the Linux function's signature")]
const fn mark_inherited_close_on_exec() -> std::io::Result<()> {
    Ok(())
}

/// Tell the launcher `command` starts to start `keeper` in `dir`, keeping `launch`.
#[cfg(target_os = "linux")]
fn keep_in(
    command: &mut Command,
    keeper: &Path,
    dir: &Path,
    launch: &tilo_app_keeper::registry::KeptLaunch,
) {
    command
        .arg("--keeper")
        .arg(keeper)
        .arg("--keep-in")
        .arg(dir)
        .arg("--keep-as")
        .arg(launch.encode());
}

/// Wait for the keeper the launcher `launcher` starts for `kept`, greet it and show its app on
/// `desktop`, then drain what it says until it goes: the launch's end is the launcher's to tell.
#[cfg(target_os = "linux")]
fn watch_started(
    kept: Arc<crate::kept::Kept>,
    launcher: u32,
    desktop: crate::kept::Desktop,
) -> Option<Heard> {
    let pidfd = crate::kept::pidfd(launcher)?;
    let spawned = std::thread::Builder::new()
        .name("kept-attach".into())
        .spawn(move || {
            if !crate::kept::wait_for_control(&kept.dir, &pidfd) {
                return None;
            }
            match crate::kept::greet(&kept.dir, crate::kept::ANSWER) {
                Ok(greeted) => {
                    if let Err(error) = kept.greeted(&greeted.control, greeted.keeper) {
                        tracing::warn!(%error, "a kept app's keeper cannot be written to");
                        return None;
                    }
                    kept.attach(&desktop);
                    kept.listen(&greeted.control, greeted.inbox)
                }
                Err(error) => {
                    tracing::warn!(dir = %kept.dir.display(), ?error, "a kept app's keeper did not answer");
                    None
                }
            }
        });
    spawned
        .inspect_err(|error| tracing::warn!(%error, "a kept app is not shown"))
        .ok()
}

#[cfg(not(target_os = "linux"))]
impl Spawner {
    /// Only Linux keeps apps across a restart.
    #[expect(clippy::unused_self, reason = "the Linux spawner's signature")]
    fn kept_launch(
        &self,
        _: &Planned,
        _: &str,
        _: Option<&str>,
        _: &mut Command,
        _: Option<()>,
    ) -> Result<(Option<()>, Vec<()>), SpawnError> {
        Ok((None, Vec::new()))
    }

    #[expect(clippy::unused_self, reason = "the Linux spawner's signature")]
    const fn abandon(&self, _: Option<&()>) {}

    #[expect(clippy::unused_self, reason = "the Linux spawner's signature")]
    fn app_of(&self, pid: u32, name: &str, _: Option<&()>) -> (App, Option<Heard>) {
        (App::of(pid, name), None)
    }
}

/// How the launch behind `kept` ended, its launcher having ended as `end` says: `None` for a
/// launch the session let go of for a restart, which the desktop after it reports. The launcher's
/// exit says only that the keeper ended; the keeper said how its app did (`heard`). A keeper that
/// said nothing was ended by a signal and took its app with it: for memory, which the launcher's
/// end tells, or otherwise ([`End::KeeperStopped`]: the signal was the keeper's, not the app's).
#[cfg(target_os = "linux")]
fn kept_end(kept: Option<&crate::kept::Kept>, heard: Option<Heard>, end: End) -> Option<End> {
    let Some(kept) = kept else {
        return Some(end);
    };
    if kept.released() {
        return None;
    }
    let heard = heard.and_then(|heard| heard.join().ok().flatten());
    kept.remove();
    Some(match (heard, end.memory()) {
        (Some(heard), Some(cause)) if heard.crashed() => heard.remembering(cause),
        (Some(heard), _) => heard,
        (None, None) if matches!(end, End::Signalled { .. }) => End::KeeperStopped,
        (None, _) => end,
    })
}

/// The thread that reads a kept launch's keeper, which answers how the keeper said its app ended
/// once the keeper is gone: `None` when it said nothing.
type Heard = std::thread::JoinHandle<Option<End>>;

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
    /// The session asked this app to end, as the person's own Quit does: its end is theirs, so no
    /// notice says it stopped working ([`crate::Event::asked_to_end`]).
    asked_to_end: Arc<AtomicBool>,
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
                asked_to_end: Arc::new(AtomicBool::new(false)),
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
            asked_to_end: Arc::new(AtomicBool::new(false)),
            pidfd,
            kept: None,
        }
    }

    /// The keeper of a launch `teardown` leaves running: a service's at any teardown; else only at
    /// an in-place restart, and only a launch whose keeper is still there to take it back
    /// ([`crate::kept::Kept::answers`]). Everything else ends.
    #[cfg(target_os = "linux")]
    fn stays_for(&self, teardown: Teardown) -> Option<&Arc<crate::kept::Kept>> {
        let kept = self.kept.as_ref()?;
        // A service's keeper is the service's to end, whatever ends the session.
        if kept.service {
            return Some(kept);
        }
        match teardown {
            Teardown::Logout => None,
            Teardown::InPlaceRestart => Some(kept).filter(|kept| kept.answers()),
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn of(pid: u32, app: &str) -> Self {
        Self {
            pid,
            name: app.to_owned(),
            started: Instant::now(),
            asked_to_end: Arc::new(AtomicBool::new(false)),
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
    /// asked to end. A kept launch's directory goes once it was ended, and what its keeper's X11
    /// display left in `/tmp` once the keeper is gone: a keeper ended with its cgroup has no
    /// moment to remove those files itself. `session_ends` when nothing of the session is left to
    /// clear them later, so the wait for the keeper is here; otherwise it is off the loop.
    fn end(&self, session_ends: bool) {
        // Whoever ends it here is the person or the session they asked: the end that follows is no
        // crash, and the app's saved place closes quietly.
        self.asked_to_end.store(true, Ordering::Release);
        self.stop();
        #[cfg(target_os = "linux")]
        if let Some(kept) = &self.kept {
            kept.remove_awaiting(if session_ends {
                crate::kept::Await::Here(crate::kept::KEEPER_END)
            } else {
                crate::kept::Await::Aside
            });
        }
        #[cfg(not(target_os = "linux"))]
        let _ = session_ends;
    }

    /// End the launch's cgroup, or ask its process to end where it has none.
    fn stop(&self) {
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
/// (`TILO_APP_STDERR_DIR`): a headless acceptance reads them there, and the log keeps them too.
fn diagnostics_file(planned: &Planned) -> Option<std::fs::File> {
    let dir = std::env::var_os("TILO_APP_STDERR_DIR")?;
    let path = std::path::Path::new(&dir).join(format!("{}.log", planned.app));
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::File::create(&path).ok()
}

/// Tell the app `command` starts to write its startup timeline, when this session measures its
/// launches ([`tilo_startup_metrics::STARTUP_TIMELINE_ENV`]): which of its steps (the launcher's,
/// the sandbox's, loading, fonts, records, first layout) an open was spent in. It is written inside
/// the app's own directory, the one place the sandbox lets it write, and its summary line reaches
/// the session's log with the app's stderr; a measured session reads either. A session that does
/// not measure costs its apps no timeline.
fn measure_startup(command: &mut Command) {
    if tilo_startup_metrics::session_measures_launches() {
        command.env(
            "TILO_STARTUP_METRICS_PATH",
            launcher::appdir::SANDBOX_STARTUP_TIMELINE,
        );
    }
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
                "printf '%s\\n' \"$WAYLAND_DISPLAY\" \"$TILO_APPS_DATA_DIR\" \"$LEAK\" > {}",
                written.display()
            );
            let planned = shell(
                &script,
                BTreeMap::from([
                    ("WAYLAND_DISPLAY".to_owned(), "wayland-6".to_owned()),
                    ("TILO_APPS_DATA_DIR".to_owned(), "person-4".to_owned()),
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

    /// The session ends its apps: the one it ended on the person's ask is reported as asked to
    /// end, so the place its window was closes quietly, and one still running when the session
    /// goes ends with it.
    #[cfg(target_os = "linux")]
    #[test]
    fn the_sessions_apps_end_with_it_as_asked_or_not() {
        let mut spawner = Spawner::default();
        let quiet = spawner
            .start(&shell("exit 0", BTreeMap::new()))
            .expect("starts");
        let asked = spawner
            .start(&shell("exec sleep infinity", BTreeMap::new()))
            .expect("starts");
        spawner.end_one(asked.id);
        let mut ended = events(&spawner, 2);
        ended.sort_by_key(|event| event.id);
        assert_eq!(ended[0].id, quiet.id);
        assert!(
            !ended[0].asked_to_end,
            "an app that closed on its own was not asked"
        );
        assert_eq!(ended[1].id, asked.id);
        assert!(
            ended[1].asked_to_end,
            "the end the session asked for is the person's"
        );
        assert!(
            ended[1].end.crashed(),
            "a signal is how the ask reaches the app: {:?}",
            ended[1].end
        );

        // The one that ended is reaped and forgotten: the drop signals only
        // what is still running.
        let running = spawner
            .start(&shell("exec sleep infinity", BTreeMap::new()))
            .expect("starts");
        let exited = spawner
            .start(&shell("exit 0", BTreeMap::new()))
            .expect("starts");
        assert_eq!(events(&spawner, 1)[0].id, exited.id);

        drop(spawner);
        tilo_test_env::exited("the app its session no longer runs", running.pid);
    }

    /// A force quit names the app by its process, not by the launch: the launch that runs it is
    /// found, and its end is the person's, so no notice says it stopped working.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_app_ended_by_its_process_is_the_persons_own_quit() {
        let mut spawner = Spawner::default();
        let stuck = spawner
            .start(&shell("exec sleep infinity", BTreeMap::new()))
            .expect("starts");
        assert!(
            spawner.end_pid(stuck.pid),
            "the launch that runs that process is found"
        );
        let ended = events(&spawner, 1);
        assert_eq!(ended[0].id, stuck.id);
        assert!(
            ended[0].asked_to_end,
            "the quit is the person's, never a crash"
        );
        assert!(
            !spawner.end_pid(u32::MAX),
            "a process no launch of this session runs is not one"
        );
    }

    /// A sandboxed app is a child of the launcher this session started: a quit naming the app's
    /// own process still finds its launch, and its end is the person's.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_app_under_its_launcher_is_ended_as_the_persons_own_quit() {
        let mut spawner = Spawner::default();
        let launch = spawner
            .start(&shell("sleep infinity & wait", BTreeMap::new()))
            .expect("starts");
        let app = child_of(launch.pid);
        assert!(
            spawner.end_pid(app),
            "the launch that runs the app is found up the process tree"
        );
        let ended = events(&spawner, 1);
        assert_eq!(ended[0].id, launch.id);
        assert!(
            ended[0].asked_to_end,
            "the quit is the person's, never a crash"
        );
        // No scope holds the app here, so it outlives the launcher it was started from: the
        // test ends it.
        // SAFETY: a plain pid signal.
        unsafe { libc::kill(i32::try_from(app).expect("a pid fits"), libc::SIGKILL) };
        tilo_test_env::exited("the app the test ended", app);
    }

    /// The first process the shell `pid` started, once it has.
    #[cfg(target_os = "linux")]
    fn child_of(pid: u32) -> u32 {
        let started = Instant::now();
        loop {
            if let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) {
                for task in tasks.flatten() {
                    if let Ok(children) = std::fs::read_to_string(task.path().join("children"))
                        && let Some(child) = children.split_whitespace().next()
                    {
                        return child.parse().expect("a pid fits");
                    }
                }
            }
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "the launch started no child"
            );
            std::thread::park_timeout(Duration::from_millis(5));
        }
    }

    /// A package's launch starts without forking the session, and with none of the descriptors the
    /// session holds. The session is the compositor: a fork copies every page table of a large
    /// multi-threaded process on the thread that draws the desktop, and in a desktop session it was
    /// most of the time between an app's open and its launcher running. glibc runs the
    /// `pthread_atfork` handlers on a `fork()` and not on a `posix_spawn`, in the forking thread,
    /// so this thread counts its own forks while other tests start what they like.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_package_launch_does_not_fork_the_session_nor_hand_it_what_the_session_holds() {
        use std::cell::Cell;
        use std::os::fd::AsRawFd as _;

        thread_local!(static FORKS: Cell<u32> = const { Cell::new(0) });
        extern "C" fn forking() {
            FORKS.with(|forks| forks.set(forks.get().saturating_add(1)));
        }
        static COUNTED: std::sync::Once = std::sync::Once::new();
        // SAFETY: the handler only bumps a thread-local counter, which is async-signal-safe.
        COUNTED.call_once(|| unsafe {
            assert_eq!(libc::pthread_atfork(Some(forking), None, None), 0);
        });

        let directory = tempfile::tempdir().expect("a directory");
        // The session's own descriptor, left without close-on-exec as a driver's own is.
        let held = std::fs::File::create(directory.path().join("the-sessions-own"))
            .expect("a file the session holds");
        // SAFETY: the descriptor is this process's, open until `held` is dropped.
        unsafe {
            assert_eq!(libc::fcntl(held.as_raw_fd(), libc::F_SETFD, 0), 0);
        }
        let listed = directory.path().join("descriptors");
        let planned = Planned {
            kind: Kind::Package,
            ..shell(
                &format!("ls -l /proc/$$/fd > {}", listed.display()),
                BTreeMap::new(),
            )
        };
        let mut spawner = Spawner::default();
        let forked = FORKS.with(Cell::get);
        spawner.start(&planned).expect("starts");
        assert_eq!(
            FORKS.with(Cell::get),
            forked,
            "the launch forked the session: a `pre_exec` step makes `Command::spawn` fork"
        );
        assert_eq!(events(&spawner, 1)[0].end, End::exited(0));
        let descriptors = std::fs::read_to_string(&listed).expect("what the app held");
        assert!(
            !descriptors.contains("the-sessions-own"),
            "the app inherited the session's descriptor:\n{descriptors}"
        );
        drop(held);
    }
}
