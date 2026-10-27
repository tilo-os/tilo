//! Rootless Xwayland, one per workspace, via `xwayland-satellite`.
//!
//! Tilo remains a Wayland compositor and does not grow an in-process XWM. The
//! satellite is itself a Wayland client and maps each legacy X11 window to an
//! ordinary `xdg_toplevel`, so X11 software participates in the same layout.
//! Failure is non-fatal: native Wayland software remains usable.
//!
//! X11 has no client isolation: any client of a display can read every other
//! client's input, windows and selections. One X server therefore never spans
//! two workspaces. Each workspace that opens a host app gets its own display
//! number, its own MIT-MAGIC-COOKIE authority and its own satellite, and the
//! DISPLAY/XAUTHORITY pair reaches only launches into that workspace.
//!
//! Displays are lazy. The compositor binds the X11 socket itself and spawns
//! satellite with `-listenfd` when the first client connects, so a workspace
//! that never runs an X11 app never runs an X server. Satellite is stopped
//! again when its workspace closes or no X client has been connected for
//! [`IDLE_TIMEOUT`]; the socket stays, and the next client starts a fresh one.
//!
//! Binding a display touches the disk (the shared socket directory, the lock
//! file, the socket, the authority file) and so does removing one. The
//! compositor thread does neither: the `xwayland-displays` thread binds a
//! display when the active workspace or a launch asks for one, and removes
//! displays whose workspace closed. Nor does it run the bridges: the
//! `xwayland-bridges` thread waits on the sockets, starts and reaps
//! satellite and `setxkbmap`, reads the kernel's socket tables for idle
//! servers and writes the composer's authority file. The compositor thread
//! only says which workspaces are open and exports the active one's display.
//!
//! Satellite hands Xwayland native output resolutions and announces the
//! output scale over XSETTINGS, which GTK follows. Toolkits that size from
//! the `Xft.dpi` resource instead (Qt, Xft and Xt clients, and the composing
//! input method's own candidate window) would stay at 96 DPI on a
//! high-density output, so each new server also gets satellite's DPI in its
//! root `RESOURCE_MANAGER` before the composer is asked to connect. The
//! client whose connection started the server may read resources before
//! that.

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::fd::{AsFd as _, AsRawFd, OwnedFd};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, mpsc};
use std::time::{Duration, Instant};

use smithay::reexports::calloop;

use super::backend::drm::input_config::KeyboardSettings;
use super::xwayland_composer::ComposerClients;
use crate::shell::host_apps::X11Session;
use tilo_lock::MutexRecover as _;

const DISPLAY_ATTEMPTS: usize = 64;
/// Restarting a compatibility bridge forever turns one crashing helper into a
/// busy loop. Three quick exits in a row give a transient failure time to
/// settle while making the eventual unavailable state deterministic.
const MAX_RAPID_EXITS: u8 = 3;
/// A bridge that stayed up this long proved itself healthy; its exit starts a
/// fresh budget rather than counting towards [`MAX_RAPID_EXITS`].
const STABLE_UPTIME: Duration = Duration::from_secs(30);
/// How long a running X server may have no connected client before it is
/// stopped. Long enough that an app restarting itself (installers, launchers
/// that re-exec) finds the server still up.
const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Wine (and similar X11 runtimes) keep the X socket after the last
/// window is gone. Two seconds is enough for a legitimate re-map and
/// short enough that the leftover process does not sit around.
const WINDOWLESS_TIMEOUT: Duration = Duration::from_secs(2);
const KEYMAP_UPDATE_TIMEOUT: Duration = Duration::from_secs(2);
/// How often the bridge thread looks at running bridges: whether they
/// exited, and whether they are idle. Reading the kernel's socket table once
/// a second is plenty for an idle stop.
const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(1);
/// How often it looks while a bridge settles: its DPI being published, a
/// keymap being applied.
const SETTLE_CHECK_INTERVAL: Duration = Duration::from_millis(100);
/// How long a freshly started X server gets before the composing input
/// method is asked to connect, so the request lands on a server that is
/// already accepting rather than stalling the composer in the handshake.
const COMPOSER_CONNECT_DELAY: Duration = Duration::from_secs(1);
/// How long satellite gets to announce its scale over XSETTINGS.
const XSETTINGS_WAIT: Duration = Duration::from_secs(2);
/// How long publishing the DPI may hold back the composer before it is asked
/// to connect anyway, and the publisher is abandoned.
const RESOURCES_TIMEOUT: Duration = Duration::from_secs(3);
/// The bridge's lifecycle lines go to the session log by default
/// (`tilo_startup_metrics::log_directives`): they time an X11 app's open.
const XWAYLAND_LOG: &str = tilo_startup_metrics::XWAYLAND_TARGET;
/// How long a launch waits for the display thread to bind its workspace's
/// display. Binding takes milliseconds; past this the disk is wedged, and the
/// launch goes ahead Wayland-only as with any other X11 failure.
const BIND_WAIT: Duration = Duration::from_secs(5);

/// Every workspace display of this session, shared by the host-launch path
/// (which creates displays) and the compositor-thread supervisor (which
/// starts, stops and removes them). `None` outside a running compositor, so
/// headless tools and tests never bind X11 sockets.
static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    displays: None,
    wake: None,
    bridges: None,
});

struct Registry {
    displays: Option<Displays>,
    /// Wakes the compositor loop: a display the active workspace waits for
    /// was bound, or a workspace's bridge kept failing.
    wake: Option<calloop::ping::Ping>,
    /// Wakes the bridge thread, so it watches a display just bound.
    bridges: Option<Arc<BridgeWake>>,
}

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    REGISTRY.lock_recover()
}

/// The X server the satellite with process id `pid` bridges: its socket and
/// MIT-MAGIC-COOKIE-1. `None` when `pid` is no bridge of this session. Takes
/// the registry, which the bridge thread may hold for a while: call it off
/// the compositor's loop.
pub(crate) fn bridged_display(pid: u32) -> Option<(PathBuf, [u8; 16])> {
    let registry = registry();
    registry
        .displays
        .as_ref()?
        .workspaces
        .values()
        .find(|display| {
            display
                .bridge
                .as_ref()
                .is_some_and(|bridge| bridge.child.id() == pid)
        })
        .map(|display| (display.socket.clone(), display.cookie))
}

/// Signalled, with [`REGISTRY`], whenever the display thread delivers a
/// display it bound (or failed to).
static DELIVERED: Condvar = Condvar::new();

/// The X11 connection for a host launch into `workspace`, its display bound
/// on first use. `None` when X11 is unavailable there; the launch then
/// proceeds Wayland-only.
///
/// A display not bound yet is bound by the display thread, and this waits
/// for it (at most [`BIND_WAIT`]): call it from a launch's own thread, never
/// from the compositor's.
pub(crate) fn session_for_workspace(workspace: &str) -> Option<X11Session> {
    let started = Instant::now();
    let mut registry = registry();
    loop {
        let displays = registry.displays.as_mut()?;
        match displays.bound(workspace) {
            Ok(Some(session)) => return Some(session),
            Ok(None) => {}
            Err(error) => {
                tracing::warn!(target: XWAYLAND_LOG, %error, "X11 apps unavailable in this workspace");
                return None;
            }
        }
        if !displays.prepare(workspace) {
            tracing::warn!(target: XWAYLAND_LOG, "X11 apps unavailable: no thread binds displays");
            return None;
        }
        let waited = started.elapsed();
        if waited >= BIND_WAIT {
            tracing::warn!(
                target: XWAYLAND_LOG,
                wait_ms = BIND_WAIT.as_millis(),
                "X11 display not bound in time; launching Wayland-only"
            );
            return None;
        }
        registry = DELIVERED
            .wait_timeout(registry, BIND_WAIT.saturating_sub(waited))
            .map_or_else(|poisoned| poisoned.into_inner().0, |(guard, _)| guard);
    }
}

/// What the compositor thread hands the display thread: the work on a
/// display that touches the disk.
enum DisplayWork {
    /// Bind a display for this workspace and deliver it.
    Bind(String),
    /// Stop and remove a display whose workspace no longer uses it.
    Retire(Box<WorkspaceDisplay>),
}

/// Start the thread that binds and retires displays under `paths`, handing
/// each bound display (or why it could not be bound) to `deliver`.
fn spawn_display_thread(
    paths: Paths,
    deliver: impl Fn(String, Result<WorkspaceDisplay, String>) + Send + 'static,
) -> Option<mpsc::Sender<DisplayWork>> {
    let (sender, work) = mpsc::channel();
    std::thread::Builder::new()
        .name("xwayland-displays".into())
        .spawn(move || {
            for job in work {
                match job {
                    DisplayWork::Bind(workspace) => {
                        deliver(workspace, WorkspaceDisplay::bind(&paths));
                    }
                    DisplayWork::Retire(display) => drop(display),
                }
            }
        })
        .inspect_err(|error| {
            tracing::warn!(target: XWAYLAND_LOG, %error, "X11 apps unavailable: cannot start the display thread");
        })
        .ok()?;
    Some(sender)
}

/// Hand a display the display thread bound to the session's displays, and
/// wake whoever waits for it: a launch, and the compositor loop, which
/// watches the new socket.
fn deliver_to_session(workspace: String, bound: Result<WorkspaceDisplay, String>) {
    let mut registry = registry();
    let Registry {
        displays,
        wake,
        bridges,
    } = &mut *registry;
    let refused = match displays.as_mut() {
        Some(displays) => displays.adopt(workspace, bound),
        None => bound.ok(),
    };
    let wake = wake.clone();
    let bridges = bridges.clone();
    drop(registry);
    DELIVERED.notify_all();
    if let Some(wake) = wake {
        wake.ping();
    }
    if let Some(bridges) = bridges {
        bridges.wake();
    }
    // Still on the display thread: removing it touches the disk.
    drop(refused);
}

/// Where the X11 sockets, lock files and private authorities live.
#[derive(Clone)]
struct Paths {
    /// The shared X11 socket directory, `/tmp/.X11-unix`.
    sockets: PathBuf,
    /// Where X servers claim a display number, `/tmp`.
    locks: PathBuf,
    /// This login's private directory for authority files.
    runtime: PathBuf,
}

impl Paths {
    fn session() -> Result<Self, String> {
        let runtime_root = tilo_paths::xdg::runtime_dir()
            .ok_or_else(|| "XDG_RUNTIME_DIR is not an absolute path".to_string())?;
        Ok(Self {
            sockets: PathBuf::from("/tmp/.X11-unix"),
            locks: PathBuf::from("/tmp"),
            runtime: runtime_root.join("tilo-session/xwayland"),
        })
    }
}

/// The per-workspace displays and their bridges. Pure state plus process
/// management; the compositor loop integration lives in the supervisor.
pub(crate) struct Displays {
    paths: Paths,
    /// The satellite binary, or why X11 is unavailable on this machine.
    satellite: Result<PathBuf, String>,
    workspaces: HashMap<String, WorkspaceDisplay>,
    /// Workspaces whose bridge kept crashing. They stay Wayland-only until
    /// the workspace closes, so a broken helper cannot restart forever.
    unavailable: HashSet<String>,
    /// Workspaces whose display the display thread is binding.
    preparing: HashSet<String>,
    /// Why a workspace's display could not be bound. It is not tried again
    /// until the workspace closes.
    bind_failed: HashMap<String, String>,
    /// The display thread. Without one (tests) displays are retired in
    /// place and none is bound on request.
    worker: Option<mpsc::Sender<DisplayWork>>,
    /// Workspaces whose bridge kept failing, for the compositor thread to
    /// announce.
    exhausted: Vec<String>,
}

pub(crate) struct WorkspaceDisplay {
    number: u16,
    listener: UnixListener,
    socket: PathBuf,
    /// Device and inode of the socket file this display bound, so teardown
    /// never unlinks a socket some later server placed at the same path.
    socket_identity: (u64, u64),
    lock: PathBuf,
    authority: PathBuf,
    /// The MIT-MAGIC-COOKIE-1 in [`Self::authority`].
    cookie: [u8; 16],
    bridge: Option<Bridge>,
    rapid_exits: u8,
    /// When the running bridge entered its current idle phase.
    idle_since: Option<Instant>,
    /// The timeout for that phase: no clients or previously mapped clients.
    idle_limit: Option<Duration>,
    idle_checked: Option<Instant>,
    /// Satellite has mapped a toplevel since clients were last observed absent.
    saw_toplevel: bool,
    /// The composer process group already asked to serve the running
    /// server, so each server and composer pair is asked once.
    composer_asked: Option<libc::pid_t>,
}

struct Bridge {
    child: Child,
    started: Instant,
    /// The keymap the X server currently has.
    keyboard: KeyboardSettings,
    keyboard_update: Option<KeyboardUpdate>,
    /// The newest settings observed while `setxkbmap` is still applying an
    /// earlier snapshot. Only one helper may touch the X server at a time,
    /// but the final state must still converge without another settings
    /// file write to wake the watcher.
    pending_keyboard: Option<KeyboardSettings>,
    /// Still publishing `Xft.dpi` to the new server.
    resources: Option<ResourcePublish>,
}

/// Mirrors satellite's XSETTINGS `Xft/DPI` into the root window's
/// `RESOURCE_MANAGER`, off the compositor thread: a starting X server may
/// take a while to accept.
struct ResourcePublish {
    worker: std::thread::JoinHandle<Result<u32, String>>,
    /// The worker's connection, shut down to unblock it when abandoned.
    stream: UnixStream,
}

impl ResourcePublish {
    fn start(display: &WorkspaceDisplay) -> Result<Self, String> {
        let stream = UnixStream::connect(&display.socket)
            .map_err(|error| format!("connecting to {}: {error}", display.socket.display()))?;
        let worker_stream = stream
            .try_clone()
            .map_err(|error| format!("duplicating X11 connection: {error}"))?;
        let cookie = display.cookie;
        let worker = std::thread::Builder::new()
            .name("xwayland-resources".into())
            .spawn(move || publish_font_dpi(worker_stream, cookie))
            .map_err(|error| format!("starting resource publisher: {error}"))?;
        Ok(Self { worker, stream })
    }

    fn abandon(self) {
        let _ = self.stream.shutdown(std::net::Shutdown::Both);
    }
}

struct KeyboardUpdate {
    child: Child,
    requested: KeyboardSettings,
    started: Instant,
}

/// Starts a satellite for a display that just received its first client.
type Spawn<'a> = dyn FnMut(&WorkspaceDisplay) -> Result<(Child, KeyboardSettings, Option<ResourcePublish>), String>
    + 'a;

impl Displays {
    fn new(paths: Paths, satellite: Result<PathBuf, String>) -> Self {
        Self {
            paths,
            satellite,
            workspaces: HashMap::new(),
            unavailable: HashSet::new(),
            preparing: HashSet::new(),
            bind_failed: HashMap::new(),
            worker: None,
            exhausted: Vec::new(),
        }
    }

    /// Bind and retire this session's displays on a thread of their own,
    /// handing each bound display to `deliver`.
    fn start_display_thread(
        &mut self,
        deliver: impl Fn(String, Result<WorkspaceDisplay, String>) + Send + 'static,
    ) {
        self.worker = spawn_display_thread(self.paths.clone(), deliver);
    }

    /// The display of `workspace` when it is bound; `Ok(None)` when it is
    /// not yet ([`Self::prepare`] asks for it), an error when X11 is
    /// unavailable there.
    fn bound(&self, workspace: &str) -> Result<Option<X11Session>, String> {
        if let Some(display) = self.workspaces.get(workspace) {
            return Ok(Some(display.session()));
        }
        if let Err(reason) = &self.satellite {
            return Err(reason.clone());
        }
        if self.unavailable.contains(workspace) {
            return Err("the X11 compatibility service kept exiting in this workspace".into());
        }
        if let Some(reason) = self.bind_failed.get(workspace) {
            return Err(reason.clone());
        }
        Ok(None)
    }

    /// Have the display thread bind `workspace`'s display, once however
    /// often it is asked. `false` when there is no display thread.
    fn prepare(&mut self, workspace: &str) -> bool {
        let Some(worker) = &self.worker else {
            return false;
        };
        if self.workspaces.contains_key(workspace) || self.preparing.contains(workspace) {
            return true;
        }
        if worker
            .send(DisplayWork::Bind(workspace.to_string()))
            .is_err()
        {
            return false;
        }
        self.preparing.insert(workspace.to_string());
        true
    }

    /// Take in the display the display thread bound for `workspace`, or why
    /// it could not. A display the workspace no longer takes (it has one, or
    /// X11 became unavailable there) comes back for the caller to retire.
    fn adopt(
        &mut self,
        workspace: String,
        bound: Result<WorkspaceDisplay, String>,
    ) -> Option<WorkspaceDisplay> {
        self.preparing.remove(&workspace);
        match bound {
            Ok(display) => {
                if !matches!(self.bound(&workspace), Ok(None)) {
                    return Some(display);
                }
                let session = display.session();
                tracing::info!(
                    target: XWAYLAND_LOG,
                    display = %session.display,
                    "X11 display ready for a workspace"
                );
                self.workspaces.insert(workspace, display);
                None
            }
            Err(error) => {
                tracing::warn!(target: XWAYLAND_LOG, %error, "X11 apps unavailable in this workspace");
                self.bind_failed.insert(workspace, error);
                None
            }
        }
    }

    /// Stop and remove `display` on the display thread (in place without
    /// one).
    fn retire(&self, display: WorkspaceDisplay) {
        if let Some(worker) = &self.worker
            && let Err(mpsc::SendError(DisplayWork::Retire(display))) =
                worker.send(DisplayWork::Retire(Box::new(display)))
        {
            drop(display);
        }
    }

    /// Bind `workspace`'s display on this thread, for tests that drive
    /// displays without a display thread. The second value says whether it
    /// was just created.
    #[cfg(test)]
    fn session_for(&mut self, workspace: &str) -> Result<(X11Session, bool), String> {
        if let Some(session) = self.bound(workspace)? {
            return Ok((session, false));
        }
        let display = WorkspaceDisplay::bind(&self.paths)?;
        let session = display.session();
        self.workspaces.insert(workspace.to_string(), display);
        Ok((session, true))
    }

    /// Remove every display whose workspace closed, stopping its X server.
    fn retain_workspaces(&mut self, live: &HashSet<String>) {
        let closed: Vec<String> = self
            .workspaces
            .keys()
            .filter(|workspace| !live.contains(*workspace))
            .cloned()
            .collect();
        for workspace in closed {
            if let Some(display) = self.workspaces.remove(&workspace) {
                self.retire(display);
            }
        }
        self.unavailable
            .retain(|workspace| live.contains(workspace));
        self.bind_failed
            .retain(|workspace, _| live.contains(workspace));
    }

    /// Advance every display: reap exited bridges, stop idle ones and start
    /// one for each display with a waiting client. Returns the workspaces
    /// that just became unavailable.
    #[cfg(test)]
    fn poll(&mut self, now: Instant, spawn: &mut Spawn<'_>) -> Vec<String> {
        self.poll_with(
            now,
            spawn,
            &mut ComposerClients::for_group(None),
            &HashSet::new(),
        )
    }

    /// [`Self::poll`] with the composing input method's view: its
    /// connections do not count towards keeping a server up, and it is
    /// asked to serve each server that is running for a real client.
    fn poll_with(
        &mut self,
        now: Instant,
        spawn: &mut Spawn<'_>,
        composer: &mut ComposerClients,
        mapped_satellites: &HashSet<u32>,
    ) -> Vec<String> {
        let mut exhausted = Vec::new();
        for (workspace, display) in &mut self.workspaces {
            if display.poll(now, spawn, composer, mapped_satellites) {
                exhausted.push(workspace.clone());
            }
        }
        for workspace in &exhausted {
            if let Some(display) = self.workspaces.remove(workspace) {
                self.retire(display);
            }
            self.unavailable.insert(workspace.clone());
        }
        exhausted
    }

    /// One turn of the bridge thread: retire the displays of workspaces that
    /// closed (once the compositor said which are open), then
    /// [`Self::poll_with`].
    fn advance(
        &mut self,
        live: Option<&HashSet<String>>,
        now: Instant,
        spawn: &mut Spawn<'_>,
        composer: &mut ComposerClients,
        mapped_satellites: &HashSet<u32>,
    ) -> Vec<String> {
        if let Some(live) = live {
            self.retain_workspaces(live);
        }
        self.poll_with(now, spawn, composer, mapped_satellites)
    }

    /// The sockets of displays without a running bridge, where a client
    /// starts one. Duplicated, so a display retired meanwhile cannot close
    /// one under the bridge thread's wait.
    fn waiting_listeners(&self) -> Vec<OwnedFd> {
        self.workspaces
            .values()
            .filter(|display| display.waits_for_client())
            .filter_map(|display| display.listener.as_fd().try_clone_to_owned().ok())
            .collect()
    }

    /// How soon running bridges need looking at again: often while one
    /// settles (its DPI, a keymap), once per idle check otherwise, never
    /// without one.
    fn next_check(&self) -> Option<Duration> {
        self.workspaces
            .values()
            .filter_map(|display| display.bridge.as_ref())
            .map(|bridge| {
                if bridge.resources.is_some() || bridge.keyboard_update.is_some() {
                    SETTLE_CHECK_INTERVAL
                } else {
                    IDLE_CHECK_INTERVAL
                }
            })
            .min()
    }

    fn reload_keyboard_settings(&mut self, keyboard: &KeyboardSettings) {
        for display in self.workspaces.values_mut() {
            display.request_keyboard(keyboard, Path::new("/usr/bin/setxkbmap"));
        }
    }
}

impl WorkspaceDisplay {
    fn bind(paths: &Paths) -> Result<Self, String> {
        ensure_socket_dir(&paths.sockets)?;
        fs::create_dir_all(&paths.runtime)
            .map_err(|error| format!("creating {}: {error}", paths.runtime.display()))?;
        fs::set_permissions(&paths.runtime, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("protecting {}: {error}", paths.runtime.display()))?;

        let random = crate::random::bytes::<2>();
        let first = 1000_u16.saturating_add(u16::from_be_bytes(random) % 20_000);
        for offset in 0..DISPLAY_ATTEMPTS {
            let number = first.wrapping_add(u16::try_from(offset).unwrap_or(0));
            let socket = paths.sockets.join(format!("X{number}"));
            let lock = paths.locks.join(format!(".X{number}-lock"));
            if fs::symlink_metadata(&socket).is_ok() || !claim_lock(&lock)? {
                continue;
            }
            let listener = match UnixListener::bind(&socket) {
                Ok(listener) => listener,
                Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
                    let _ = fs::remove_file(&lock);
                    continue;
                }
                Err(error) => {
                    let _ = fs::remove_file(&lock);
                    return Err(format!("binding {}: {error}", socket.display()));
                }
            };
            let socket_identity = match fs::symlink_metadata(&socket) {
                Ok(metadata) => (metadata.dev(), metadata.ino()),
                Err(error) => {
                    let _ = fs::remove_file(&lock);
                    return Err(format!("inspecting {}: {error}", socket.display()));
                }
            };
            let authority = paths.runtime.join(format!("Xauthority-{number}"));
            let _ = fs::remove_file(&authority);
            let cookie = match write_xauthority(&authority, number) {
                Ok(cookie) => cookie,
                Err(error) => {
                    let _ = fs::remove_file(&socket);
                    let _ = fs::remove_file(&lock);
                    return Err(error);
                }
            };
            return Ok(Self {
                number,
                listener,
                socket,
                socket_identity,
                lock,
                authority,
                cookie,
                bridge: None,
                rapid_exits: 0,
                idle_since: None,
                idle_limit: None,
                idle_checked: None,
                saw_toplevel: false,
                composer_asked: None,
            });
        }
        Err("no unused X display number is available".to_string())
    }

    fn session(&self) -> X11Session {
        X11Session {
            display: format!(":{}", self.number),
            authority: self.authority.clone(),
        }
    }

    /// Whether a client connecting starts a bridge: only while none is
    /// running to accept it.
    fn waits_for_client(&self) -> bool {
        self.bridge.is_none()
    }

    /// Returns `true` once the bridge has exited too often to restart.
    fn poll(
        &mut self,
        now: Instant,
        spawn: &mut Spawn<'_>,
        composer: &mut ComposerClients,
        mapped_satellites: &HashSet<u32>,
    ) -> bool {
        let session = self.session();
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.poll_keyboard_update(&session);
            match bridge.child.try_wait() {
                Ok(Some(status)) => {
                    let uptime = now.saturating_duration_since(bridge.started);
                    tracing::warn!(
                        target: XWAYLAND_LOG,
                        %status,
                        display = self.number,
                        uptime_ms = uptime.as_millis(),
                        "Xwayland bridge exited"
                    );
                    self.stop_bridge();
                    if uptime >= STABLE_UPTIME {
                        self.rapid_exits = 0;
                    }
                    self.rapid_exits = self.rapid_exits.saturating_add(1);
                    return self.rapid_exits >= MAX_RAPID_EXITS;
                }
                Ok(None) => {}
                Err(error) => {
                    // Do not start a second server when the kernel cannot
                    // tell us whether the first one is alive.
                    tracing::warn!(
                        target: XWAYLAND_LOG,
                        %error,
                        "could not inspect Xwayland bridge"
                    );
                    return false;
                }
            }
            let uptime = now.saturating_duration_since(bridge.started);
            bridge.settle_resources(self.number, uptime);
            if let Some(group) = composer.process_group()
                && self.composer_asked != Some(group)
                && uptime >= COMPOSER_CONNECT_DELAY
                && bridge.resources.is_none()
            {
                self.composer_asked = Some(group);
                composer.requests.push(session.display.clone());
            }
            if self
                .idle_checked
                .is_some_and(|checked| now.saturating_duration_since(checked) < IDLE_CHECK_INTERVAL)
            {
                return false;
            }
            self.idle_checked = Some(now);
            self.reap_if_idle(
                now,
                composer.connected_clients(&self.socket),
                mapped_satellites,
            );
            return false;
        }
        if !has_pending_connection(&self.listener) {
            return false;
        }
        match spawn(self) {
            Ok((child, keyboard, resources)) => {
                tracing::info!(
                    target: XWAYLAND_LOG,
                    display = self.number,
                    "starting Xwayland bridge for its first client"
                );
                self.bridge = Some(Bridge {
                    child,
                    started: now,
                    keyboard,
                    keyboard_update: None,
                    pending_keyboard: None,
                    resources,
                });
                self.idle_since = None;
                self.idle_limit = None;
                false
            }
            Err(error) => {
                tracing::warn!(
                    target: XWAYLAND_LOG,
                    %error,
                    display = self.number,
                    "could not start Xwayland bridge"
                );
                self.rapid_exits = self.rapid_exits.saturating_add(1);
                self.rapid_exits >= MAX_RAPID_EXITS
            }
        }
    }

    fn reap_if_idle(
        &mut self,
        now: Instant,
        connected: Option<usize>,
        mapped_satellites: &HashSet<u32>,
    ) {
        let mapped = self
            .bridge
            .as_ref()
            .is_some_and(|bridge| mapped_satellites.contains(&bridge.child.id()));
        if mapped {
            self.saw_toplevel = true;
        }
        if connected == Some(0) {
            // The next client is a new app, not the previous app's leftovers.
            self.saw_toplevel = false;
        }
        if let Some(limit) = idle_deadline(connected, mapped, self.saw_toplevel) {
            if self.idle_limit != Some(limit) {
                self.idle_limit = Some(limit);
                self.idle_since = Some(now);
            }
            let idle_since = *self.idle_since.get_or_insert(now);
            if now.saturating_duration_since(idle_since) >= limit {
                let uptime = self.bridge.as_ref().map_or(Duration::ZERO, |bridge| {
                    now.saturating_duration_since(bridge.started)
                });
                tracing::info!(
                    target: XWAYLAND_LOG,
                    display = self.number,
                    uptime_ms = uptime.as_millis(),
                    "stopping idle Xwayland bridge"
                );
                self.stop_bridge();
            }
        } else {
            self.idle_since = None;
            self.idle_limit = None;
        }
    }

    fn stop_bridge(&mut self) {
        self.idle_since = None;
        self.idle_limit = None;
        self.idle_checked = None;
        self.saw_toplevel = false;
        self.composer_asked = None;
        let Some(mut bridge) = self.bridge.take() else {
            return;
        };
        if let Some(mut update) = bridge.keyboard_update.take() {
            let _ = update.child.kill();
            let _ = update.child.wait();
        }
        if let Some(publish) = bridge.resources.take() {
            publish.abandon();
        }
        if matches!(bridge.child.try_wait(), Ok(None)) {
            let _ = bridge.child.kill();
        }
        let _ = bridge.child.wait();
    }

    /// Apply an updated Tilo keymap to a running X server. Xwayland cannot
    /// consume the Wayland seat's keymap, so without this an X11 application
    /// keeps the layout from its server's start. `setxkbmap` updates the
    /// server in place: existing windows stay open and subsequent key events
    /// use the same map as native clients. A display without a running
    /// server needs nothing: the next one starts with the newest settings.
    fn request_keyboard(&mut self, keyboard: &KeyboardSettings, setxkbmap: &Path) {
        let session = self.session();
        let Some(bridge) = self.bridge.as_mut() else {
            return;
        };
        if bridge.keyboard_update.is_some() {
            // The desired layout may change back to the last applied one
            // while a different update is running. Preserve that request
            // too: after the helper succeeds `bridge.keyboard` will no longer
            // describe the X server and it must be actively restored.
            bridge.pending_keyboard = Some(keyboard.clone());
            return;
        }
        if *keyboard == bridge.keyboard {
            return;
        }
        match start_keyboard_update(&session, keyboard.clone(), setxkbmap) {
            Ok(update) => bridge.keyboard_update = Some(update),
            Err(error) => tracing::warn!(
                target: XWAYLAND_LOG,
                %error,
                "Xwayland: could not start keyboard settings update; preserving the working keymap"
            ),
        }
    }
}

impl Bridge {
    /// Collect the Xft.dpi publisher once it finished, or abandon it after
    /// [`RESOURCES_TIMEOUT`].
    fn settle_resources(&mut self, number: u16, uptime: Duration) {
        if let Some(publish) = self
            .resources
            .take_if(|publish| publish.worker.is_finished())
        {
            match publish.worker.join() {
                Ok(Ok(dpi)) => {
                    // When satellite announced its scale: the bridge is serving.
                    tracing::info!(
                        target: XWAYLAND_LOG,
                        display = number,
                        dpi,
                        since_start_ms = uptime.as_millis(),
                        "Xwayland: published Xft.dpi"
                    );
                }
                Ok(Err(error)) => {
                    tracing::warn!(
                        target: XWAYLAND_LOG,
                        %error,
                        display = number,
                        "Xwayland: could not publish Xft.dpi"
                    );
                }
                Err(_) => tracing::warn!(
                    target: XWAYLAND_LOG,
                    display = number,
                    "Xwayland: Xft.dpi publisher panicked"
                ),
            }
        } else if uptime >= RESOURCES_TIMEOUT
            && let Some(publish) = self.resources.take()
        {
            tracing::warn!(
                target: XWAYLAND_LOG,
                display = number,
                "Xwayland: gave up publishing Xft.dpi"
            );
            publish.abandon();
        }
    }

    fn poll_keyboard_update(&mut self, session: &X11Session) {
        let Some(update) = self.keyboard_update.as_mut() else {
            return;
        };
        let result = match update.child.try_wait() {
            Ok(Some(status)) if status.success() => Ok(true),
            Ok(Some(status)) => Err(format!("setxkbmap exited with {status}")),
            Ok(None) if update.started.elapsed() < KEYMAP_UPDATE_TIMEOUT => Ok(false),
            Ok(None) => {
                let _ = update.child.kill();
                let _ = update.child.wait();
                Err(format!(
                    "setxkbmap did not finish within {} seconds",
                    KEYMAP_UPDATE_TIMEOUT.as_secs()
                ))
            }
            Err(error) => Err(format!("checking setxkbmap: {error}")),
        };
        match result {
            Ok(false) => return,
            Ok(true) => {
                if let Some(update) = self.keyboard_update.take() {
                    tracing::info!(
                        target: XWAYLAND_LOG,
                        sources = ?update.requested.source_ids,
                        active = update.requested.active_index,
                        "Xwayland: applied per-user XKB sources"
                    );
                    self.keyboard = update.requested;
                }
            }
            Err(error) => {
                self.keyboard_update = None;
                tracing::warn!(
                    target: XWAYLAND_LOG,
                    %error,
                    "Xwayland: could not apply keyboard settings; preserving the working keymap"
                );
            }
        }
        // Start the latest coalesced request. Intermediate layouts are
        // deliberately skipped; the server only needs to converge to the
        // most recent saved preference.
        let Some(keyboard) = self.pending_keyboard.take() else {
            return;
        };
        if keyboard == self.keyboard {
            return;
        }
        match start_keyboard_update(session, keyboard, Path::new("/usr/bin/setxkbmap")) {
            Ok(update) => self.keyboard_update = Some(update),
            Err(error) => tracing::warn!(
                target: XWAYLAND_LOG,
                %error,
                "Xwayland: could not start coalesced keyboard settings update"
            ),
        }
    }
}

impl Drop for WorkspaceDisplay {
    fn drop(&mut self) {
        self.stop_bridge();
        if fs::symlink_metadata(&self.socket)
            .is_ok_and(|metadata| (metadata.dev(), metadata.ino()) == self.socket_identity)
        {
            let _ = fs::remove_file(&self.socket);
        }
        let _ = fs::remove_file(&self.lock);
        let _ = fs::remove_file(&self.authority);
    }
}

/// What the compositor thread tells the bridge thread. The bridge thread
/// reads it each time it wakes.
#[derive(Default)]
struct BridgeInputs {
    /// The session's open workspaces; `None` until the compositor first says,
    /// so no display is retired before then.
    live: Option<HashSet<String>>,
    /// Satellite processes that have a mapped toplevel.
    mapped: HashSet<u32>,
    /// Keyboard settings to give running servers, if they changed.
    keyboard: Option<KeyboardChange>,
}

/// New keyboard settings for running X servers.
enum KeyboardChange {
    /// Read the saved settings again.
    Reload,
    /// Use these.
    Apply(KeyboardSettings),
}

impl KeyboardChange {
    fn settings(self) -> KeyboardSettings {
        match self {
            Self::Reload => KeyboardSettings::load(),
            Self::Apply(keyboard) => keyboard,
        }
    }
}

/// Wakes the bridge thread. Never blocks: a full pipe already holds a wake.
struct BridgeWake(UnixStream);

impl BridgeWake {
    fn wake(&self) {
        let _ = (&self.0).write(&[1]);
    }
}

/// The compositor thread's handle on the X11 displays. It starts, watches
/// and stops no process and reads no file: the `xwayland-bridges` thread
/// does ([`run_bridges`]), and the `xwayland-displays` thread binds and
/// removes displays. This only tells the bridge thread which workspaces are
/// open, exports the active workspace's display and announces a workspace
/// whose bridge kept failing.
pub(crate) struct XwaylandSupervisor {
    inputs: Arc<Mutex<BridgeInputs>>,
    wake: Option<Arc<BridgeWake>>,
}

impl XwaylandSupervisor {
    /// Make X11 available to host launches. Nothing is bound or spawned
    /// until a launch asks for a workspace's display.
    pub(crate) fn start(wake: calloop::ping::Ping, wayland_socket: &OsStr) -> Self {
        let inputs = Arc::new(Mutex::new(BridgeInputs::default()));
        let mut bridges = None;
        match Paths::session() {
            Ok(paths) => {
                let satellite = resolve_satellite();
                if let Err(error) = &satellite {
                    tracing::warn!(target: XWAYLAND_LOG, %error, "X11 apps are unavailable");
                }
                let mut displays = Displays::new(paths, satellite);
                displays.start_display_thread(deliver_to_session);
                bridges = spawn_bridge_thread(Arc::clone(&inputs), wayland_socket.to_os_string());
                let mut registry = registry();
                registry.displays = Some(displays);
                registry.wake = Some(wake);
                registry.bridges.clone_from(&bridges);
            }
            Err(error) => tracing::warn!(target: XWAYLAND_LOG, %error, "X11 apps are unavailable"),
        }
        Self {
            inputs,
            wake: bridges,
        }
    }

    /// Have running X servers read the saved keyboard settings again.
    pub(crate) fn reload_keyboard_settings(&self) {
        self.send_keyboard(KeyboardChange::Reload);
    }

    /// Give running X servers `keyboard`.
    pub(crate) fn apply_keyboard_settings(&self, keyboard: &KeyboardSettings) {
        self.send_keyboard(KeyboardChange::Apply(keyboard.clone()));
    }

    fn send_keyboard(&self, keyboard: KeyboardChange) {
        self.inputs.lock_recover().keyboard = Some(keyboard);
        if let Some(wake) = &self.wake {
            wake.wake();
        }
    }

    /// Poll from the compositor's frame loop, before launch actions run, so
    /// a launch in this tick sees the current set of displays. Never waits:
    /// while the bridge thread holds the displays this tick does nothing,
    /// and the bridge thread wakes the loop when it has news.
    pub(crate) fn poll(
        &self,
        chrome: &crate::shell::chrome_state::Chrome,
        mapped_satellites: &HashSet<u32>,
    ) {
        let live = live_workspaces(chrome);
        {
            let mut inputs = self.inputs.lock_recover();
            if inputs.mapped != *mapped_satellites {
                inputs.mapped.clone_from(mapped_satellites);
            }
            if inputs.live.as_ref() != Some(&live) {
                inputs.live = Some(live.clone());
                drop(inputs);
                if let Some(wake) = &self.wake {
                    wake.wake();
                }
            }
        }
        let mut registry = match REGISTRY.try_lock() {
            Ok(registry) => registry,
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(std::sync::TryLockError::WouldBlock) => return,
        };
        let Some(displays) = registry.displays.as_mut() else {
            return;
        };
        // D-Bus-activated X11 apps inherit the session environment. Bind
        // the active workspace's socket (satellite still starts on first
        // connect) so DISPLAY exists before anyone clicks a host app. The
        // display thread binds it; a later tick exports it.
        if let Some(active) = active_workspace_room(chrome)
            && live.contains(&active)
        {
            match displays.bound(&active) {
                Ok(Some(session)) => {
                    crate::compositor::session_env::export_x11(
                        &session.display,
                        &session.authority,
                    );
                }
                Ok(None) => {
                    displays.prepare(&active);
                }
                Err(_) => {}
            }
        }
        let exhausted = std::mem::take(&mut displays.exhausted);
        drop(registry);
        for workspace in exhausted {
            tracing::warn!(
                target: XWAYLAND_LOG,
                %workspace,
                "X11 compatibility unavailable in workspace"
            );
            announce_unavailable(chrome);
        }
    }
}

impl Drop for XwaylandSupervisor {
    fn drop(&mut self) {
        let mut registry = registry();
        registry.displays = None;
        registry.wake = None;
        registry.bridges = None;
        drop(registry);
        // The bridge thread finds no displays and ends.
        if let Some(wake) = &self.wake {
            wake.wake();
        }
    }
}

/// Start the thread that runs the displays' bridges ([`run_bridges`]).
fn spawn_bridge_thread(
    inputs: Arc<Mutex<BridgeInputs>>,
    wayland_socket: OsString,
) -> Option<Arc<BridgeWake>> {
    let (wake, woken) = UnixStream::pair()
        .inspect_err(|error| {
            tracing::warn!(target: XWAYLAND_LOG, %error, "X11 apps unavailable: no bridge wake");
        })
        .ok()?;
    if let Err(error) = wake
        .set_nonblocking(true)
        .and_then(|()| woken.set_nonblocking(true))
    {
        tracing::warn!(target: XWAYLAND_LOG, %error, "X11 apps unavailable: no bridge wake");
        return None;
    }
    std::thread::Builder::new()
        .name("xwayland-bridges".into())
        .spawn(move || run_bridges(&inputs, &woken, &wayland_socket))
        .inspect_err(|error| {
            tracing::warn!(target: XWAYLAND_LOG, %error, "X11 apps unavailable: cannot start the bridge thread");
        })
        .ok()?;
    Some(Arc::new(BridgeWake(wake)))
}

/// The bridge thread: start a display's satellite when a client connects to
/// it, reap and restart bridges, stop idle ones, apply keyboard settings and
/// ask the composer to serve each running server. Starting and reaping
/// processes, `setxkbmap`, the kernel's socket tables and the composer's
/// authority file never touch the compositor thread.
///
/// It sleeps until a display without a running bridge has a client
/// waiting, the compositor sends news, or a running bridge is due a check.
/// Ends once the session's displays are gone.
fn run_bridges(inputs: &Mutex<BridgeInputs>, woken: &UnixStream, wayland_socket: &OsStr) {
    loop {
        let Some((waiting, timeout)) = registry()
            .displays
            .as_ref()
            .map(|displays| (displays.waiting_listeners(), displays.next_check()))
        else {
            return;
        };
        wait_for_bridges(woken, &waiting, timeout);
        drop(waiting);
        let (live, mapped, keyboard) = {
            let mut inputs = inputs.lock_recover();
            (
                inputs.live.clone(),
                inputs.mapped.clone(),
                inputs.keyboard.take(),
            )
        };
        let keyboard = keyboard.map(KeyboardChange::settings);
        let mut registry = registry();
        let Registry { displays, wake, .. } = &mut *registry;
        let Some(displays) = displays.as_mut() else {
            return;
        };
        if let Some(keyboard) = &keyboard {
            displays.reload_keyboard_settings(keyboard);
        }
        let satellite = displays.satellite.clone();
        let mut spawn = |display: &WorkspaceDisplay| match satellite.as_ref() {
            Ok(binary) => {
                let (child, keyboard) = spawn_satellite(binary, wayland_socket, display)?;
                let resources = ResourcePublish::start(display)
                    .inspect_err(|error| {
                        tracing::warn!(
                            target: XWAYLAND_LOG,
                            %error,
                            "Xwayland: cannot publish Xft.dpi"
                        );
                    })
                    .ok();
                Ok((child, keyboard, resources))
            }
            Err(reason) => Err(reason.clone()),
        };
        let mut composer = ComposerClients::current();
        let exhausted = displays.advance(
            live.as_ref(),
            Instant::now(),
            &mut spawn,
            &mut composer,
            &mapped,
        );
        serve_composer(displays, &mut composer);
        let wake = (!exhausted.is_empty()).then(|| wake.clone()).flatten();
        displays.exhausted.extend(exhausted);
        drop(registry);
        if let Some(wake) = wake {
            wake.ping();
        }
    }
}

/// Sleep until `woken` is written to, one of `waiting` has a client to
/// accept, or `timeout` passes (forever without one).
fn wait_for_bridges(woken: &UnixStream, waiting: &[OwnedFd], timeout: Option<Duration>) {
    let mut descriptors: Vec<libc::pollfd> = std::iter::once(woken.as_raw_fd())
        .chain(waiting.iter().map(AsRawFd::as_raw_fd))
        .map(|fd| libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        })
        .collect();
    let timeout = timeout.map_or(-1, |timeout| {
        i32::try_from(timeout.as_millis()).unwrap_or(i32::MAX)
    });
    let count = libc::nfds_t::try_from(descriptors.len()).unwrap_or(libc::nfds_t::MAX);
    // SAFETY: `descriptors` holds `count` valid pollfds for descriptors this
    // thread keeps open for the call.
    let _ = unsafe { libc::poll(descriptors.as_mut_ptr(), count, timeout) };
    let mut drained = [0_u8; 64];
    while matches!((&*woken).read(&mut drained), Ok(read) if read > 0) {}
}

/// Ask the composer to serve the servers that `composer` says it should,
/// once its authority file names every display.
fn serve_composer(displays: &Displays, composer: &mut ComposerClients) {
    if composer.requests.is_empty() {
        return;
    }
    let authorities: Vec<PathBuf> = displays
        .workspaces
        .values()
        .map(|display| display.authority.clone())
        .collect();
    let target =
        super::xwayland_composer::composer_authority_path(&crate::shell::launcher::runtime_dir());
    match super::xwayland_composer::write_composer_authority(&target, &authorities) {
        Ok(()) => {
            for display in composer.requests.drain(..) {
                super::xwayland_composer::request_connection(display);
            }
        }
        Err(error) => {
            tracing::warn!(
                target: XWAYLAND_LOG,
                %error,
                "composer cannot reach X11 displays"
            );
        }
    }
}

fn idle_deadline(connected: Option<usize>, mapped: bool, saw_toplevel: bool) -> Option<Duration> {
    match connected {
        Some(0) => Some(IDLE_TIMEOUT),
        Some(_) if saw_toplevel && !mapped => Some(WINDOWLESS_TIMEOUT),
        _ => None,
    }
}

fn live_workspaces(chrome: &crate::shell::chrome_state::Chrome) -> HashSet<String> {
    let state = chrome.borrow();
    let tree = state.tree();
    tree.desktop
        .workspaces
        .iter()
        .map(|workspace| workspace.room().to_string())
        .collect()
}

fn active_workspace_room(chrome: &crate::shell::chrome_state::Chrome) -> Option<String> {
    chrome
        .borrow()
        .tree()
        .desktop
        .active_workspace()
        .map(|workspace| workspace.room().to_string())
}

fn announce_unavailable(chrome: &crate::shell::chrome_state::Chrome) {
    let mut notification = tilo_desktop::notifications::Notification::new(
        crate::shell::i18n::t("xwayland-unavailable-title"),
        crate::shell::i18n::t("xwayland-unavailable-body"),
    )
    .with_severity(tilo_desktop::notifications::NotificationSeverity::Error)
    .with_app_name(crate::shell::i18n::t("host-source-system"))
    .with_dismiss_after(tilo_desktop::notifications::PERSISTENT_DISMISS);
    // A broken compatibility service is machine state, not an app's social
    // notification. Do Not Disturb must not conceal the only explanation for
    // why legacy applications no longer launch.
    notification.bypass_dnd = true;
    chrome.borrow_mut().push_notification_full(notification);
}

fn spawn_satellite(
    binary: &Path,
    wayland_socket: &OsStr,
    display: &WorkspaceDisplay,
) -> Result<(Child, KeyboardSettings), String> {
    let keyboard = KeyboardSettings::load();
    let runtime_path = satellite_runtime_path(binary, std::env::var_os("PATH").as_deref())?;
    let listen_fd = display.listener.as_raw_fd();
    let mut command = Command::new(binary);
    command
        .arg(format!(":{}", display.number))
        .args(["-listenfd", &listen_fd.to_string()])
        .arg("-auth")
        .arg(&display.authority)
        .args(["-nolisten", "tcp"])
        .env("WAYLAND_DISPLAY", wayland_socket)
        .env_remove("DISPLAY")
        .env_remove("XAUTHORITY")
        // Xwayland compiles its own XKB map before it receives any input.
        // In a display-manager session there need not be PAM-provided
        // XKB_DEFAULT_* variables, so inheriting the process environment
        // silently gives every legacy app US regardless of the Tilo seat
        // keymap. Keep both sides on the same validated settings.
        .env("XKB_DEFAULT_LAYOUT", &keyboard.layouts)
        .env("XKB_DEFAULT_VARIANT", &keyboard.variants)
        .env("XKB_DEFAULT_MODEL", &keyboard.model)
        .env(
            "XKB_DEFAULT_OPTIONS",
            keyboard.options.as_deref().unwrap_or(""),
        )
        // Satellite deliberately execs a bare `Xwayland`. Resolve the
        // sibling shipped in the same runtime first so a container/host PATH
        // mix (or a user-local shadow) cannot pair it with an ABI-
        // incompatible X server. Development fallbacks remain available
        // after that first directory.
        .env("PATH", runtime_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    // SAFETY: the closure runs in the forked child before exec and only
    // calls the async-signal-safe `fcntl`. Clearing FD_CLOEXEC there hands
    // the listening socket to satellite without leaking it into any other
    // process the compositor starts.
    unsafe {
        command.pre_exec(move || {
            if libc::fcntl(listen_fd, libc::F_SETFD, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command
        .spawn()
        .map_err(|error| format!("starting {}: {error}", binary.display()))?;
    Ok((child, keyboard))
}

/// The installed satellite, if it can take over a socket Tilo bound.
fn resolve_satellite() -> Result<PathBuf, String> {
    let binary = satellite_binary().ok_or_else(|| {
        "xwayland-satellite is not installed; X11-only applications are unavailable".to_string()
    })?;
    let supported = Command::new(&binary)
        .arg("--test-listenfd-support")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| format!("running {}: {error}", binary.display()))?
        .success();
    if supported {
        Ok(binary)
    } else {
        Err(format!(
            "{} cannot start on demand (no -listenfd support); X11-only applications are unavailable",
            binary.display()
        ))
    }
}

fn satellite_binary() -> Option<PathBuf> {
    // The native binary never accepts an arbitrary satellite from its
    // environment. Hosted integration tests, however, need to validate a
    // candidate upstream satellite against the same private compositor they
    // start, without replacing the installed host binary.
    #[cfg(feature = "hosted-test-control")]
    if let Some(candidate) = std::env::var_os("TILO_TEST_XWAYLAND_SATELLITE") {
        let candidate = PathBuf::from(candidate);
        if candidate.is_absolute() && candidate.is_file() {
            return Some(candidate);
        }
    }

    let fixed = PathBuf::from("/usr/bin/xwayland-satellite");
    if fixed.is_file() {
        return Some(fixed);
    }
    std::env::var_os("PATH").and_then(|path| {
        std::env::split_paths(&path)
            .map(|dir| dir.join("xwayland-satellite"))
            .find(|candidate| candidate.is_file())
    })
}

fn satellite_runtime_path(satellite: &Path, inherited: Option<&OsStr>) -> Result<OsString, String> {
    let directory = satellite.parent().ok_or_else(|| {
        format!(
            "xwayland-satellite path has no parent directory: {}",
            satellite.display()
        )
    })?;
    let mut paths = vec![directory.to_path_buf()];
    if let Some(inherited) = inherited {
        paths.extend(std::env::split_paths(inherited).filter(|path| path != directory));
    }
    std::env::join_paths(paths)
        .map_err(|error| format!("constructing xwayland-satellite runtime PATH: {error}"))
}

fn setxkbmap_args(keyboard: &KeyboardSettings) -> Vec<OsString> {
    let active = keyboard
        .active_index
        .min(keyboard.layouts.split(',').count().saturating_sub(1));
    let layout = keyboard.layouts.split(',').nth(active).unwrap_or("us");
    let variant = keyboard.variants.split(',').nth(active).unwrap_or("");
    let mut args = vec![
        OsString::from("-model"),
        OsString::from(&keyboard.model),
        OsString::from("-layout"),
        OsString::from(layout),
    ];
    if !variant.is_empty() {
        args.extend([OsString::from("-variant"), OsString::from(variant)]);
    }
    // Clear inherited/server options before applying the exact Tilo set.
    args.extend([OsString::from("-option"), OsString::new()]);
    if let Some(options) = keyboard
        .options
        .as_deref()
        .filter(|value| !value.is_empty())
    {
        args.extend([OsString::from("-option"), OsString::from(options)]);
    }
    args
}

fn start_keyboard_update(
    session: &X11Session,
    keyboard: KeyboardSettings,
    binary: &Path,
) -> Result<KeyboardUpdate, String> {
    let child = Command::new(binary)
        .args(setxkbmap_args(&keyboard))
        .env("DISPLAY", &session.display)
        .env("XAUTHORITY", &session.authority)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("starting setxkbmap: {error}"))?;
    Ok(KeyboardUpdate {
        child,
        requested: keyboard,
        started: Instant::now(),
    })
}

/// Create the shared socket directory the way the X server would, if no
/// earlier server or tmpfiles rule has.
fn ensure_socket_dir(directory: &Path) -> Result<(), String> {
    match fs::symlink_metadata(directory) {
        Ok(metadata) if metadata.is_dir() => Ok(()),
        Ok(_) => Err(format!("{} is not a directory", directory.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => fs::create_dir(directory)
            .and_then(|()| fs::set_permissions(directory, fs::Permissions::from_mode(0o1777)))
            .map_err(|error| format!("creating {}: {error}", directory.display())),
        Err(error) => Err(format!("inspecting {}: {error}", directory.display())),
    }
}

/// Claim a display number with the conventional X server lock file, so
/// neither another Tilo workspace nor any other X server on the machine picks
/// the same number. `Ok(false)` means the number is taken.
fn claim_lock(lock: &Path) -> Result<bool, String> {
    match OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o444)
        .open(lock)
    {
        Ok(mut file) => {
            // The X convention: the owner's pid, ten columns wide.
            file.write_all(format!("{:>10}\n", std::process::id()).as_bytes())
                .map_err(|error| format!("writing {}: {error}", lock.display()))?;
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(format!("claiming {}: {error}", lock.display())),
    }
}

fn has_pending_connection(listener: &UnixListener) -> bool {
    let mut descriptor = libc::pollfd {
        fd: listener.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    // SAFETY: one valid pollfd for a descriptor `listener` keeps open; a
    // zero timeout never blocks.
    let ready = unsafe { libc::poll(&raw mut descriptor, 1, 0) };
    ready > 0 && descriptor.revents & libc::POLLIN != 0
}

/// Write a fresh authority file for `display` and return its cookie.
///
/// This runs under the display registry lock that the compositor loop takes
/// every tick, often on the loop thread itself, so it must not wait for the
/// disk. It deliberately does not fsync: the file is valid only while this
/// session's X server lives, and it sits in the login's runtime directory
/// (`$XDG_RUNTIME_DIR`, a tmpfs). Readers see it through the page cache as
/// soon as the write returns, and after a crash it is regenerated with a new
/// cookie, so durability buys nothing while an fsync under I/O or memory
/// pressure has frozen the desktop for seconds. Privacy does not depend on
/// it either: the file is created exclusively with mode 0600 inside a 0700
/// directory, and no client learns the path before `bind` returns.
fn write_xauthority(path: &Path, display: u16) -> Result<[u8; 16], String> {
    let cookie = crate::random::bytes::<16>();
    let mut contents = Vec::with_capacity(64);
    push_u16(&mut contents, u16::MAX); // FamilyWild
    push_field(&mut contents, &[]);
    push_field(&mut contents, display.to_string().as_bytes());
    push_field(&mut contents, b"MIT-MAGIC-COOKIE-1");
    push_field(&mut contents, &cookie);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(|error| format!("creating Xauthority: {error}"))?;
    file.write_all(&contents)
        .map_err(|error| format!("writing Xauthority: {error}"))?;
    Ok(cookie)
}

/// Wait for satellite's XSETTINGS `Xft/DPI` and merge it into the root
/// window's `RESOURCE_MANAGER` as `Xft.dpi`. Returns the published DPI.
fn publish_font_dpi(stream: UnixStream, cookie: [u8; 16]) -> Result<u32, String> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _, PropMode};
    use x11rb::wrapper::ConnectionExt as _;

    fn text(error: impl std::fmt::Display) -> String {
        error.to_string()
    }
    let (stream, _) =
        x11rb::rust_connection::DefaultStream::from_unix_stream(stream).map_err(text)?;
    let conn = x11rb::rust_connection::RustConnection::connect_to_stream_with_auth_info(
        stream,
        0,
        b"MIT-MAGIC-COOKIE-1".to_vec(),
        cookie.to_vec(),
    )
    .map_err(text)?;
    let root = conn
        .setup()
        .roots
        .first()
        .ok_or("the X server has no screen")?
        .root;
    let atom = |name: &[u8]| -> Result<u32, String> {
        Ok(conn
            .intern_atom(false, name)
            .map_err(text)?
            .reply()
            .map_err(text)?
            .atom)
    };
    let selection = atom(b"_XSETTINGS_S0")?;
    let settings = atom(b"_XSETTINGS_SETTINGS")?;
    let started = Instant::now();
    let scaled_dpi = loop {
        let owner = conn
            .get_selection_owner(selection)
            .map_err(text)?
            .reply()
            .map_err(text)?
            .owner;
        if owner != x11rb::NONE {
            let value = conn
                .get_property(false, owner, settings, AtomEnum::ANY, 0, u32::MAX)
                .map_err(text)?
                .reply()
                .map_err(text)?
                .value;
            if let Some(dpi) = xsettings_integer(&value, "Xft/DPI") {
                break dpi;
            }
        }
        if started.elapsed() >= XSETTINGS_WAIT {
            return Err("satellite announced no Xft/DPI".into());
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    // XSETTINGS carries the DPI in 1/1024ths.
    let dpi = u32::try_from(scaled_dpi)
        .ok()
        .and_then(|dpi| dpi.checked_div(1024))
        .filter(|dpi| *dpi > 0)
        .ok_or_else(|| format!("satellite announced Xft/DPI {scaled_dpi}"))?;
    let existing = conn
        .get_property(
            false,
            root,
            AtomEnum::RESOURCE_MANAGER,
            AtomEnum::STRING,
            0,
            u32::MAX,
        )
        .map_err(text)?
        .reply()
        .map_err(text)?
        .value;
    let merged = with_resource(
        &String::from_utf8_lossy(&existing),
        "Xft.dpi",
        &dpi.to_string(),
    );
    let merged = with_resource(
        &merged,
        "Xcursor.size",
        &cursor_size_for_dpi(dpi).to_string(),
    );
    conn.change_property8(
        PropMode::REPLACE,
        root,
        AtomEnum::RESOURCE_MANAGER,
        AtomEnum::STRING,
        merged.as_bytes(),
    )
    .map_err(text)?;
    conn.sync().map_err(text)?;
    Ok(dpi)
}

/// The integer setting `name` of an XSETTINGS `_XSETTINGS_SETTINGS` value.
fn xsettings_integer(data: &[u8], name: &str) -> Option<i32> {
    let big_endian = match data.first()? {
        0 => false,
        1 => true,
        _ => return None,
    };
    let u16_at = |offset: usize| -> Option<u16> {
        let bytes: [u8; 2] = data.get(offset..offset.checked_add(2)?)?.try_into().ok()?;
        Some(if big_endian {
            u16::from_be_bytes(bytes)
        } else {
            u16::from_le_bytes(bytes)
        })
    };
    let u32_at = |offset: usize| -> Option<u32> {
        let bytes: [u8; 4] = data.get(offset..offset.checked_add(4)?)?.try_into().ok()?;
        Some(if big_endian {
            u32::from_be_bytes(bytes)
        } else {
            u32::from_le_bytes(bytes)
        })
    };
    let padded = |length: usize| length.checked_add(3).map(|length| length & !3);
    let count = u32_at(8)?;
    let mut offset = 12_usize;
    for _ in 0..count {
        let kind = *data.get(offset)?;
        let name_length = usize::from(u16_at(offset.checked_add(2)?)?);
        let name_start = offset.checked_add(4)?;
        let setting = data.get(name_start..name_start.checked_add(name_length)?)?;
        // Past the padded name and the setting's last-change serial.
        offset = name_start
            .checked_add(padded(name_length)?)?
            .checked_add(4)?;
        let value_length = match kind {
            0 => {
                if setting == name.as_bytes() {
                    return u32_at(offset).map(u32::cast_signed);
                }
                4
            }
            1 => 4_usize.checked_add(padded(usize::try_from(u32_at(offset)?).ok()?)?)?,
            2 => 8,
            _ => return None,
        };
        offset = offset.checked_add(value_length)?;
    }
    None
}

fn cursor_size_for_dpi(dpi: u32) -> u32 {
    dpi.saturating_mul(24).saturating_div(96).max(24)
}

/// `resources` with `key` set to `value`, every other line kept.
fn with_resource(resources: &str, key: &str, value: &str) -> String {
    let mut merged: String = resources
        .lines()
        .filter(|line| {
            line.split_once(':')
                .is_none_or(|(name, _)| name.trim() != key)
        })
        .flat_map(|line| [line, "\n"])
        .collect();
    merged.push_str(key);
    merged.push_str(":\t");
    merged.push_str(value);
    merged.push('\n');
    merged
}

fn push_field(buffer: &mut Vec<u8>, field: &[u8]) {
    push_u16(buffer, u16::try_from(field.len()).unwrap_or(u16::MAX));
    buffer.extend_from_slice(field);
}

fn push_u16(buffer: &mut Vec<u8>, value: u16) {
    buffer.extend_from_slice(&value.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixStream;

    struct Fixture {
        _root: tempfile::TempDir,
        displays: Displays,
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let paths = Paths {
            sockets: root.path().join(".X11-unix"),
            locks: root.path().to_path_buf(),
            runtime: root.path().join("runtime/tilo-session/xwayland"),
        };
        Fixture {
            displays: Displays::new(paths, Ok(PathBuf::from("/usr/bin/xwayland-satellite"))),
            _root: root,
        }
    }

    fn live(workspaces: &[&str]) -> HashSet<String> {
        workspaces.iter().map(|w| (*w).to_string()).collect()
    }

    fn sleeper() -> Result<(Child, KeyboardSettings, Option<ResourcePublish>), String> {
        let child = Command::new("/usr/bin/sleep")
            .arg("30")
            .spawn()
            .map_err(|error| error.to_string())?;
        Ok((child, KeyboardSettings::default(), None))
    }

    fn connect(session: &X11Session, displays: &Displays) -> UnixStream {
        let number = session.display.trim_start_matches(':');
        UnixStream::connect(displays.paths.sockets.join(format!("X{number}"))).unwrap()
    }

    #[test]
    fn each_workspace_gets_its_own_display_and_authority() {
        let mut fixture = fixture();
        let (first, created) = fixture.displays.session_for("room-a").unwrap();
        assert!(created);
        let (again, created) = fixture.displays.session_for("room-a").unwrap();
        assert!(!created, "a workspace keeps one display");
        assert_eq!(first, again);
        let (second, _) = fixture.displays.session_for("room-b").unwrap();
        assert_ne!(first.display, second.display);
        assert_ne!(first.authority, second.authority);
        assert_ne!(
            fs::read(&first.authority).unwrap(),
            fs::read(&second.authority).unwrap(),
            "one workspace's cookie must not open another workspace's server"
        );
        for session in [&first, &second] {
            let number = session.display.trim_start_matches(':');
            assert!(
                fixture
                    .displays
                    .paths
                    .locks
                    .join(format!(".X{number}-lock"))
                    .exists(),
                "the display number is claimed machine-wide"
            );
        }
    }

    /// The compositor thread never binds or removes a display: it asks the
    /// display thread, which binds once however often it is asked, and
    /// adopts what it delivers. A closed workspace's display is removed
    /// there too.
    #[test]
    fn the_display_thread_binds_and_removes_displays() {
        let mut fixture = fixture();
        let (delivered, received) = mpsc::channel();
        fixture
            .displays
            .start_display_thread(move |workspace, bound| {
                let _ = delivered.send((workspace, bound));
            });
        assert!(fixture.displays.worker.is_some());
        assert_eq!(fixture.displays.bound("room-a"), Ok(None));
        assert!(fixture.displays.prepare("room-a"));
        assert!(fixture.displays.prepare("room-a"));
        let (workspace, bound) = received.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(workspace, "room-a");
        assert!(fixture.displays.adopt(workspace, bound).is_none());
        let session = fixture.displays.bound("room-a").unwrap().unwrap();
        let number = session.display.trim_start_matches(':').to_string();
        let socket = fixture.displays.paths.sockets.join(format!("X{number}"));
        let lock = fixture
            .displays
            .paths
            .locks
            .join(format!(".X{number}-lock"));
        assert!(socket.exists() && lock.exists() && session.authority.exists());

        // Asked twice, it bound once: the thread works in order, so the next
        // delivery is the next request's.
        assert!(fixture.displays.prepare("room-a"), "already bound");
        assert!(fixture.displays.prepare("room-b"));
        let (workspace, bound) = received.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(workspace, "room-b");
        assert!(fixture.displays.adopt(workspace, bound).is_none());

        // A display for a workspace that has one comes back to be retired.
        let extra = WorkspaceDisplay::bind(&fixture.displays.paths).unwrap();
        let extra = fixture
            .displays
            .adopt("room-a".into(), Ok(extra))
            .expect("the workspace keeps its display");
        assert_eq!(fixture.displays.bound("room-a"), Ok(Some(session.clone())));
        fixture.displays.retire(extra);

        fixture.displays.retain_workspaces(&live(&["room-b"]));
        assert_eq!(fixture.displays.bound("room-a"), Ok(None));
        // Once a later bind is delivered, the closed workspace's display is
        // gone.
        assert!(fixture.displays.prepare("room-c"));
        let _ = received.recv_timeout(Duration::from_secs(10)).unwrap();
        assert!(!socket.exists(), "the socket is removed");
        assert!(!lock.exists(), "the display number is released");
        assert!(!session.authority.exists(), "the authority is removed");
    }

    #[test]
    fn launch_environment_carries_only_the_owning_workspace_display() {
        let mut fixture = fixture();
        let (a, _) = fixture.displays.session_for("room-a").unwrap();
        let (b, _) = fixture.displays.session_for("room-b").unwrap();
        let app = crate::shell::host_apps::HostApp {
            desktop_id: "com.valvesoftware.Steam".into(),
            name: "Steam".into(),
            kind: desktop_entry::EntryKind::System,
            categories: Vec::new(),
            keywords: Vec::new(),
            mime_types: Vec::new(),
            terminal: false,
            icon: None,
            downloaded: None,
            window_identities: vec!["steam".into()],
        };
        let command = |x11: Option<&X11Session>| {
            crate::shell::host_apps::build_launch_command(
                &app,
                Path::new("/usr/libexec/tilo/tilo-launcher"),
                Path::new("/run/user/1000/wayland-1"),
                None,
                x11,
                None,
                None,
            )
        };
        let in_a = command(Some(&a));
        assert!(
            in_a.args
                .windows(2)
                .any(|pair| pair == ["--x11-display", a.display.as_str()])
        );
        assert!(!in_a.args.contains(&b.display));
        assert!(
            !in_a
                .args
                .iter()
                .any(|arg| *arg == b.authority.display().to_string()),
            "a launch never learns another workspace's cookie"
        );
        assert!(
            in_a.env
                .iter()
                .all(|(key, _)| key != "DISPLAY" && key != "XAUTHORITY"),
            "the display travels as a launch argument, never ambient environment"
        );
        let unattributed = command(None);
        assert!(
            !unattributed
                .args
                .iter()
                .any(|arg| arg == "--x11-display" || arg == "--x11-authority")
        );
    }

    #[test]
    fn a_display_starts_its_bridge_only_when_a_client_connects() {
        let mut fixture = fixture();
        let (session, _) = fixture.displays.session_for("room-a").unwrap();
        let spawned = std::cell::Cell::new(0_u32);
        let mut spawn = |_: &WorkspaceDisplay| {
            spawned.set(spawned.get().saturating_add(1));
            sleeper()
        };
        fixture.displays.poll(Instant::now(), &mut spawn);
        assert!(
            fixture.displays.workspaces["room-a"].waits_for_client(),
            "no client, no X server"
        );

        let _client = connect(&session, &fixture.displays);
        fixture.displays.poll(Instant::now(), &mut spawn);
        fixture.displays.poll(Instant::now(), &mut spawn);
        assert_eq!(spawned.get(), 1, "one bridge per display, started once");
        assert!(!fixture.displays.workspaces["room-a"].waits_for_client());
    }

    #[test]
    fn closing_a_workspace_stops_its_bridge_and_removes_its_display() {
        let mut fixture = fixture();
        let (session, _) = fixture.displays.session_for("room-a").unwrap();
        let (kept, _) = fixture.displays.session_for("room-b").unwrap();
        let _client = connect(&session, &fixture.displays);
        fixture.displays.poll(Instant::now(), &mut |_| sleeper());
        let pid = fixture.displays.workspaces["room-a"]
            .bridge
            .as_ref()
            .unwrap()
            .child
            .id();
        let socket = fixture.displays.workspaces["room-a"].socket.clone();
        let lock = fixture.displays.workspaces["room-a"].lock.clone();

        fixture.displays.retain_workspaces(&live(&["room-b"]));
        assert!(!fixture.displays.workspaces.contains_key("room-a"));
        assert!(!socket.exists(), "the socket is unlinked");
        assert!(!lock.exists(), "the display number is released");
        assert!(!session.authority.exists(), "the cookie is removed");
        assert!(
            !Path::new(&format!("/proc/{pid}")).exists(),
            "the bridge process is reaped"
        );
        assert!(kept.authority.exists(), "other workspaces are untouched");
    }

    #[test]
    fn wine_without_a_window_stops_the_bridge() {
        assert_eq!(idle_deadline(Some(0), false, false), Some(IDLE_TIMEOUT));
        assert_eq!(
            idle_deadline(Some(1), false, true),
            Some(WINDOWLESS_TIMEOUT),
            "an X client with no toplevel is Wine after close, not a live app"
        );
        assert_eq!(
            idle_deadline(Some(1), true, true),
            None,
            "a mapped satellite is not idle"
        );
        assert_eq!(
            idle_deadline(Some(1), false, false),
            None,
            "a server that has not mapped yet is still starting"
        );
        assert_eq!(idle_deadline(None, false, true), None);
    }

    #[test]
    fn a_new_client_after_an_idle_spell_is_not_reaped_as_windowless() {
        for gap in [3, 30, 59] {
            let mut fixture = fixture();
            let (session, _) = fixture.displays.session_for("room-a").unwrap();
            let _client = connect(&session, &fixture.displays);
            let start = Instant::now();
            fixture.displays.poll(start, &mut |_| sleeper());
            let display = fixture.displays.workspaces.get_mut("room-a").unwrap();
            let pid = display.bridge.as_ref().unwrap().child.id();
            let mapped = HashSet::from([pid]);

            display.reap_if_idle(start, Some(1), &mapped);
            display.reap_if_idle(start + Duration::from_secs(1), Some(0), &HashSet::new());
            let connected_at = start + Duration::from_secs(gap);
            display.reap_if_idle(connected_at, Some(1), &HashSet::new());
            assert!(
                display.bridge.is_some(),
                "a new client must survive connection after {gap}s"
            );
            display.reap_if_idle(
                connected_at + Duration::from_secs(5),
                Some(1),
                &HashSet::new(),
            );
            assert!(
                display.bridge.is_some(),
                "a new client may take more than two seconds to map"
            );
            display.reap_if_idle(connected_at + Duration::from_secs(6), Some(1), &mapped);
            assert!(
                display.bridge.is_some(),
                "the same bridge serves the new window"
            );
        }
    }

    #[test]
    fn windowless_clock_starts_when_the_window_goes() {
        let mut fixture = fixture();
        let (session, _) = fixture.displays.session_for("room-a").unwrap();
        let _client = connect(&session, &fixture.displays);
        let start = Instant::now();
        fixture.displays.poll(start, &mut |_| sleeper());
        let display = fixture.displays.workspaces.get_mut("room-a").unwrap();
        let pid = display.bridge.as_ref().unwrap().child.id();
        let mapped = HashSet::from([pid]);

        display.reap_if_idle(start, Some(1), &mapped);
        display.reap_if_idle(start + Duration::from_secs(50), Some(1), &mapped);
        let closed_at = start + Duration::from_secs(51);
        display.reap_if_idle(closed_at, Some(1), &HashSet::new());
        display.reap_if_idle(
            closed_at + Duration::from_millis(1999),
            Some(2),
            &HashSet::new(),
        );
        assert!(
            display.bridge.is_some(),
            "Wine helpers do not reset the windowless clock"
        );
        display.reap_if_idle(closed_at + Duration::from_secs(2), Some(2), &HashSet::new());
        assert!(
            display.bridge.is_none(),
            "leftover clients lose the bridge after two seconds"
        );
    }

    #[test]
    fn the_empty_bridge_clock_starts_when_the_last_client_leaves() {
        let mut fixture = fixture();
        let (session, _) = fixture.displays.session_for("room-a").unwrap();
        let _client = connect(&session, &fixture.displays);
        let start = Instant::now();
        fixture.displays.poll(start, &mut |_| sleeper());
        let display = fixture.displays.workspaces.get_mut("room-a").unwrap();
        let pid = display.bridge.as_ref().unwrap().child.id();

        display.reap_if_idle(start, Some(1), &HashSet::from([pid]));
        display.reap_if_idle(start + Duration::from_secs(1), Some(1), &HashSet::new());
        let disconnected_at = start + Duration::from_millis(2500);
        display.reap_if_idle(disconnected_at, Some(0), &HashSet::new());
        display.reap_if_idle(
            disconnected_at + Duration::from_millis(59999),
            Some(0),
            &HashSet::new(),
        );
        assert!(
            display.bridge.is_some(),
            "the empty phase gets its own full minute"
        );
        display.reap_if_idle(
            disconnected_at + Duration::from_secs(60),
            Some(0),
            &HashSet::new(),
        );
        assert!(
            display.bridge.is_none(),
            "an empty bridge still expires after one minute"
        );
    }

    #[test]
    fn an_idle_bridge_stops_and_the_next_client_restarts_it() {
        let mut fixture = fixture();
        let (session, _) = fixture.displays.session_for("room-a").unwrap();
        let client = connect(&session, &fixture.displays);
        let start = Instant::now();
        let spawned = std::cell::Cell::new(0_u32);
        let mut spawn = |_: &WorkspaceDisplay| {
            spawned.set(spawned.get().saturating_add(1));
            sleeper()
        };
        fixture.displays.poll(start, &mut spawn);
        // The fake bridge never accepts, so the connection stays in the
        // backlog and the server has no connected client.
        drop(client);
        let display = fixture.displays.workspaces.get_mut("room-a").unwrap();
        // Drain the backlog the fake bridge left behind.
        display.listener.set_nonblocking(true).unwrap();
        while display.listener.accept().is_ok() {}
        fixture.displays.poll(start, &mut spawn);
        assert!(!fixture.displays.workspaces["room-a"].waits_for_client());
        fixture.displays.poll(start + IDLE_TIMEOUT, &mut spawn);
        assert!(
            fixture.displays.workspaces["room-a"].waits_for_client(),
            "no client for the idle timeout stops the server"
        );
        assert_eq!(
            fixture.displays.workspaces["room-a"].rapid_exits, 0,
            "an idle stop is not a failure"
        );
        assert_eq!(
            fixture.displays.session_for("room-a").unwrap().0,
            session,
            "the display outlives its idle server"
        );

        let _client = connect(&session, &fixture.displays);
        fixture.displays.poll(start + IDLE_TIMEOUT, &mut spawn);
        assert_eq!(spawned.get(), 2);
    }

    #[test]
    fn a_crash_looping_bridge_makes_only_its_workspace_unavailable() {
        let mut fixture = fixture();
        let (session, _) = fixture.displays.session_for("room-a").unwrap();
        fixture.displays.session_for("room-b").unwrap();
        let _client = connect(&session, &fixture.displays);
        let mut exhausted = Vec::new();
        for _ in 0..MAX_RAPID_EXITS {
            exhausted = fixture
                .displays
                .poll(Instant::now(), &mut |_| Err("exec failed".into()));
        }
        assert_eq!(exhausted, ["room-a"]);
        assert!(fixture.displays.session_for("room-a").is_err());
        assert!(fixture.displays.session_for("room-b").is_ok());

        fixture.displays.retain_workspaces(&live(&["room-b"]));
        fixture
            .displays
            .retain_workspaces(&live(&["room-a", "room-b"]));
        assert!(
            fixture.displays.session_for("room-a").is_ok(),
            "a reopened workspace tries again"
        );
    }

    #[test]
    fn a_missing_satellite_hands_out_no_display() {
        let mut fixture = fixture();
        fixture.displays.satellite = Err("not installed".into());
        assert!(fixture.displays.session_for("room-a").is_err());
        assert!(fixture.displays.workspaces.is_empty());
    }

    #[test]
    fn the_bridge_thread_waits_only_on_displays_without_a_running_bridge() {
        let mut fixture = fixture();
        let (session, _) = fixture.displays.session_for("room-a").unwrap();
        let now = Instant::now();
        let advance = |displays: &mut Displays, open: &[&str]| {
            displays.advance(
                Some(&live(open)),
                now,
                &mut |_| sleeper(),
                &mut ComposerClients::for_group(None),
                &HashSet::new(),
            )
        };
        advance(&mut fixture.displays, &["room-a"]);
        assert_eq!(fixture.displays.waiting_listeners().len(), 1);
        assert_eq!(fixture.displays.next_check(), None, "no bridge to look at");

        let _client = connect(&session, &fixture.displays);
        advance(&mut fixture.displays, &["room-a"]);
        assert!(
            fixture.displays.waiting_listeners().is_empty(),
            "a running bridge accepts its own clients"
        );
        assert_eq!(fixture.displays.next_check(), Some(IDLE_CHECK_INTERVAL));

        fixture.displays.advance(
            None,
            now,
            &mut |_| sleeper(),
            &mut ComposerClients::for_group(None),
            &HashSet::new(),
        );
        assert_eq!(
            fixture.displays.workspaces.len(),
            1,
            "nothing is retired before the compositor says which workspaces are open"
        );
        advance(&mut fixture.displays, &[]);
        assert!(fixture.displays.waiting_listeners().is_empty());
        assert!(fixture.displays.workspaces.is_empty());
    }

    #[test]
    fn the_composer_is_asked_once_per_running_server_and_never_keeps_it_up() {
        let mut fixture = fixture();
        let (session, _) = fixture.displays.session_for("room-a").unwrap();
        let start = Instant::now();
        let composer_group = Some(4242);

        // No server yet: nothing to connect to, and the request never
        // starts one.
        let mut composer = ComposerClients::for_group(composer_group);
        fixture
            .displays
            .poll_with(start, &mut |_| sleeper(), &mut composer, &HashSet::new());
        assert!(composer.requests.is_empty());

        let client = connect(&session, &fixture.displays);
        let mut composer = ComposerClients::for_group(composer_group);
        fixture
            .displays
            .poll_with(start, &mut |_| sleeper(), &mut composer, &HashSet::new());
        assert!(
            composer.requests.is_empty(),
            "a server that just started is given time to accept"
        );

        let later = start + COMPOSER_CONNECT_DELAY;
        let mut composer = ComposerClients::for_group(composer_group);
        fixture
            .displays
            .poll_with(later, &mut |_| sleeper(), &mut composer, &HashSet::new());
        assert_eq!(composer.requests, std::slice::from_ref(&session.display));
        let mut composer = ComposerClients::for_group(composer_group);
        fixture.displays.poll_with(
            later + IDLE_CHECK_INTERVAL,
            &mut |_| sleeper(),
            &mut composer,
            &HashSet::new(),
        );
        assert!(composer.requests.is_empty(), "asked once per server");

        let mut restarted = ComposerClients::for_group(Some(4343));
        fixture.displays.poll_with(
            later + IDLE_CHECK_INTERVAL * 2,
            &mut |_| sleeper(),
            &mut restarted,
            &HashSet::new(),
        );
        assert_eq!(
            restarted.requests,
            std::slice::from_ref(&session.display),
            "a restarted composer is asked again"
        );
        drop(client);
    }

    /// An `_XSETTINGS_SETTINGS` value: a string, then integers, as satellite
    /// announces them.
    fn xsettings(big_endian: bool, integers: &[(&str, i32)]) -> Vec<u8> {
        let u16b = |v: u16| {
            if big_endian {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let u32b = |v: u32| {
            if big_endian {
                v.to_be_bytes()
            } else {
                v.to_le_bytes()
            }
        };
        let mut data = vec![u8::from(big_endian), 0, 0, 0];
        data.extend(u32b(7));
        data.extend(u32b(
            u32::try_from(integers.len())
                .unwrap()
                .checked_add(1)
                .unwrap(),
        ));
        let header = |data: &mut Vec<u8>, kind: u8, name: &str| {
            data.extend([kind, 0]);
            data.extend(u16b(u16::try_from(name.len()).unwrap()));
            data.extend(name.as_bytes());
            data.resize(data.len().next_multiple_of(4), 0);
            data.extend(u32b(0));
        };
        header(&mut data, 1, "Net/ThemeName");
        data.extend(u32b(7));
        data.extend(b"Adwaita\0");
        for (name, value) in integers {
            header(&mut data, 0, name);
            data.extend(u32b(value.cast_unsigned()));
        }
        data
    }

    #[test]
    fn satellite_dpi_is_read_from_xsettings_in_either_byte_order() {
        for big_endian in [false, true] {
            let data = xsettings(
                big_endian,
                &[
                    ("Gdk/WindowScalingFactor", 2),
                    ("Xft/DPI", 192 * 1024),
                    ("Gdk/UnscaledDPI", 96 * 1024),
                ],
            );
            assert_eq!(xsettings_integer(&data, "Xft/DPI"), Some(192 * 1024));
            assert_eq!(xsettings_integer(&data, "Gdk/WindowScalingFactor"), Some(2));
            assert_eq!(
                xsettings_integer(&data, "Net/ThemeName"),
                None,
                "a string is not an integer"
            );
            assert_eq!(xsettings_integer(&data, "Xft/Hinting"), None);
            let truncated = &data[..data.len() - 2];
            assert_eq!(xsettings_integer(truncated, "Gdk/UnscaledDPI"), None);
        }
        assert_eq!(xsettings_integer(&[], "Xft/DPI"), None);
    }

    #[test]
    fn publishing_dpi_replaces_only_that_resource() {
        assert_eq!(cursor_size_for_dpi(96), 24);
        assert_eq!(cursor_size_for_dpi(192), 48);
        assert_eq!(with_resource("", "Xft.dpi", "192"), "Xft.dpi:\t192\n");
        assert_eq!(
            with_resource(
                "Xcursor.theme:\tAdwaita\nXft.dpi:\t96\n*customization: -color",
                "Xft.dpi",
                "192"
            ),
            "Xcursor.theme:\tAdwaita\n*customization: -color\nXft.dpi:\t192\n"
        );
    }

    #[test]
    fn the_composer_waits_for_the_published_dpi_but_not_forever() {
        let (release, released) = std::sync::mpsc::channel::<()>();
        let released = Mutex::new(Some(released));
        let mut far_ends = Vec::new();
        let mut spawn = |_: &WorkspaceDisplay| {
            let (child, keyboard, _) = sleeper()?;
            let (stream, far_end) = UnixStream::pair().map_err(|error| error.to_string())?;
            far_ends.push(far_end);
            let resources = released
                .lock()
                .unwrap()
                .take()
                .map(|released| ResourcePublish {
                    worker: std::thread::spawn(move || {
                        released.recv().map_err(|error| error.to_string())?;
                        Ok(192)
                    }),
                    stream,
                });
            Ok((child, keyboard, resources))
        };
        let mut fixture = fixture();
        let (session, _) = fixture.displays.session_for("room-a").unwrap();
        let client = connect(&session, &fixture.displays);
        let start = Instant::now();
        let group = Some(4242);
        fixture.displays.poll_with(
            start,
            &mut spawn,
            &mut ComposerClients::for_group(group),
            &HashSet::new(),
        );

        let mut composer = ComposerClients::for_group(group);
        fixture.displays.poll_with(
            start + COMPOSER_CONNECT_DELAY,
            &mut spawn,
            &mut composer,
            &HashSet::new(),
        );
        assert!(
            composer.requests.is_empty(),
            "the composer would read 96 DPI"
        );

        release.send(()).unwrap();
        let display = fixture.displays.workspaces.get("room-a").unwrap();
        let bridge = display.bridge.as_ref().unwrap();
        while !bridge.resources.as_ref().unwrap().worker.is_finished() {
            std::thread::yield_now();
        }
        let mut composer = ComposerClients::for_group(group);
        fixture.displays.poll_with(
            start + COMPOSER_CONNECT_DELAY,
            &mut spawn,
            &mut composer,
            &HashSet::new(),
        );
        assert_eq!(composer.requests, std::slice::from_ref(&session.display));

        // A server whose publisher never finishes still gets its composer.
        fixture
            .displays
            .workspaces
            .get_mut("room-a")
            .unwrap()
            .stop_bridge();
        let (_stalled, stalled) = std::sync::mpsc::channel::<()>();
        *released.lock().unwrap() = Some(stalled);
        let restart = start + STABLE_UPTIME;
        fixture.displays.poll_with(
            restart,
            &mut spawn,
            &mut ComposerClients::for_group(group),
            &HashSet::new(),
        );
        let mut composer = ComposerClients::for_group(group);
        fixture.displays.poll_with(
            restart + COMPOSER_CONNECT_DELAY,
            &mut spawn,
            &mut composer,
            &HashSet::new(),
        );
        assert!(composer.requests.is_empty());
        let mut composer = ComposerClients::for_group(group);
        fixture.displays.poll_with(
            restart + RESOURCES_TIMEOUT,
            &mut spawn,
            &mut composer,
            &HashSet::new(),
        );
        assert_eq!(composer.requests, std::slice::from_ref(&session.display));
        let mut byte = [0_u8; 1];
        assert_eq!(
            std::io::Read::read(&mut far_ends.last().unwrap(), &mut byte).unwrap(),
            0,
            "the abandoned publisher's connection is shut down"
        );
        drop(client);
    }

    #[test]
    fn connected_clients_are_counted_from_the_socket_table() {
        use super::super::xwayland_composer::{connected_clients, count_connected};
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("X1234");
        let listener = UnixListener::bind(&path).unwrap();
        assert_eq!(connected_clients(&path), Some(0));
        let _client = UnixStream::connect(&path).unwrap();
        let (_accepted, _) = listener.accept().unwrap();
        assert_eq!(connected_clients(&path), Some(1));

        let table = "Num RefCount Protocol Flags Type St Inode Path\n\
            0: 00000002 00000000 00010000 0001 01 1 /tmp/.X11-unix/X7\n\
            0: 00000003 00000000 00000000 0001 03 2 /tmp/.X11-unix/X7\n\
            0: 00000003 00000000 00000000 0001 03 3 /tmp/.X11-unix/X70\n\
            0: 00000003 00000000 00000000 0001 03 4\n";
        assert_eq!(count_connected(table, Path::new("/tmp/.X11-unix/X7")), 1);
    }

    #[test]
    fn xwayland_keyboard_settings_compile_to_setxkbmap_arguments() {
        // Phase `keyboard_settings_retain_layout_variant_and_options_for_xwayland`.
        {
            let section = serde_json::json!({
                "keyboard_layout": "de:nodeadkeys",
                "keyboard_layouts": ["de:nodeadkeys"],
                "compose_key": "rwin"
            })
            .as_object()
            .unwrap()
            .clone();
            let settings = KeyboardSettings::from_section(&section);
            assert_eq!(settings.layouts, "de");
            assert_eq!(settings.variants, "nodeadkeys");
            assert_eq!(settings.options.as_deref(), Some("compose:rwin"));
        }
        // Phase `live_xwayland_update_selects_the_active_layout_and_clears_old_options`.
        {
            let section = serde_json::json!({
                "keyboard_layouts": ["us", "de:nodeadkeys"],
                "keyboard_layout": "de:nodeadkeys",
                "compose_key": "rwin"
            })
            .as_object()
            .unwrap()
            .clone();
            let settings = KeyboardSettings::from_section(&section);
            assert_eq!(
                setxkbmap_args(&settings),
                [
                    "-model",
                    "pc105",
                    "-layout",
                    "de",
                    "-variant",
                    "nodeadkeys",
                    "-option",
                    "",
                    "-option",
                    "compose:rwin",
                ]
                .map(OsString::from)
            );

            let plain = KeyboardSettings::default();
            assert_eq!(
                setxkbmap_args(&plain),
                ["-model", "pc105", "-layout", "us", "-option", ""].map(OsString::from)
            );
        }
    }

    #[test]
    fn xwayland_runtime_path_and_authority_are_private() {
        // Phase `satellite_and_xwayland_are_resolved_from_one_runtime_first`.
        {
            let path = satellite_runtime_path(
                Path::new("/usr/bin/xwayland-satellite"),
                Some(OsStr::new("/run/host/usr/bin:/opt/tools:/usr/bin")),
            )
            .unwrap();
            let entries = std::env::split_paths(&path).collect::<Vec<_>>();
            assert_eq!(
                entries,
                vec![
                    PathBuf::from("/usr/bin"),
                    PathBuf::from("/run/host/usr/bin"),
                    PathBuf::from("/opt/tools"),
                ]
            );
        }
        // Phase `authority_is_private_and_contains_no_text_cookie`.
        {
            let temp = tempfile::tempdir().unwrap();
            let path = temp.path().join("Xauthority");
            write_xauthority(&path, 1234).unwrap();
            let metadata = fs::metadata(&path).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            let bytes = fs::read(path).unwrap();
            assert!(bytes.windows(4).any(|window| window == b"1234"));
            assert!(
                bytes
                    .windows(b"MIT-MAGIC-COOKIE-1".len())
                    .any(|window| window == b"MIT-MAGIC-COOKIE-1")
            );
        }
    }

    /// The compositor loop binds a workspace's X display through
    /// `session_for`, which writes the display's authority file, and its
    /// supervisor gives the composing input method every cookie. Nothing on
    /// those paths may wait for the disk: under I/O or memory pressure an
    /// fsync there froze input and presentation for seconds.
    #[test]
    fn binding_a_display_never_waits_for_the_disk() {
        // The shared guard proves its filter is in force before the body runs.
        crate::shell::disk_sync_guard::on_a_thread_that_cannot_sync(|| {
            let mut fixture = fixture();
            let (session, fresh) = fixture
                .displays
                .session_for("room-a")
                .expect("binding a display must not sync to disk");
            assert!(fresh);
            let display = &fixture.displays.workspaces["room-a"];
            assert_eq!(session.authority, display.authority);
            let metadata = fs::metadata(&session.authority).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            let runtime = fs::metadata(&fixture.displays.paths.runtime).unwrap();
            assert_eq!(runtime.permissions().mode() & 0o777, 0o700);
            let bytes = fs::read(&session.authority).unwrap();
            assert!(
                bytes.ends_with(&display.cookie),
                "the file carries the cookie"
            );

            let composer = fixture.displays.paths.runtime.join("composer-Xauthority");
            super::super::xwayland_composer::write_composer_authority(
                &composer,
                std::slice::from_ref(&display.authority),
            )
            .expect("installing the composer's authority must not sync to disk");
            let metadata = fs::metadata(&composer).unwrap();
            assert_eq!(metadata.permissions().mode() & 0o777, 0o600);
            assert_eq!(fs::read(&composer).unwrap(), bytes);
        });
    }

    #[test]
    fn keyboard_update_waits_for_a_running_server_and_coalesces_to_the_latest() {
        let mut fixture = fixture();
        let (session, _) = fixture.displays.session_for("room-a").unwrap();
        let temp = tempfile::tempdir().unwrap();
        let slow_setxkbmap = temp.path().join("setxkbmap");
        fs::write(&slow_setxkbmap, "#!/bin/sh\nexec /usr/bin/sleep 30\n").unwrap();
        fs::set_permissions(&slow_setxkbmap, fs::Permissions::from_mode(0o700)).unwrap();
        let german = KeyboardSettings {
            model: "pc105".into(),
            source_ids: vec!["de:nodeadkeys".into()],
            layouts: "de".into(),
            variants: "nodeadkeys".into(),
            options: None,
            active_index: 0,
        };

        let display = fixture.displays.workspaces.get_mut("room-a").unwrap();
        display.request_keyboard(&german, &slow_setxkbmap);
        assert!(display.bridge.is_none(), "no server, nothing to update");

        let _client = connect(&session, &fixture.displays);
        fixture.displays.poll(Instant::now(), &mut |_| sleeper());
        let display = fixture.displays.workspaces.get_mut("room-a").unwrap();
        display.request_keyboard(&german, &slow_setxkbmap);
        // The latest request returns to the layout the server already has
        // while the German update is still in flight; it must not be lost.
        display.request_keyboard(&KeyboardSettings::default(), &slow_setxkbmap);
        let bridge = display.bridge.as_ref().unwrap();
        assert_eq!(
            bridge
                .keyboard_update
                .as_ref()
                .map(|update| &update.requested),
            Some(&german)
        );
        assert_eq!(bridge.pending_keyboard, Some(KeyboardSettings::default()));
    }
}
