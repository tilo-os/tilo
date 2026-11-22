//! Apps that stay running while the desktop restarts in place.
//!
//! A program Tilo did not build (one of the machine's own apps, anything whose windows the Tilo
//! runtime does not draw) loses its windows, and usually its life, when its compositor goes away.
//! Such a launch therefore starts behind an app keeper (`tilo-app-keeper`): the launcher starts the
//! keeper in the app's scope once it is there, the app as the keeper's child, and the keeper is the
//! app's Wayland display while the desktop is one client of it ([`Planned::kept`]).
//!
//! Each keeper has a private directory in the keepers' registry
//! ([`tilo_app_keeper::registry`]): its sockets, the record of how its app ended, and the launch it
//! keeps. A run of the session adopts every keeper it finds there, whoever started it, and keeps
//! watching the registry: a keeper a service starts later (the Linux VM's window forwarder) is
//! taken up and shown when it appears.
//!
//! - **Running.** The session connects to the keeper's control socket, says hello, hands it a new
//!   desktop connection named for the keeper's process (so its windows trace back to the launch),
//!   and asks it to describe itself. Only a keeper that answered is one this run may leave running.
//! - **In-place restart.** The run that ends leaves each such launch as it is: no signal, no cgroup
//!   kill ([`crate::spawner::Teardown`]). Only a keeper this session met whose process is still there
//!   and whose socket still takes a connection is left; a keeper that is gone ends with the desktop
//!   ([`Kept::answers`]). Everything else ends as at a logout, and a logout ends kept apps too.
//! - **Adoption.** The run that follows a restart describes each keeper again, checks that the
//!   app's process is the one the keeper started (pid and start time against `/proc`), and takes
//!   the launch back into its spawner: tiers, the memory watch, the end report and ending it at
//!   logout work as for a launch it started, and what the app wrote goes on being kept: the
//!   keeper holds the newest [`tilo_app_keeper::output::BYTES`] of each stream in its directory,
//!   and the desktop reads it from the first line still held. Nobody watches memory pressure in
//!   the seconds between
//!   runs; the kernel's group OOM kill is the backstop, and its cause is read from the cgroup's
//!   events when the end is reported. An app that ended while no desktop was there is reported from
//!   the keeper's `ended` record. Any other start finds only leftovers: their apps are ended and
//!   their directories removed.
//! - **A service's keeper** ([`KeptLaunch::service`]) is the service's: any run adopts it, shows
//!   its windows and leaves it running, at a logout too; its end is the service's to handle and is
//!   never reported, and the session never signals it or kills its cgroup (it runs in the
//!   service's own).
//! - **Outdated.** A keeper of a build before an update that changed its interface cannot be
//!   shown ([`Outdated`]). It is taken as a launch all the same, with no windows, so no second
//!   copy of its app starts and a logout ends it; its keeper holds the app while this session stays
//!   connected; and restarting it ([`end_outdated`]) ends the old app so it can start again.

use std::io::{self, Read as _, Write as _};
use std::os::fd::{AsFd as _, AsRawFd as _, FromRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

// What an adopted launch is, as the session reads it.
pub use tilo_app_keeper::registry::{KeptLaunch, LAUNCH_VERSION};
use tilo_app_keeper::wire::{self, Description, FromKeeper, ToKeeper};
use tilo_lock::MutexRecover as _;
/// When a process started, `None` once it exited (reaped or not): an app or keeper that exited
/// is never taken for one still running.
pub(crate) use tilo_process::start_time;

use crate::spawner::End;

pub use tilo_app_keeper::wire::Window;

#[cfg(any(test, feature = "test-support"))]
pub mod testing;

/// How long a keeper may take to answer a session that greets it: one in the middle of a restart
/// answers in milliseconds, and a machine busy applying the update gets this long before the
/// keeper is not taken.
pub(crate) const ANSWER: Duration = Duration::from_secs(5);

/// How long a session that is ending waits for a keeper it ended to be gone before it clears what
/// the keeper's X11 display left: a killed process ends in moments.
pub(crate) const KEEPER_END: Duration = Duration::from_secs(1);

/// How long a session that goes on watches a keeper it removed the directory of, to clear what its
/// X11 display left once it is gone: longer than a keeper told to end gives its app.
const KEEPER_GONE: Duration = Duration::from_secs(30);

/// How long clearing an ended keeper's display waits for its name to be free: the launch's X
/// server holds it for a moment after the keeper ends.
const DISPLAY_FREE: Duration = Duration::from_secs(1);

/// Where whoever removes a keeper's directory waits for the keeper to be gone before it clears
/// what the keeper's X11 display left in `/tmp`. A keeper that ends on its own removes its files
/// itself; one killed (its cgroup ended, by a logout or the memory guard) cannot.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Await {
    /// Here, up to this long: the session is ending, and nothing of it would clear them later.
    Here(Duration),
    /// On a thread of its own, up to [`KEEPER_GONE`]: the session's loop never waits on a keeper.
    Aside,
}

/// A connection to the desktop for the process `pid`: what a keeper is attached with.
pub type Desktop = Arc<dyn Fn(u32) -> io::Result<UnixStream> + Send + Sync>;

/// What starting apps behind keepers needs: the keeper program, the registry and the desktop.
#[derive(Clone)]
pub struct Keeping {
    pub keeper: PathBuf,
    pub root: PathBuf,
    pub desktop: Desktop,
}

impl Keeping {
    /// The keeper installed here and the person's registry, attached to `desktop`; `None` when no
    /// keeper is installed.
    #[must_use]
    pub fn installed(desktop: Desktop) -> Option<Self> {
        Some(Self {
            keeper: tilo_app_keeper::registry::find_keeper()?,
            root: tilo_app_keeper::registry::root(),
            desktop,
        })
    }
}

/// The keeper directories under `root`.
pub(crate) fn dirs(root: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut dirs: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    dirs.sort();
    dirs
}

/// How a kept app ended, from the record its keeper left.
pub(crate) fn recorded_end(dir: &Path) -> Option<End> {
    let bytes = std::fs::read(dir.join("ended")).ok()?;
    serde_json::from_slice::<wire::End>(&bytes).ok().map(end_of)
}

pub(crate) fn end_of(end: wire::End) -> End {
    match end {
        wire::End::Exited { code } => End::exited(code),
        wire::End::Signalled { signal, memory } => {
            let end = End::signalled(signal);
            if memory {
                end.remembering(crate::spawner::Cause::Kernel)
            } else {
                end
            }
        }
    }
}

/// The launch the session wrote into `dir`, when it was this version's.
pub(crate) fn written_launch(dir: &Path) -> Option<KeptLaunch> {
    KeptLaunch::decode(&std::fs::read_to_string(dir.join("launch")).ok()?)
}

/// The X11 display a keeper in `dir` bound, and the cookie file beside it, once it wrote one.
fn x11_of(dir: &Path) -> Option<(String, PathBuf)> {
    let display = std::fs::read_to_string(dir.join("x11")).ok()?;
    let display = display.trim();
    display
        .starts_with(':')
        .then(|| (display.to_owned(), dir.join("Xauthority")))
}

/// `N` of the X11 display `:N` the keeper in `dir` bound, once it wrote it.
fn display_number(dir: &Path) -> Option<u32> {
    x11_of(dir)?.0.strip_prefix(':')?.parse().ok()
}

/// Remove the keeper directory `dir`, and what its X11 display left in `/tmp` once its keeper
/// (`keeper`, `None` for one already gone) is gone, waiting for that as `wait` says.
pub(crate) fn remove_dir(dir: &Path, keeper: Option<OwnedFd>, wait: Await) -> io::Result<()> {
    let display = display_number(dir);
    let removed = std::fs::remove_dir_all(dir);
    if let Some(number) = display {
        clear_once_gone(PathBuf::from("/tmp"), number, keeper, wait);
    }
    removed
}

/// Clear what X11 display `number` left in `tmp` once `keeper` is gone, waiting as `wait` says:
/// at once for a keeper that is gone already.
fn clear_once_gone(tmp: PathBuf, number: u32, keeper: Option<OwnedFd>, wait: Await) {
    let Some(keeper) = keeper.filter(|keeper| !ended_within(keeper, Duration::ZERO)) else {
        clear_dead_display(&tmp, number);
        return;
    };
    match wait {
        Await::Here(patience) => {
            if ended_within(&keeper, patience) {
                clear_dead_display(&tmp, number);
            }
        }
        Await::Aside => {
            let watching = std::thread::Builder::new()
                .name("keeper-gone".into())
                .spawn(move || {
                    if ended_within(&keeper, KEEPER_GONE) {
                        clear_dead_display(&tmp, number);
                    }
                });
            if let Err(error) = watching {
                tracing::warn!(%error, number, "a removed keeper's X11 display files stay");
            }
        }
    }
}

/// Remove the lock and socket files of X11 display `number` under `tmp` when no process holds
/// the display any more: a keeper that was killed (a stop past its grace, a logout's end of its
/// cgroup) cannot remove its own, and over a long uptime they pile up. Nothing is removed while
/// the display's abstract name is bound (a keeper holds it for as long as it lives) or while the
/// lock names a process that is still alive. The name is held here meanwhile, as a keeper takes a
/// display, so one taking the number at the same moment never loses its files; and since an
/// ended keeper's X server holds it for a moment more, the take waits for the server to go rather
/// than giving up. True when the files were a dead keeper's and are gone.
pub(crate) fn clear_dead_display(tmp: &Path, number: u32) -> bool {
    use std::os::linux::net::SocketAddrExt as _;
    let Ok(name) =
        std::os::unix::net::SocketAddr::from_abstract_name(format!("/tmp/.X11-unix/X{number}"))
    else {
        return false;
    };
    // Taking the name is what keeps a live keeper's files from ever being removed: a keeper holds
    // it for as long as it lives. One that just ended leaves its X server closing for a moment,
    // and that holds the name too, so the take is waited for; a keeper that keeps the display is
    // never waited out.
    let Some(deadline) = Instant::now().checked_add(DISPLAY_FREE) else {
        return false;
    };
    let _held = loop {
        match std::os::unix::net::UnixListener::bind_addr(&name) {
            Ok(held) => break held,
            Err(error) => {
                if Instant::now() >= deadline {
                    tracing::info!(number, %error, "an ended keeper's display is still held");
                    return false;
                }
                std::thread::sleep(Duration::from_millis(2));
            }
        }
    };
    let lock = tmp.join(format!(".X{number}-lock"));
    let holder = std::fs::read_to_string(&lock)
        .ok()
        .and_then(|text| text.trim().parse::<u32>().ok());
    if holder.is_some_and(alive) {
        return false;
    }
    for file in [lock, tmp.join(".X11-unix").join(format!("X{number}"))] {
        if let Err(error) = std::fs::remove_file(&file)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(file = %file.display(), %error, "a dead X11 display's file stays");
        }
    }
    true
}

/// Whether `pid` is alive: an ended process nobody reaped yet is not.
fn alive(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).is_ok_and(|stat| {
        stat.rsplit_once(')')
            .and_then(|(_, rest)| rest.split_whitespace().next())
            .is_some_and(|state| state != "Z" && state != "X")
    })
}

/// A kept launch's keeper, as the spawner holds it.
pub(crate) struct Kept {
    pub(crate) dir: PathBuf,
    /// A service started it ([`KeptLaunch::service`]): the session only shows it.
    pub(crate) service: bool,
    /// The control connection, once greeted: what the session writes on.
    control: Mutex<Option<UnixStream>>,
    /// The keeper's process, once connected.
    keeper: AtomicU32,
    /// The keeper answered Describe in this run: only such a launch may outlive it.
    described: AtomicBool,
    /// The run let go of the launch for an in-place restart: its watcher stops quietly.
    released: AtomicBool,
    /// This session took the launch back after a restart: what the desktop that started it
    /// served for the launch — its filtered network — is this one's to serve again.
    adopted: AtomicBool,
    /// A desktop was handed to the keeper in this run.
    attached: AtomicBool,
    /// What the keeper was last told of where each window stands, by tag.
    placed: Mutex<std::collections::HashMap<String, String>>,
    /// The X11 display this launch serves its programs, and the cookie file that admits a
    /// client, once the keeper bound one.
    x11: Mutex<Option<(String, PathBuf)>>,
    /// The keeper speaks another version ([`Outdated`]): this session shows nothing of it, and
    /// only ends it.
    outdated: bool,
    /// When the keeper this session greeted started, as `/proc` read it: what its pid is checked
    /// against before it is trusted with its app again (0: not known).
    keeper_started: AtomicU64,
}

impl Kept {
    pub(crate) fn new(dir: PathBuf, service: bool) -> Arc<Self> {
        Arc::new(Self {
            dir,
            service,
            control: Mutex::new(None),
            keeper: AtomicU32::new(0),
            described: AtomicBool::new(false),
            released: AtomicBool::new(false),
            adopted: AtomicBool::new(false),
            attached: AtomicBool::new(false),
            placed: Mutex::new(std::collections::HashMap::new()),
            x11: Mutex::new(None),
            outdated: false,
            keeper_started: AtomicU64::new(0),
        })
    }

    /// The outdated keeper `outdated` in `dir`, holding its app on the connection it was asked on
    /// when it does.
    pub(crate) fn outdated(dir: PathBuf, service: bool, outdated: Outdated) -> Arc<Self> {
        Arc::new(Self {
            dir,
            service,
            control: Mutex::new(outdated.holding),
            keeper: AtomicU32::new(outdated.keeper),
            described: AtomicBool::new(false),
            released: AtomicBool::new(false),
            adopted: AtomicBool::new(true),
            attached: AtomicBool::new(true),
            placed: Mutex::new(std::collections::HashMap::new()),
            x11: Mutex::new(None),
            outdated: true,
            keeper_started: AtomicU64::new(outdated.keeper_started.unwrap_or(0)),
        })
    }

    /// Whether the keeper speaks another version than this session.
    pub(crate) const fn is_outdated(&self) -> bool {
        self.outdated
    }

    /// End an outdated keeper's app as Restart asks ([`end_outdated`]).
    pub(crate) fn end_outdated(&self) {
        if self.outdated {
            end_outdated(&self.dir, self.keeper(), self.keeper_started());
        }
    }

    /// Whether this session may leave the launch through an in-place restart.
    ///
    /// What counts is what this session knows, not how fast the keeper answers now. It met the
    /// keeper — it answered, or said it speaks another version — so the launch stays when its
    /// keeper's process is still the one met and its control socket still takes a connection. The
    /// session that follows greets it again and takes it back; one that cannot answer even then is
    /// left to end its own app once no desktop comes back ([`tilo_app_keeper::DESKTOP_GONE`]).
    ///
    /// Nothing here waits for a reply. An in-place restart applies an update, and the machine may
    /// be busy doing it: a second round trip asked inside the teardown would turn a delay in
    /// scheduling a healthy keeper into an app — with whatever unsaved work it holds — ended for
    /// nothing. Only a keeper that is gone ends with this session: its process is not the one greeted
    /// (stopped long enough to be reaped, or replaced by another process holding its pid), or its
    /// socket refuses, and no later session could take it back either way.
    pub(crate) fn answers(&self) -> bool {
        // An outdated keeper is never described; the next session meets it as this one did.
        let answered = self.outdated || self.described.load(Ordering::Acquire);
        answered
            && (self.outdated || self.control.lock_recover().is_some())
            && self.keeper_alive()
            && tilo_files::native::socket::connect(&self.dir.join("control")).is_ok()
    }

    /// Whether the keeper process is still the one this session met: the pid names a process that
    /// started when the keeper did, so a pid the kernel handed to someone else is never taken for
    /// it.
    fn keeper_alive(&self) -> bool {
        let keeper = self.keeper();
        keeper != 0
            && self
                .keeper_started()
                .is_some_and(|at| start_time(keeper) == Some(at))
    }

    /// When the keeper started, as read when it was greeted (or met, for an outdated one).
    fn keeper_started(&self) -> Option<u64> {
        match self.keeper_started.load(Ordering::Acquire) {
            0 => None,
            started => Some(started),
        }
    }

    /// The keeper's process, once connected (0 before).
    pub(crate) fn keeper(&self) -> u32 {
        self.keeper.load(Ordering::Acquire)
    }

    /// Tell the keeper that its window `tag` stands at `placement`, unless that is what it was
    /// last told.
    pub(crate) fn place(&self, tag: &str, placement: &str) {
        if self
            .placed
            .lock_recover()
            .get(tag)
            .is_some_and(|last| last == placement)
        {
            return;
        }
        let Some(control) = self
            .control
            .lock_recover()
            .as_ref()
            .and_then(|control| control.try_clone().ok())
        else {
            return;
        };
        let message = ToKeeper::SetPlacement {
            tag: tag.to_owned(),
            placement: placement.to_owned(),
        };
        match send(&control, &message, None) {
            Ok(()) => {
                self.placed
                    .lock_recover()
                    .insert(tag.to_owned(), placement.to_owned());
            }
            Err(error) => {
                tracing::warn!(dir = %self.dir.display(), %error, "a kept window's place was not told");
            }
        }
    }

    /// Whether the run let go of the launch for an in-place restart.
    pub(crate) fn released(&self) -> bool {
        self.released.load(Ordering::Acquire)
    }

    /// Whether this session took the launch back after a restart, rather than starting it.
    pub(crate) fn adopted(&self) -> bool {
        self.adopted.load(Ordering::Acquire)
    }

    /// The launch was taken back after a restart: what its first desktop served for it may have
    /// gone with that desktop.
    pub(crate) fn take_back(&self) {
        self.adopted.store(true, Ordering::Release);
    }

    /// A descriptor of the keeper's process, while it is known.
    fn keeper_fd(&self) -> Option<OwnedFd> {
        let keeper = self.keeper.load(Ordering::Acquire);
        (keeper != 0).then(|| pidfd(keeper)).flatten()
    }

    /// Let go of the keeper without touching it: the next desktop takes it back.
    pub(crate) fn release(&self) {
        self.released.store(true, Ordering::Release);
        let control = self.control.lock_recover().take();
        if let Some(control) = control {
            let _ = control.shutdown(std::net::Shutdown::Both);
        }
    }

    /// The X11 display this launch serves its programs, and the cookie file that admits a
    /// client: what a session asks its input method to serve as XIM. `None` for a launch the
    /// keeper was not asked to serve X11 to, and while its server is not up: a client that
    /// asked for the display would start a server the app never asked for.
    pub(crate) fn x11(&self) -> Option<(String, PathBuf)> {
        if !self.dir.join("x11-server").is_file() {
            return None;
        }
        let mut x11 = self.x11.lock_recover();
        if x11.is_none() {
            *x11 = x11_of(&self.dir);
        }
        x11.clone()
    }

    /// The control socket of this launch's network proxy, once it has one: the desktop that
    /// adopts the program after a restart sets the policy it enforces through it.
    #[cfg(all(target_os = "linux", feature = "keeper"))]
    pub(crate) fn network_control(&self) -> Option<PathBuf> {
        let control = self.dir.join(launcher::netproxy::CONTROL);
        control.exists().then_some(control)
    }

    /// The launch ends: its directory goes with it, unless a service's (the service reuses or
    /// removes it).
    pub(crate) fn remove(&self) {
        self.remove_awaiting(Await::Aside);
    }

    /// [`Self::remove`], waiting for the keeper to be gone as `wait` says before what its X11
    /// display left in `/tmp` goes.
    pub(crate) fn remove_awaiting(&self, wait: Await) {
        if self.service {
            return;
        }
        if let Err(error) = remove_dir(&self.dir, self.keeper_fd(), wait)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(dir = %self.dir.display(), %error, "a kept app's directory stays");
        }
    }

    /// Hand the keeper a new connection to `desktop`, unless it has one from this run.
    pub(crate) fn attach(&self, desktop: &Desktop) {
        let keeper = self.keeper.load(Ordering::Acquire);
        let control = self
            .control
            .lock_recover()
            .as_ref()
            .and_then(|control| control.try_clone().ok());
        let Some(control) = control else {
            return;
        };
        if keeper == 0 || self.attached.swap(true, Ordering::AcqRel) {
            return;
        }
        let attached =
            desktop(keeper).and_then(|socket| send(&control, &ToKeeper::Attach, Some(&socket)));
        if let Err(error) = attached {
            tracing::warn!(dir = %self.dir.display(), %error, "a kept app was not shown");
        }
    }
}

/// Why a keeper was not taken up.
#[derive(Debug)]
pub(crate) enum Refused {
    /// Nothing listens: the keeper is gone.
    Gone,
    /// It speaks another version of the interface: it was started before an update and runs on.
    Version(Outdated),
    Failed(io::Error),
}

/// A keeper at another version than this session's: its app goes on, and this session can show
/// none of it. Only the part of the interface that never changes is spoken with it
/// ([`tilo_app_keeper::wire`]).
#[derive(Debug)]
pub(crate) struct Outdated {
    pub(crate) version: u32,
    /// The keeper's process, and when it started, so a pid the kernel gave someone else since is
    /// never signalled.
    pub(crate) keeper: u32,
    pub(crate) keeper_started: Option<u64>,
    /// The connection the keeper holds its app for while it stays open; none from a keeper of a
    /// build before that rule, which closes it and ends its app once its desktop stayed away for
    /// [`tilo_app_keeper::DESKTOP_GONE`].
    pub(crate) holding: Option<UnixStream>,
}

/// How long an app asked to end may take before its launch's scope is ended under it: what the
/// person waits for after pressing Restart.
pub(crate) const ASKED_TO_END: Duration = Duration::from_secs(3);

/// The app of the outdated keeper in `dir`, verified against `/proc`: as its `app` file names it,
/// else the keeper's one child (keepers from before that file).
pub(crate) fn kept_app(dir: &Path, keeper: u32) -> Option<wire::AppProcess> {
    let written = std::fs::read(dir.join("app"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<wire::AppProcess>(&bytes).ok());
    let process = written.or_else(|| {
        // Each of its threads lists the children it started.
        let children: Vec<u32> = std::fs::read_dir(format!("/proc/{keeper}/task"))
            .ok()?
            .filter_map(Result::ok)
            .filter_map(|task| std::fs::read_to_string(task.path().join("children")).ok())
            .flat_map(|children| {
                children
                    .split_whitespace()
                    .filter_map(|pid| pid.parse().ok())
                    .collect::<Vec<u32>>()
            })
            .collect();
        // A keeper starts one process, its app.
        let [pid] = children.as_slice() else {
            return None;
        };
        let pid = *pid;
        Some(wire::AppProcess {
            pid,
            started_at: start_time(pid)?,
        })
    })?;
    (start_time(process.pid) == Some(process.started_at)).then_some(process)
}

/// End the app of the outdated keeper in `dir`, as Restart asks: its process group is asked to
/// end and given [`ASKED_TO_END`], then the launch's own scope ends with everything left in it,
/// keeper included, and the directory goes. Every process is checked against when it started
/// before it is signalled; a launch outside a scope of its own is never ended by cgroup, only its
/// verified app and keeper are.
pub(crate) fn end_outdated(dir: &Path, keeper: u32, keeper_started: Option<u64>) {
    if let Some(app) = kept_app(dir, keeper)
        && let Some(fd) = pidfd(app.pid)
        // Checked once the descriptor holds the process: it is that one, never a later one.
        && start_time(app.pid) == Some(app.started_at)
    {
        signal_group(app.pid, libc::SIGTERM);
        ended_within(&fd, ASKED_TO_END);
    }
    let keeper_alive = keeper_started.is_some() && start_time(keeper) == keeper_started;
    if keeper_alive {
        let scope = std::fs::read_to_string(format!("/proc/{keeper}/cgroup"))
            .ok()
            .and_then(|cgroup| launcher::app_cgroup::app_cgroup_at(&cgroup))
            .map(|cgroup| launcher::app_cgroup::fs_path(&cgroup));
        if let Some(scope) = scope {
            if let Err(error) = launcher::app_cgroup::kill(&scope) {
                tracing::warn!(%error, "an outdated kept app's scope did not end");
            }
        } else {
            // Outside a scope of its own (a service's keeper), only its verified processes end.
            if let Some(app) = kept_app(dir, keeper) {
                signal(app.pid, libc::SIGKILL);
            }
            signal(keeper, libc::SIGTERM);
        }
    }
    let keeper = keeper_alive.then(|| pidfd(keeper)).flatten();
    if let Err(error) = remove_dir(dir, keeper, Await::Aside)
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(dir = %dir.display(), %error, "an outdated keeper's directory stays");
    }
}

/// Send `signal` to the process group `leader` leads, when it leads one: a keeper starts its app
/// in a group of its own.
fn signal_group(leader: u32, signal: i32) {
    let Ok(pid) = libc::pid_t::try_from(leader) else {
        return;
    };
    // SAFETY: getpgid and kill take plain integers.
    unsafe {
        if libc::getpgid(pid) == pid {
            libc::kill(pid.saturating_neg(), signal);
        } else {
            libc::kill(pid, signal);
        }
    }
}

/// Wait at most `within` for the process `fd` holds to end: whether it did.
pub(crate) fn ended_within(fd: &OwnedFd, within: Duration) -> bool {
    let mut fds = [libc::pollfd {
        fd: fd.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    }];
    let timeout = i32::try_from(within.as_millis()).unwrap_or(i32::MAX);
    // SAFETY: one live pollfd in a buffer of that length.
    unsafe { libc::poll(fds.as_mut_ptr(), 1, timeout) > 0 }
}

impl From<io::Error> for Refused {
    fn from(error: io::Error) -> Self {
        // A keeper that went away while it was being asked is gone, not failing.
        match error.kind() {
            io::ErrorKind::ConnectionReset
            | io::ErrorKind::BrokenPipe
            | io::ErrorKind::UnexpectedEof => Self::Gone,
            _ => Self::Failed(error),
        }
    }
}

/// A keeper that answered: its connection, its process, what it holds, and what it said after.
pub(crate) struct Greeted {
    pub(crate) control: UnixStream,
    pub(crate) keeper: u32,
    pub(crate) description: Description,
    pub(crate) inbox: Inbox,
}

/// Connect to the keeper in `dir`, greet it, and read what it holds, waiting at most `answer`
/// for each of its replies.
pub(crate) fn greet(dir: &Path, answer: Duration) -> Result<Greeted, Refused> {
    let control = match tilo_files::native::socket::connect(&dir.join("control")) {
        Ok(control) => control,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Err(Refused::Gone);
        }
        Err(error) => return Err(error.into()),
    };
    control.set_read_timeout(Some(answer))?;
    let keeper = peer_pid(&control)?;
    send(
        &control,
        &ToKeeper::Hello {
            version: wire::INTERFACE_VERSION,
        },
        None,
    )?;
    let mut buffer = Inbox::default();
    let Some(first) = receive_frame(&control, &mut buffer)? else {
        return Err(Refused::Gone);
    };
    if let Some(version) = wire::mismatch_version(&first) {
        // A keeper that holds its app for this session says so next; one from before that rule
        // closes the connection.
        let holds = matches!(receive_frame(&control, &mut buffer), Ok(Some(frame)) if wire::holding(&frame));
        control.set_read_timeout(None)?;
        return Err(Refused::Version(Outdated {
            version,
            keeper,
            keeper_started: start_time(keeper),
            holding: holds.then_some(control),
        }));
    }
    if !matches!(serde_json::from_slice(&first), Ok(FromKeeper::Hello { .. })) {
        return Err(Refused::Gone);
    }
    send(&control, &ToKeeper::Describe, None)?;
    loop {
        match receive(&control, &mut buffer)? {
            Some(FromKeeper::Described(description)) => {
                control.set_read_timeout(None)?;
                return Ok(Greeted {
                    control,
                    keeper,
                    description,
                    inbox: buffer,
                });
            }
            Some(_) => {}
            None => return Err(Refused::Gone),
        }
    }
}

impl Kept {
    /// The keeper answered on `control` in this run: what the session writes on from now.
    pub(crate) fn greeted(&self, control: &UnixStream, keeper: u32) -> io::Result<()> {
        *self.control.lock_recover() = Some(control.try_clone()?);
        self.keeper.store(keeper, Ordering::Release);
        self.keeper_started
            .store(start_time(keeper).unwrap_or(0), Ordering::Release);
        self.described.store(true, Ordering::Release);
        Ok(())
    }

    /// Read the keeper's events until its app ended or the connection did; answer the end when
    /// it was heard (`None` when the run let go of the keeper, or it went without a word).
    pub(crate) fn listen(&self, control: &UnixStream, mut buffer: Inbox) -> Option<End> {
        loop {
            match receive(control, &mut buffer) {
                Ok(Some(FromKeeper::Ended { end })) => return Some(end_of(end)),
                Ok(Some(FromKeeper::Fault { fault })) => {
                    tracing::warn!(dir = %self.dir.display(), %fault, "a kept app's windows broke the desktop's protocol");
                }
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => break,
            }
        }
        if self.released.load(Ordering::Acquire) {
            return None;
        }
        recorded_end(&self.dir)
    }
}

/// The process at the other end of `stream`.
pub(crate) fn peer_pid(stream: &UnixStream) -> io::Result<u32> {
    let mut cred = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len =
        libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).map_err(io::Error::other)?;
    // SAFETY: SO_PEERCRED writes one ucred into the buffer of the length given.
    let read = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            std::ptr::from_mut(&mut cred).cast(),
            &raw mut len,
        )
    };
    if read != 0 {
        return Err(io::Error::last_os_error());
    }
    u32::try_from(cred.pid)
        .ok()
        .filter(|pid| *pid != 0)
        .ok_or_else(|| io::Error::other("the keeper's process is unknown"))
}

/// Whether the app a keeper described is the process it started: its pid names a process that
/// started when the keeper says it did, so a pid the kernel gave someone else is never taken.
pub(crate) fn same_process(description: &Description) -> bool {
    match (description.app_pid, description.started_at) {
        (Some(pid), Some(at)) => start_time(pid) == Some(at),
        _ => false,
    }
}

/// A descriptor for `pid`, for a signal that can reach no later process with its pid.
pub(crate) fn pidfd(pid: u32) -> Option<OwnedFd> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    // SAFETY: pidfd_open takes a pid and flags and returns a new descriptor or -1.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    let fd = i32::try_from(fd).ok().filter(|fd| *fd >= 0)?;
    // SAFETY: the kernel just returned this descriptor, and nothing else owns it.
    Some(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// End the keeper `keeper` and everything in its cgroup: a leftover from a run that did not end
/// in a restart. A keeper outside an app's cgroup is asked to end, and its app ends with it.
pub(crate) fn end_leftover(dir: &Path) {
    let mut ended = None;
    if let Ok(control) = tilo_files::native::socket::connect(&dir.join("control"))
        && let Ok(keeper) = peer_pid(&control)
    {
        ended = pidfd(keeper);
        tracing::info!(dir = %dir.display(), keeper, "a kept app left from before ends");
        let cgroup = std::fs::read_to_string(format!("/proc/{keeper}/cgroup"))
            .ok()
            .and_then(|cgroup| launcher::app_cgroup::app_cgroup_at(&cgroup))
            .map(|cgroup| launcher::app_cgroup::fs_path(&cgroup));
        match cgroup {
            Some(cgroup) => {
                if let Err(error) = launcher::app_cgroup::kill(&cgroup) {
                    tracing::warn!(%error, "a leftover kept app did not end");
                }
            }
            None => signal(keeper, libc::SIGTERM),
        }
    }
    if let Err(error) = remove_dir(dir, ended, Await::Aside)
        && error.kind() != io::ErrorKind::NotFound
    {
        tracing::warn!(dir = %dir.display(), %error, "a leftover keeper directory stays");
    }
}

/// Send `signal` to `pid` through a pidfd.
pub(crate) fn signal(pid: u32, signal: i32) {
    if let Some(fd) = pidfd(pid) {
        // SAFETY: the descriptor is a live pidfd this function owns, and the info may be null.
        let _ = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                fd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
    }
}

/// Wait until the keeper the launcher `launcher` starts in `dir` listens, or the launcher ended.
/// True once it listens.
pub(crate) fn wait_for_control(dir: &Path, launcher: &OwnedFd) -> bool {
    let control = dir.join("control");
    // SAFETY: inotify_init1 takes flags and returns a new descriptor or -1.
    let raw = unsafe { libc::inotify_init1(libc::IN_CLOEXEC) };
    if raw < 0 {
        return false;
    }
    // SAFETY: the kernel just returned this descriptor, owned by nobody else.
    let mut inotify = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(raw) });
    let Ok(path) = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()) else {
        return false;
    };
    // SAFETY: a live inotify descriptor and a NUL-terminated path.
    if unsafe { libc::inotify_add_watch(raw, path.as_ptr(), libc::IN_CREATE | libc::IN_MOVED_TO) }
        < 0
    {
        return false;
    }
    loop {
        // A socket is bound before it listens: one that refuses yet is waited for again.
        if tilo_files::native::socket::connect(&control).is_ok() {
            return true;
        }
        let mut fds = [
            libc::pollfd {
                fd: inotify.as_fd().as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
            libc::pollfd {
                fd: launcher.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            },
        ];
        let timeout = if control.exists() { 5 } else { 60_000 };
        // SAFETY: two live pollfds in a buffer of that length.
        let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, timeout) };
        if ready < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
        if fds[1].revents != 0 {
            return tilo_files::native::socket::connect(&control).is_ok();
        }
        if fds[0].revents != 0 {
            let mut events = [0u8; 4096];
            let _ = inotify.read(&mut events);
        } else if ready == 0 && !control.exists() {
            // A keeper that has not bound in a minute is not coming.
            return false;
        }
    }
}

/// Write `message`, with `fd` beside it when given.
fn send(stream: &UnixStream, message: &ToKeeper, fd: Option<&UnixStream>) -> io::Result<()> {
    let frame = wire::encode(message)?;
    let Some(fd) = fd else {
        return (&*stream).write_all(&frame);
    };
    let raw = fd.as_raw_fd();
    let space = usize::try_from(
        // SAFETY: CMSG_SPACE computes a size from a length.
        unsafe { libc::CMSG_SPACE(u32::try_from(std::mem::size_of::<i32>()).unwrap_or(4)) },
    )
    .unwrap_or(64);
    let mut control = vec![0u8; space];
    let mut iov = libc::iovec {
        iov_base: frame.as_ptr().cast_mut().cast(),
        iov_len: frame.len(),
    };
    // SAFETY: an all-zero msghdr is valid; the fields set below point at live buffers.
    let mut header: libc::msghdr = unsafe { std::mem::zeroed() };
    header.msg_iov = &raw mut iov;
    header.msg_iovlen = 1;
    header.msg_control = control.as_mut_ptr().cast();
    header.msg_controllen = space;
    // SAFETY: the control buffer holds one header with room for one descriptor.
    unsafe {
        let cmsg = libc::CMSG_FIRSTHDR(&raw const header);
        (*cmsg).cmsg_level = libc::SOL_SOCKET;
        (*cmsg).cmsg_type = libc::SCM_RIGHTS;
        (*cmsg).cmsg_len =
            libc::CMSG_LEN(u32::try_from(std::mem::size_of::<i32>()).unwrap_or(4)) as usize;
        std::ptr::copy_nonoverlapping(
            std::ptr::from_ref(&raw).cast::<u8>(),
            libc::CMSG_DATA(cmsg),
            std::mem::size_of::<i32>(),
        );
    }
    // SAFETY: the header points at the frame and the control buffer, both alive for the call.
    let sent = unsafe { libc::sendmsg(stream.as_raw_fd(), &raw const header, libc::MSG_NOSIGNAL) };
    let sent = usize::try_from(sent).map_err(|_| io::Error::last_os_error())?;
    // The rest of a frame that did not go at once follows without its descriptor.
    (&*stream).write_all(frame.get(sent..).unwrap_or_default())
}

/// What has come from a keeper and not been read yet.
#[derive(Default)]
pub(crate) struct Inbox {
    bytes: Vec<u8>,
    frames: std::collections::VecDeque<Vec<u8>>,
}

/// The next message from the keeper; `None` once the connection ended. A message this session
/// cannot read is passed over.
fn receive(stream: &UnixStream, inbox: &mut Inbox) -> io::Result<Option<FromKeeper>> {
    while let Some(frame) = receive_frame(stream, inbox)? {
        if let Ok(message) = serde_json::from_slice(&frame) {
            return Ok(Some(message));
        }
    }
    Ok(None)
}

/// The next frame from the keeper, as it came; `None` once the connection ended.
fn receive_frame(stream: &UnixStream, inbox: &mut Inbox) -> io::Result<Option<Vec<u8>>> {
    loop {
        if let Some(frame) = inbox.frames.pop_front() {
            return Ok(Some(frame));
        }
        let mut chunk = [0u8; 8192];
        let read = (&*stream).read(&mut chunk)?;
        if read == 0 {
            return Ok(None);
        }
        inbox
            .bytes
            .extend_from_slice(chunk.get(..read).unwrap_or_default());
        inbox.frames.extend(wire::take_frames(&mut inbox.bytes)?);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::AtomicBool;
    use std::time::Instant;

    use super::*;
    use crate::plan::{Kind, Planned};
    use crate::spawner::{Event, Spawner, Teardown};
    use tilo_test_env::SpawnUnderEnvLock as _;

    /// A display number no X server of this machine uses: the abstract name is machine-wide.
    fn unused_display() -> u32 {
        64_000_u32.saturating_add(std::process::id().checked_rem(1_000).unwrap_or(0))
    }

    #[test]
    fn a_dead_keepers_display_files_go_and_a_live_ones_stay() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".X11-unix")).unwrap();
        let number = unused_display();
        let lock = tmp.path().join(format!(".X{number}-lock"));
        let socket = tmp.path().join(".X11-unix").join(format!("X{number}"));
        let leave = |pid: u32| {
            std::fs::write(&lock, format!("{pid:>10}\n")).unwrap();
            std::fs::write(&socket, "").unwrap();
        };
        let mut ended = std::process::Command::new("/bin/true")
            .spawn_locked()
            .unwrap();
        let dead = ended.id();
        ended.wait().unwrap();

        // A live process's display stays.
        leave(std::process::id());
        assert!(!clear_dead_display(tmp.path(), number));
        assert!(lock.exists() && socket.exists());
        // A display whose abstract name is bound is held, whatever its lock says.
        leave(dead);
        {
            use std::os::linux::net::SocketAddrExt as _;
            let name = std::os::unix::net::SocketAddr::from_abstract_name(format!(
                "/tmp/.X11-unix/X{number}"
            ))
            .unwrap();
            let _holder = std::os::unix::net::UnixListener::bind_addr(&name).unwrap();
            assert!(!clear_dead_display(tmp.path(), number));
            assert!(lock.exists() && socket.exists());
        }
        // A killed keeper's display leaves nothing behind.
        assert!(clear_dead_display(tmp.path(), number));
        assert!(!lock.exists() && !socket.exists());
    }

    /// Whoever removes a keeper's directory clears what its display left once the keeper is gone:
    /// at once when it is gone already (a keeper that died, one ended at start), before the
    /// session goes on when it ends while the session waits (a logout), from a watch of its own
    /// when the session goes on (a force quit, the memory guard), and never while it lives.
    #[test]
    fn a_removed_keepers_display_files_go_once_the_keeper_is_gone() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".X11-unix")).unwrap();
        let number = unused_display().saturating_add(1);
        let lock = tmp.path().join(format!(".X{number}-lock"));
        let socket = tmp.path().join(".X11-unix").join(format!("X{number}"));
        let gone = || !lock.exists() && !socket.exists();
        // A keeper that lives for `seconds`, its display's lock naming it.
        let keeper = |seconds: &str| {
            let child = std::process::Command::new("sleep")
                .arg(seconds)
                .spawn_locked()
                .unwrap();
            std::fs::write(&lock, format!("{:>10}\n", child.id())).unwrap();
            std::fs::write(&socket, "").unwrap();
            let fd = pidfd(child.id()).unwrap();
            (child, fd)
        };
        let clear = |fd: OwnedFd, wait: Await| {
            clear_once_gone(tmp.path().to_owned(), number, Some(fd), wait);
        };

        // Gone already: its files go at once.
        let (mut child, fd) = keeper("60");
        child.kill().unwrap();
        child.wait().unwrap();
        clear(fd, Await::Aside);
        assert!(gone(), "a dead keeper's display files go at once");

        // Ending while the session waits for it: its files are gone before the session goes on.
        let (mut child, fd) = keeper("0.2");
        clear(fd, Await::Here(Duration::from_secs(30)));
        assert!(
            gone(),
            "a keeper that ended while waited for leaves nothing"
        );
        child.wait().unwrap();

        // Alive past the wait: its files stay, for it to remove as it ends.
        let (mut child, fd) = keeper("60");
        clear(fd, Await::Here(Duration::ZERO));
        assert!(!gone(), "a live keeper's display files stay");
        child.kill().unwrap();
        child.wait().unwrap();

        // Watched aside: nothing waits, and its files go once it is gone.
        let (mut child, fd) = keeper("60");
        clear(fd, Await::Aside);
        assert!(!gone(), "a live keeper's display files stay while it lives");
        child.kill().unwrap();
        child.wait().unwrap();
        tilo_test_env::wait_until("the watched keeper's display files go", gone);
    }

    #[test]
    fn an_end_the_keeper_saw_the_kernel_make_for_memory_says_so() {
        let end = end_of(wire::End::Signalled {
            signal: libc::SIGKILL,
            memory: true,
        });
        assert_eq!(end.memory(), Some(crate::spawner::Cause::Kernel));
        let killed = end_of(wire::End::Signalled {
            signal: libc::SIGKILL,
            memory: false,
        });
        assert_eq!(killed.memory(), None);
    }

    /// Not a test: the keeper process the others start, as `tilo-app-keeper` would run.
    #[test]
    fn keeper_process() {
        testing::serve_keeper().expect("the keeper ran");
    }

    /// A desktop on a thread of its own, for as long as the value lives.
    struct Desktop {
        connector: super::Desktop,
        stop: Arc<AtomicBool>,
        thread: Option<std::thread::JoinHandle<()>>,
    }

    impl Desktop {
        fn new() -> Self {
            let stop = Arc::new(AtomicBool::new(false));
            let (sender, connector) = std::sync::mpsc::channel();
            let stopped = Arc::clone(&stop);
            let thread = std::thread::spawn(move || {
                let mut compositor = tilo_compositor::Compositor::headless(Box::new(
                    tilo_compositor::policy::StackPolicy,
                ))
                .expect("a desktop");
                compositor
                    .add_output(&tilo_compositor::OutputConfig::new("HEADLESS-1", 640, 480))
                    .expect("an output");
                let _ = sender.send(compositor.client_connector());
                while !stopped.load(Ordering::Acquire) {
                    let _ = compositor.dispatch(Duration::from_millis(5));
                }
            });
            let connector = connector.recv().expect("the desktop's connector");
            Self {
                connector: Arc::new(move |pid| connector.connect_for(pid)),
                stop,
                thread: Some(thread),
            }
        }
    }

    impl Drop for Desktop {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Release);
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }

    /// A registry, the keeper program (this binary) and a launcher that starts the app behind it
    /// as `tilo-launcher machine-app --keeper` does.
    struct Rig {
        dir: tempfile::TempDir,
    }

    /// One run of the session: its spawner and its desktop, which goes with it.
    struct Spawned {
        spawner: Spawner,
        _desktop: Desktop,
    }

    impl std::ops::Deref for Spawned {
        type Target = Spawner;
        fn deref(&self) -> &Spawner {
            &self.spawner
        }
    }

    impl std::ops::DerefMut for Spawned {
        fn deref_mut(&mut self) -> &mut Spawner {
            &mut self.spawner
        }
    }

    impl Rig {
        fn new() -> Self {
            Self::with_app(&["/bin/sh", "-c", "exec sleep 1000"])
        }

        /// A rig whose app is `command`: its program, then its arguments.
        fn with_app(command: &[&str]) -> Self {
            let dir = tempfile::tempdir().expect("a directory");
            testing::keeper_program(&dir.path().join("keeper"), "kept::tests::keeper_process")
                .expect("the keeper");
            let command: Vec<String> = command.iter().map(|arg| (*arg).to_owned()).collect();
            testing::launcher_program(&dir.path().join("launcher"), &command)
                .expect("the launcher");
            Self { dir }
        }

        fn root(&self) -> PathBuf {
            self.dir.path().join("kept")
        }

        /// A keeper a service started itself, as the Linux VM's supervisor does: written into the
        /// registry, never launched by the session.
        fn service_keeper(&self) -> std::process::Child {
            let launch = KeptLaunch {
                version: tilo_app_keeper::registry::LAUNCH_VERSION,
                app: "legacy.linux".into(),
                title: "Linux apps".into(),
                workspace: String::new(),
                asked: None,
                window_identities: Vec::new(),
                service: true,
            };
            let dir =
                tilo_app_keeper::registry::new_dir(&self.root(), &launch).expect("a directory");
            std::process::Command::new(self.dir.path().join("keeper"))
                .arg("--dir")
                .arg(&dir)
                .arg("--launch")
                .arg(launch.encode())
                .args(["--", "/bin/sh", "-c", "exec sleep 1000"])
                .spawn_locked()
                .expect("the service starts its keeper")
        }

        /// Wait until every keeper in the registry listens.
        fn listening(&self) {
            let started = Instant::now();
            while !dirs(&self.root())
                .iter()
                .all(|dir| UnixStream::connect(dir.join("control")).is_ok())
            {
                assert!(
                    started.elapsed() < Duration::from_secs(20),
                    "a keeper never listened"
                );
                std::thread::park_timeout(Duration::from_millis(20));
            }
        }

        /// A run of the session's spawner.
        fn spawned(&self) -> Spawned {
            let desktop = Desktop::new();
            let mut spawner = Spawner::default();
            spawner.keep_behind(Keeping {
                keeper: self.dir.path().join("keeper"),
                root: self.root(),
                desktop: Arc::clone(&desktop.connector),
            });
            Spawned {
                spawner,
                _desktop: desktop,
            }
        }

        /// Start the kept app in `spawner`, and wait until it runs on the desktop: its process.
        fn start(&self, spawner: &mut Spawned) -> (u32, u64) {
            let planned = Planned {
                app: "org.example.Kept".into(),
                title: "Kept".into(),
                kind: Kind::Program,
                program: self.dir.path().join("launcher"),
                args: Vec::new(),
                env: BTreeMap::new(),
                window_identities: vec!["org.example.Kept".into()],
            };
            spawner
                .start_with(
                    &planned,
                    None,
                    &|_| {},
                    "workspace-1",
                    Some("org.example.Kept"),
                )
                .expect("starts");
            self.shown()
        }

        /// The one keeper's description, once its app runs on a desktop.
        fn shown(&self) -> (u32, u64) {
            let started = Instant::now();
            loop {
                if let Some((_, description)) = testing::described(&self.root()).first()
                    && description.attached
                    && let (Some(pid), Some(at)) = (description.app_pid, description.started_at)
                {
                    return (pid, at);
                }
                assert!(
                    started.elapsed() < Duration::from_secs(20),
                    "the kept app never showed"
                );
                std::thread::park_timeout(Duration::from_millis(20));
            }
        }
    }

    /// Wait for `count` events of `spawner`.
    fn events(spawner: &Spawned, count: usize) -> Vec<Event> {
        let started = Instant::now();
        let mut collected = Vec::new();
        while collected.len() < count {
            collected.extend(spawner.events());
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "no end was reported"
            );
            std::thread::park_timeout(Duration::from_millis(10));
        }
        collected
    }

    fn running(pid: u32, at: u64) -> bool {
        start_time(pid) == Some(at)
    }

    fn kill(pid: u32) {
        signal(pid, libc::SIGKILL);
    }

    tilo_test_env::scenario! {
        fn a_kept_app_stays_running_through_in_place_restarts_and_ends_at_logout;

        phase an_in_place_restart_leaves_it_running_and_the_next_run_takes_it_back;
        {
            let rig = Rig::new();
            let mut first = rig.spawned();
            let (pid, at) = rig.start(&mut first);
            first.end_running(Teardown::InPlaceRestart);
            drop(first);
            assert!(running(pid, at), "the app outlives the desktop that started it");

            let mut second = rig.spawned();
            let adopted = second.adopt_kept();
            assert_eq!(adopted.len(), 1, "the kept app is taken back");
            let kept = adopted.first().expect("one");
            assert_eq!(kept.launch.app, "org.example.Kept");
            assert_eq!(kept.launch.workspace, "workspace-1");
            assert_eq!(kept.launch.asked.as_deref(), Some("org.example.Kept"));
            second.attach_kept(kept.pid);
            assert_eq!(rig.shown(), (pid, at), "the same process shows on the new desktop");

            kill(pid);
            let ended = events(&second, 1);
            assert_eq!(ended.first().map(|event| event.id), Some(kept.id));
            assert_eq!(ended.first().map(|event| event.end.clone()), Some(End::signalled(9)));
            assert!(dirs(&rig.root()).is_empty(), "its directory goes with it");
        }

        phase it_survives_two_restarts_in_a_row_and_a_logout_ends_it;
        {
            let rig = Rig::new();
            let mut first = rig.spawned();
            let (pid, at) = rig.start(&mut first);
            first.end_running(Teardown::InPlaceRestart);
            drop(first);
            let mut second = rig.spawned();
            let [kept] = second.adopt_kept().try_into().expect("one kept app");
            second.attach_kept(kept.pid);
            rig.shown();
            // The new desktop never became ready: the update is rolled back, in place again.
            second.end_running(Teardown::InPlaceRestart);
            drop(second);
            let mut third = rig.spawned();
            let [kept] = third.adopt_kept().try_into().expect("one kept app");
            third.attach_kept(kept.pid);
            assert_eq!(rig.shown(), (pid, at), "the same process after both");

            third.end_running(Teardown::Logout);
            tilo_test_env::exited("the kept app at logout", pid);
            assert!(dirs(&rig.root()).is_empty(), "a logout removes the keeper's directory");
        }

        phase an_app_that_ended_while_no_desktop_was_there_is_reported;
        {
            let rig = Rig::new();
            let mut first = rig.spawned();
            let (pid, _) = rig.start(&mut first);
            first.end_running(Teardown::InPlaceRestart);
            drop(first);
            kill(pid);
            let dir = dirs(&rig.root()).into_iter().next().expect("the keeper's directory");
            let started = Instant::now();
            while recorded_end(&dir).is_none() {
                assert!(started.elapsed() < Duration::from_secs(20), "the keeper never recorded the end");
                std::thread::park_timeout(Duration::from_millis(20));
            }
            let mut second = rig.spawned();
            assert!(second.adopt_kept().is_empty(), "nothing runs to take back");
            let ended = events(&second, 1);
            assert_eq!(ended.first().map(|event| event.app.as_str()), Some("org.example.Kept"));
            assert_eq!(ended.first().map(|event| event.end.clone()), Some(End::signalled(9)));
            assert!(dirs(&rig.root()).is_empty());
        }

        phase a_start_that_follows_no_restart_ends_what_was_left;
        {
            let rig = Rig::new();
            let mut first = rig.spawned();
            let (pid, _) = rig.start(&mut first);
            first.end_running(Teardown::InPlaceRestart);
            drop(first);
            let second = rig.spawned();
            second.clear_kept();
            tilo_test_env::exited("a leftover kept app", pid);
            assert!(dirs(&rig.root()).is_empty(), "and its directory is gone");
        }

        phase a_keeper_of_another_version_is_left_running_and_not_taken;
        {
            let rig = Rig::new();
            let dir = rig.root().join("older");
            std::fs::create_dir_all(&dir).expect("a keeper directory");
            let listener = tilo_files::native::socket::bind(&dir.join("control")).expect("bound");
            let answering = std::thread::spawn(move || {
                let (stream, _) = listener.accept().expect("the session connects");
                // Heard before it is answered, as a keeper does: a reply to a hello not yet sent
                // closes the connection under the session, which reads as a keeper gone.
                let mut hello = [0u8; 1];
                (&stream).read_exact(&mut hello).expect("the session says hello");
                let frame = wire::encode(&FromKeeper::VersionMismatch { version: 0 }).expect("a frame");
                (&stream).write_all(&frame).expect("answered");
            });
            let mut spawned = rig.spawned();
            assert!(spawned.adopt_kept().is_empty(), "an older keeper is not taken");
            let _ = answering.join();
            assert!(dir.exists(), "nor is it removed");
            assert!(spawned.events().is_empty(), "nor reported as ended");
        }

        phase a_services_keeper_is_shown_at_a_fresh_start_and_never_ended;
        {
            let rig = Rig::new();
            let mut keeper = rig.service_keeper();
            rig.listening();
            let mut spawned = rig.spawned();
            spawned.clear_kept();
            let adopted = spawned.adopt_kept();
            assert_eq!(adopted.len(), 1, "a fresh start takes up a service's keeper too");
            assert!(adopted.first().is_some_and(|kept| kept.launch.service));
            spawned.attach_kept(adopted[0].pid);
            let (pid, at) = rig.shown();
            spawned.end_running(Teardown::Logout);
            drop(spawned);
            assert!(running(pid, at), "a logout leaves the service's app to the service");
            assert_eq!(dirs(&rig.root()).len(), 1, "and its directory");
            let _ = keeper.kill();
            let _ = keeper.wait();
            kill(pid);
            let mut next = rig.spawned();
            assert!(next.adopt_kept().is_empty(), "a keeper its service lost is not taken up");
            assert!(dirs(&rig.root()).is_empty(), "and its directory goes");
            assert!(next.events().is_empty(), "nor is its end reported");
        }

        phase a_services_keeper_that_appears_later_is_shown_and_its_end_not_reported;
        {
            let rig = Rig::new();
            let mut spawned = rig.spawned();
            spawned.clear_kept();
            assert!(spawned.adopt_kept().is_empty());
            let mut keeper = rig.service_keeper();
            let started = Instant::now();
            let adopted = loop {
                let adopted = spawned.adopt_appeared();
                if !adopted.is_empty() {
                    break adopted;
                }
                assert!(started.elapsed() < Duration::from_secs(20), "the keeper was never taken up");
                std::thread::park_timeout(Duration::from_millis(20));
            };
            assert_eq!(adopted.len(), 1);
            let (pid, _) = rig.shown();
            assert!(spawned.adopt_appeared().is_empty(), "taken up once");
            kill(pid);
            let _ = keeper.wait();
            std::thread::park_timeout(Duration::from_millis(200));
            assert!(spawned.events().is_empty(), "the service's app ending is the service's to handle");
        }

        phase an_app_behind_an_outdated_keeper_is_held_through_restarts_and_restart_ends_it;
        {
            let rig = Rig::new();
            let mut first = rig.spawned();
            let (pid, at) = rig.start(&mut first);
            first.end_running(Teardown::InPlaceRestart);
            drop(first);
            let dir = dirs(&rig.root()).into_iter().next().expect("the keeper's directory");
            testing::age(&dir, true).expect("aged");

            let mut second = rig.spawned();
            let adopted = second.adopt_kept();
            assert_eq!(adopted.len(), 1, "taken as a launch of this session");
            let kept = adopted.first().expect("one");
            assert!(kept.outdated, "one this session cannot show");
            assert!(kept.windows.is_empty());
            assert_eq!(kept.launch.app, "org.example.Kept");
            assert!(running(pid, at), "its app goes on, with whatever it holds");
            assert!(dir.join("control").exists(), "and its keeper with it");
            // A desktop that restarts in place again leaves it as it is.
            second.end_running(Teardown::InPlaceRestart);
            drop(second);
            assert!(running(pid, at));

            // A keeper from before the `app` file: its app is found as its child.
            std::fs::remove_file(dir.join("app")).expect("the file goes");
            let mut third = rig.spawned();
            let adopted = third.adopt_kept();
            let kept = adopted.first().expect("taken again");
            assert!(kept.outdated);
            assert!(third.restart_outdated(kept.id), "Restart ends it");
            let ended = events(&third, 1);
            assert_eq!(ended.first().map(|event| event.id), Some(kept.id));
            assert!(!running(pid, at), "the old app ended");
            assert!(dirs(&rig.root()).is_empty(), "its directory goes with it");
        }

        phase an_outdated_keeper_from_before_the_hold_is_reported_once_its_app_ended;
        {
            let rig = Rig::new();
            let mut first = rig.spawned();
            let (pid, at) = rig.start(&mut first);
            first.end_running(Teardown::InPlaceRestart);
            drop(first);
            let dir = dirs(&rig.root()).into_iter().next().expect("the keeper's directory");
            testing::age(&dir, false).expect("aged");
            let mut second = rig.spawned();
            let adopted = second.adopt_kept();
            let kept = adopted.first().expect("taken");
            assert!(kept.outdated);
            assert!(running(pid, at));
            // Its keeper ends it once no desktop came back in time.
            signal(pid, libc::SIGTERM);
            let ended = events(&second, 1);
            assert_eq!(ended.first().map(|event| event.id), Some(kept.id));
            assert_eq!(ended.first().map(|event| event.end.clone()), Some(End::signalled(libc::SIGTERM)));
        }

        phase a_logout_ends_an_app_behind_an_outdated_keeper;
        {
            let rig = Rig::new();
            let mut first = rig.spawned();
            let (pid, _) = rig.start(&mut first);
            first.end_running(Teardown::InPlaceRestart);
            drop(first);
            let dir = dirs(&rig.root()).into_iter().next().expect("the keeper's directory");
            testing::age(&dir, true).expect("aged");
            let mut second = rig.spawned();
            assert_eq!(second.adopt_kept().len(), 1);
            second.end_running(Teardown::Logout);
            tilo_test_env::exited("the outdated kept app at logout", pid);
            assert!(dirs(&rig.root()).is_empty());
        }

        phase a_services_keeper_from_before_an_update_outlives_a_logout_and_restart_ends_only_it;
        {
            let rig = Rig::new();
            let mut keeper = rig.service_keeper();
            rig.listening();
            let mut before = rig.spawned();
            before.clear_kept();
            let [kept] = before.adopt_kept().try_into().expect("one kept app");
            before.attach_kept(kept.pid);
            let (pid, at) = rig.shown();
            before.end_running(Teardown::InPlaceRestart);
            drop(before);
            let dir = dirs(&rig.root()).into_iter().next().expect("its directory");
            testing::age(&dir, true).expect("aged");
            let mut spawned = rig.spawned();
            let adopted = spawned.adopt_kept();
            let kept = adopted.first().expect("taken up");
            assert!(kept.outdated && kept.launch.service);
            spawned.end_running(Teardown::Logout);
            drop(spawned);
            assert!(
                matches!(keeper.try_wait(), Ok(None)),
                "a logout leaves the service's keeper to the service"
            );
            let mut next = rig.spawned();
            let adopted = next.adopt_kept();
            let kept = adopted.first().expect("taken up again");
            assert!(next.restart_outdated(kept.id), "Restart ends it");
            let ended = events(&next, 1);
            assert_eq!(ended.first().map(|event| event.id), Some(kept.id));
            // Its service starts a current keeper once this one ended.
            keeper.wait().expect("the old keeper ended");
            assert!(!running(pid, at), "and its app with it");
            assert!(dirs(&rig.root()).is_empty(), "its directory goes with it");
        }

        phase a_logout_ends_a_kept_app_the_same_run_started;
        {
            let rig = Rig::new();
            let mut first = rig.spawned();
            let (pid, _) = rig.start(&mut first);
            drop(first);
            tilo_test_env::exited("the kept app its session no longer runs", pid);
        }
    }

    /// An app that prints on its standard error as it starts and when it is signalled, so what it
    /// wrote before, during and after a restart can be told apart.
    const PRINTS: &str = "echo before >&2; trap 'echo during >&2' USR1; \
                          trap 'echo after >&2' USR2; while :; do sleep 0.05; done";

    /// Whether the keeper's file for `stream` holds `text`: the app wrote it, and the keeper read
    /// it.
    fn kept_says(dir: &Path, stream: &str, text: &str) -> bool {
        std::fs::read(tilo_app_keeper::output::path(dir, stream)).is_ok_and(|bytes| {
            bytes
                .windows(text.len())
                .any(|window| window == text.as_bytes())
        })
    }

    /// Wait until the keeper's file for `stream` holds `text`: the app wrote it and the keeper
    /// read it, whatever a desktop has read yet.
    fn until_kept(dir: &Path, stream: &str, text: &str) {
        let started = Instant::now();
        while !kept_says(dir, stream, text) {
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "the keeper never kept {text:?}"
            );
            std::thread::park_timeout(Duration::from_millis(10));
        }
    }

    /// What a kept app wrote before, during and after an in-place restart is what the session
    /// that follows keeps, in the order it wrote it, up to its end.
    #[test]
    fn what_a_kept_app_wrote_reaches_the_session_that_follows_it() {
        let rig = Rig::with_app(&["/bin/sh", "-c", PRINTS]);
        let mut first = rig.spawned();
        let (pid, at) = rig.start(&mut first);
        let dir = dirs(&rig.root())
            .into_iter()
            .next()
            .expect("the keeper's directory");
        until_kept(&dir, "stderr", "before");
        first.end_running(Teardown::InPlaceRestart);
        drop(first);
        assert!(
            running(pid, at),
            "the app outlives the desktop that started it"
        );

        // While no desktop is there, the app prints, and the keeper keeps it.
        signal(pid, libc::SIGUSR1);
        until_kept(&dir, "stderr", "during");

        let mut second = rig.spawned();
        assert_eq!(second.adopt_kept().len(), 1, "the kept app is taken back");
        signal(pid, libc::SIGUSR2);
        until_kept(&dir, "stderr", "after");

        kill(pid);
        let ended = events(&second, 1);
        assert_eq!(
            ended.first().expect("its end").said.stderr,
            ["before", "during", "after"],
            "the session that follows keeps what the app wrote around the restart, in order"
        );
        assert!(dirs(&rig.root()).is_empty(), "its directory goes with it");
    }
}
