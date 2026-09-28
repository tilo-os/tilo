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

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::os::fd::{AsFd as _, AsRawFd as _, OwnedFd};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _};
use std::os::unix::net::UnixListener;
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use getrandom::{SysRng, rand_core::UnwrapErr};
use rand_core::Rng as _;
use smithay::reexports::calloop::{
    self, Interest, LoopHandle, Mode, PostAction, RegistrationToken, generic::Generic,
};

use super::backend::drm::input_config::KeyboardSettings;
use super::xwayland_composer::ComposerClients;
use crate::shell::host_apps::X11Session;

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
const KEYMAP_UPDATE_TIMEOUT: Duration = Duration::from_secs(2);
/// The frame loop polls far more often than an idle stop needs; reading the
/// kernel's socket table once a second keeps idle detection off the frame
/// budget.
const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(1);
/// How long a freshly started X server gets before the composing input
/// method is asked to connect, so the request lands on a server that is
/// already accepting rather than stalling the composer in the handshake.
const COMPOSER_CONNECT_DELAY: Duration = Duration::from_secs(1);

/// Every workspace display of this session, shared by the host-launch path
/// (which creates displays) and the compositor-thread supervisor (which
/// starts, stops and removes them). `None` outside a running compositor, so
/// headless tools and tests never bind X11 sockets.
static REGISTRY: Mutex<Registry> = Mutex::new(Registry {
    displays: None,
    wake: None,
});

struct Registry {
    displays: Option<Displays>,
    /// Wakes the compositor loop so it starts watching a display created by
    /// a launch outside its own tick.
    wake: Option<calloop::ping::Ping>,
}

fn registry() -> std::sync::MutexGuard<'static, Registry> {
    REGISTRY
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The X11 connection for a host launch into `workspace`, creating that
/// workspace's display on first use. `None` when X11 is unavailable there;
/// the launch then proceeds Wayland-only.
pub(crate) fn session_for_workspace(workspace: &str) -> Option<X11Session> {
    let mut registry = registry();
    let Registry { displays, wake } = &mut *registry;
    let result = displays.as_mut()?.session_for(workspace);
    let wake = wake.clone();
    drop(registry);
    match result {
        Ok((session, created)) => {
            if created && let Some(wake) = wake {
                wake.ping();
            }
            Some(session)
        }
        Err(error) => {
            tracing::warn!(%error, "X11 apps unavailable in this workspace");
            None
        }
    }
}

/// Where the X11 sockets, lock files and private authorities live.
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
        let runtime_root = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
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
    bridge: Option<Bridge>,
    rapid_exits: u8,
    /// When the running bridge was first observed with no connected client.
    idle_since: Option<Instant>,
    idle_checked: Option<Instant>,
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
}

struct KeyboardUpdate {
    child: Child,
    requested: KeyboardSettings,
    started: Instant,
}

/// Starts a satellite for a display that just received its first client.
type Spawn<'a> = dyn FnMut(&WorkspaceDisplay) -> Result<(Child, KeyboardSettings), String> + 'a;

impl Displays {
    fn new(paths: Paths, satellite: Result<PathBuf, String>) -> Self {
        Self {
            paths,
            satellite,
            workspaces: HashMap::new(),
            unavailable: HashSet::new(),
        }
    }

    /// The display of `workspace`, bound now if it has none yet. The second
    /// value says whether it was just created and needs watching.
    fn session_for(&mut self, workspace: &str) -> Result<(X11Session, bool), String> {
        if let Some(display) = self.workspaces.get(workspace) {
            return Ok((display.session(), false));
        }
        if let Err(reason) = &self.satellite {
            return Err(reason.clone());
        }
        if self.unavailable.contains(workspace) {
            return Err("the X11 compatibility service kept exiting in this workspace".into());
        }
        let display = WorkspaceDisplay::bind(&self.paths)?;
        let session = display.session();
        tracing::info!(display = %session.display, "X11 display ready for a workspace");
        self.workspaces.insert(workspace.to_string(), display);
        Ok((session, true))
    }

    /// Remove every display whose workspace closed, stopping its X server.
    fn retain_workspaces(&mut self, live: &HashSet<String>) {
        self.workspaces
            .retain(|workspace, _| live.contains(workspace));
        self.unavailable
            .retain(|workspace| live.contains(workspace));
    }

    /// Advance every display: reap exited bridges, stop idle ones and start
    /// one for each display with a waiting client. Returns the workspaces
    /// that just became unavailable.
    #[cfg(test)]
    fn poll(&mut self, now: Instant, spawn: &mut Spawn<'_>) -> Vec<String> {
        self.poll_with(now, spawn, &mut ComposerClients::for_group(None))
    }

    /// [`Self::poll`] with the composing input method's view: its
    /// connections do not count towards keeping a server up, and it is
    /// asked to serve each server that is running for a real client.
    fn poll_with(
        &mut self,
        now: Instant,
        spawn: &mut Spawn<'_>,
        composer: &mut ComposerClients,
    ) -> Vec<String> {
        let mut exhausted = Vec::new();
        for (workspace, display) in &mut self.workspaces {
            if display.poll(now, spawn, composer) {
                exhausted.push(workspace.clone());
            }
        }
        for workspace in &exhausted {
            self.workspaces.remove(workspace);
            self.unavailable.insert(workspace.clone());
        }
        exhausted
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

        let mut random = [0_u8; 2];
        UnwrapErr(SysRng).fill_bytes(&mut random);
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
            if let Err(error) = write_xauthority(&authority, number) {
                let _ = fs::remove_file(&socket);
                let _ = fs::remove_file(&lock);
                return Err(error);
            }
            return Ok(Self {
                number,
                listener,
                socket,
                socket_identity,
                lock,
                authority,
                bridge: None,
                rapid_exits: 0,
                idle_since: None,
                idle_checked: None,
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

    /// Whether the compositor loop should wake when a client connects: only
    /// while no bridge is running to accept it.
    fn wants_watch(&self) -> bool {
        self.bridge.is_none()
    }

    /// Returns `true` once the bridge has exited too often to restart.
    fn poll(
        &mut self,
        now: Instant,
        spawn: &mut Spawn<'_>,
        composer: &mut ComposerClients,
    ) -> bool {
        let session = self.session();
        if let Some(bridge) = self.bridge.as_mut() {
            bridge.poll_keyboard_update(&session);
            match bridge.child.try_wait() {
                Ok(Some(status)) => {
                    let uptime = now.saturating_duration_since(bridge.started);
                    tracing::warn!(%status, display = self.number, "Xwayland bridge exited");
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
                    tracing::warn!(%error, "could not inspect Xwayland bridge");
                    return false;
                }
            }
            if let Some(group) = composer.process_group()
                && self.composer_asked != Some(group)
                && now.saturating_duration_since(bridge.started) >= COMPOSER_CONNECT_DELAY
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
            match composer.connected_clients(&self.socket) {
                Some(0) => {
                    let idle_since = *self.idle_since.get_or_insert(now);
                    if now.saturating_duration_since(idle_since) >= IDLE_TIMEOUT {
                        tracing::info!(display = self.number, "stopping idle Xwayland bridge");
                        self.stop_bridge();
                    }
                }
                // An unreadable socket table must never stop a server that
                // may still have clients.
                Some(_) | None => self.idle_since = None,
            }
            return false;
        }
        if !has_pending_connection(&self.listener) {
            return false;
        }
        match spawn(self) {
            Ok((child, keyboard)) => {
                tracing::info!(
                    display = self.number,
                    "starting Xwayland bridge for its first client"
                );
                self.bridge = Some(Bridge {
                    child,
                    started: now,
                    keyboard,
                    keyboard_update: None,
                    pending_keyboard: None,
                });
                self.idle_since = None;
                false
            }
            Err(error) => {
                tracing::warn!(%error, display = self.number, "could not start Xwayland bridge");
                self.rapid_exits = self.rapid_exits.saturating_add(1);
                self.rapid_exits >= MAX_RAPID_EXITS
            }
        }
    }

    fn stop_bridge(&mut self) {
        self.idle_since = None;
        self.idle_checked = None;
        self.composer_asked = None;
        let Some(mut bridge) = self.bridge.take() else {
            return;
        };
        if let Some(mut update) = bridge.keyboard_update.take() {
            let _ = update.child.kill();
            let _ = update.child.wait();
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
                %error,
                "Xwayland: could not start keyboard settings update; preserving the working keymap"
            ),
        }
    }
}

impl Bridge {
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

/// How the supervisor asks the event loop to wake for a display's socket.
pub(crate) trait ListenerWatch {
    type Token;
    fn watch(&mut self, listener: &UnixListener) -> Result<Self::Token, String>;
    fn set_watching(&mut self, token: &Self::Token, watching: bool);
    fn unwatch(&mut self, token: Self::Token);
}

impl<D: 'static> ListenerWatch for LoopHandle<'static, D> {
    type Token = RegistrationToken;

    fn watch(&mut self, listener: &UnixListener) -> Result<RegistrationToken, String> {
        let fd: OwnedFd = listener
            .as_fd()
            .try_clone_to_owned()
            .map_err(|error| format!("duplicating X11 listener: {error}"))?;
        // Level-triggered and never accepted here: the wake only makes the
        // loop tick, and the supervisor disables it while a bridge accepts.
        self.insert_source(Generic::new(fd, Interest::READ, Mode::Level), |_, _, _| {
            Ok(PostAction::Continue)
        })
        .map_err(|error| format!("watching X11 listener: {error}"))
    }

    fn set_watching(&mut self, token: &RegistrationToken, watching: bool) {
        let result = if watching {
            self.enable(token)
        } else {
            self.disable(token)
        };
        if let Err(error) = result {
            tracing::warn!(%error, watching, "could not update X11 listener wake");
        }
    }

    fn unwatch(&mut self, token: RegistrationToken) {
        self.remove(token);
    }
}

/// Owns the workspace displays' lifecycle on the compositor thread.
pub(crate) struct XwaylandSupervisor<W: ListenerWatch> {
    watch: W,
    wayland_socket: OsString,
    /// Registration and current watching state of each display's socket.
    watched: HashMap<String, (W::Token, bool)>,
}

impl<W: ListenerWatch> XwaylandSupervisor<W> {
    /// Make X11 available to host launches. Nothing is bound or spawned
    /// until a launch asks for a workspace's display.
    pub(crate) fn start(watch: W, wake: calloop::ping::Ping, wayland_socket: &OsStr) -> Self {
        match Paths::session() {
            Ok(paths) => {
                let satellite = resolve_satellite();
                if let Err(error) = &satellite {
                    tracing::warn!(%error, "X11 apps are unavailable");
                }
                let mut registry = registry();
                registry.displays = Some(Displays::new(paths, satellite));
                registry.wake = Some(wake);
            }
            Err(error) => tracing::warn!(%error, "X11 apps are unavailable"),
        }
        Self {
            watch,
            wayland_socket: wayland_socket.to_os_string(),
            watched: HashMap::new(),
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "only a running supervisor's displays have X servers to update"
    )]
    pub(crate) fn reload_keyboard_settings(&self) {
        let keyboard = KeyboardSettings::load();
        if let Some(displays) = registry().displays.as_mut() {
            displays.reload_keyboard_settings(&keyboard);
        }
    }

    /// Poll from the compositor's frame loop, before launch actions run, so
    /// a launch in this tick sees the current set of displays.
    pub(crate) fn poll(&mut self, chrome: &crate::shell::chrome_state::Chrome) {
        let mut registry = registry();
        let Some(displays) = registry.displays.as_mut() else {
            return;
        };
        if displays.workspaces.is_empty()
            && displays.unavailable.is_empty()
            && self.watched.is_empty()
        {
            return;
        }
        let live = live_workspaces(chrome);
        let wayland_socket = self.wayland_socket.clone();
        let satellite = displays.satellite.clone();
        let mut spawn = |display: &WorkspaceDisplay| match satellite.as_ref() {
            Ok(binary) => spawn_satellite(binary, &wayland_socket, display),
            Err(reason) => Err(reason.clone()),
        };
        let mut composer = ComposerClients::current();
        let exhausted = self.sync(displays, &live, Instant::now(), &mut spawn, &mut composer);
        if !composer.requests.is_empty() {
            let authorities: Vec<PathBuf> = displays
                .workspaces
                .values()
                .map(|display| display.authority.clone())
                .collect();
            let target = super::xwayland_composer::composer_authority_path(
                &crate::shell::launcher::runtime_dir(),
            );
            match super::xwayland_composer::write_composer_authority(&target, &authorities) {
                Ok(()) => {
                    for display in composer.requests.drain(..) {
                        super::xwayland_composer::request_connection(display);
                    }
                }
                Err(error) => tracing::warn!(%error, "composer cannot reach X11 displays"),
            }
        }
        drop(registry);
        for workspace in exhausted {
            tracing::warn!(%workspace, "X11 compatibility unavailable in workspace");
            announce_unavailable(chrome);
        }
    }

    /// Bring displays and loop wakes in line with the open workspaces.
    fn sync(
        &mut self,
        displays: &mut Displays,
        live: &HashSet<String>,
        now: Instant,
        spawn: &mut Spawn<'_>,
        composer: &mut ComposerClients,
    ) -> Vec<String> {
        displays.retain_workspaces(live);
        let exhausted = displays.poll_with(now, spawn, composer);
        let gone: Vec<String> = self
            .watched
            .keys()
            .filter(|workspace| !displays.workspaces.contains_key(*workspace))
            .cloned()
            .collect();
        for workspace in gone {
            if let Some((token, _)) = self.watched.remove(&workspace) {
                self.watch.unwatch(token);
            }
        }
        for (workspace, display) in &displays.workspaces {
            let wanted = display.wants_watch();
            if let Some((token, watching)) = self.watched.get_mut(workspace) {
                if *watching != wanted {
                    self.watch.set_watching(token, wanted);
                    *watching = wanted;
                }
                continue;
            }
            match self.watch.watch(&display.listener) {
                Ok(token) => {
                    if !wanted {
                        self.watch.set_watching(&token, false);
                    }
                    self.watched.insert(workspace.clone(), (token, wanted));
                }
                Err(error) => tracing::warn!(%error, "X11 display cannot wake the compositor"),
            }
        }
        exhausted
    }
}

impl<W: ListenerWatch> Drop for XwaylandSupervisor<W> {
    fn drop(&mut self) {
        for (_, (token, _)) in self.watched.drain() {
            self.watch.unwatch(token);
        }
        let mut registry = registry();
        registry.displays = None;
        registry.wake = None;
    }
}

fn live_workspaces(chrome: &crate::shell::chrome_state::Chrome) -> HashSet<String> {
    let state = chrome.borrow();
    let tree = state.tree();
    tree.desktop
        .workspaces
        .iter()
        .map(|workspace| workspace.room.to_string())
        .collect()
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
    let mut args = vec![OsString::from("-layout"), OsString::from(layout)];
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
    // SAFETY: one valid pollfd for a descriptor `listener` keeps open, and a
    // zero timeout never blocks the compositor thread.
    let ready = unsafe { libc::poll(&raw mut descriptor, 1, 0) };
    ready > 0 && descriptor.revents & libc::POLLIN != 0
}

fn write_xauthority(path: &Path, display: u16) -> Result<(), String> {
    let mut cookie = [0_u8; 16];
    UnwrapErr(SysRng).fill_bytes(&mut cookie);
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
        .and_then(|()| file.sync_all())
        .map_err(|error| format!("writing Xauthority: {error}"))
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

    fn sleeper() -> Result<(Child, KeyboardSettings), String> {
        let child = Command::new("/usr/bin/sleep")
            .arg("30")
            .spawn()
            .map_err(|error| error.to_string())?;
        Ok((child, KeyboardSettings::default()))
    }

    fn connect(session: &X11Session, displays: &Displays) -> UnixStream {
        let number = session.display.trim_start_matches(':');
        UnixStream::connect(displays.paths.sockets.join(format!("X{number}"))).unwrap()
    }

    #[derive(Default)]
    struct FakeWatch {
        next: u32,
        live: HashMap<u32, bool>,
    }

    impl ListenerWatch for &mut FakeWatch {
        type Token = u32;
        fn watch(&mut self, _listener: &UnixListener) -> Result<u32, String> {
            self.next = self.next.saturating_add(1);
            self.live.insert(self.next, true);
            Ok(self.next)
        }
        fn set_watching(&mut self, token: &u32, watching: bool) {
            self.live.insert(*token, watching);
        }
        fn unwatch(&mut self, token: u32) {
            self.live.remove(&token);
        }
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

    #[test]
    fn launch_environment_carries_only_the_owning_workspace_display() {
        let mut fixture = fixture();
        let (a, _) = fixture.displays.session_for("room-a").unwrap();
        let (b, _) = fixture.displays.session_for("room-b").unwrap();
        let app = crate::shell::host_apps::HostApp {
            desktop_id: "com.valvesoftware.Steam".into(),
            name: "Steam".into(),
            source_label: "System",
            categories: Vec::new(),
            keywords: Vec::new(),
            mime_types: Vec::new(),
            terminal: false,
            icon: None,
            window_identities: vec!["steam".into()],
        };
        let command = |x11: Option<&X11Session>| {
            crate::shell::host_apps::build_launch_command(
                &app,
                Path::new("/usr/bin/tilo-launcher"),
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
            fixture.displays.workspaces["room-a"].wants_watch(),
            "no client, no X server"
        );

        let _client = connect(&session, &fixture.displays);
        fixture.displays.poll(Instant::now(), &mut spawn);
        fixture.displays.poll(Instant::now(), &mut spawn);
        assert_eq!(spawned.get(), 1, "one bridge per display, started once");
        assert!(!fixture.displays.workspaces["room-a"].wants_watch());
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
        assert!(!fixture.displays.workspaces["room-a"].wants_watch());
        fixture.displays.poll(start + IDLE_TIMEOUT, &mut spawn);
        assert!(
            fixture.displays.workspaces["room-a"].wants_watch(),
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
    fn the_supervisor_watches_only_displays_without_a_running_bridge() {
        let mut fixture = fixture();
        let mut fake = FakeWatch::default();
        let mut supervisor = XwaylandSupervisor {
            watch: &mut fake,
            wayland_socket: OsString::from("wayland-test"),
            watched: HashMap::new(),
        };
        let (session, _) = fixture.displays.session_for("room-a").unwrap();
        let now = Instant::now();
        supervisor.sync(
            &mut fixture.displays,
            &live(&["room-a"]),
            now,
            &mut |_| sleeper(),
            &mut ComposerClients::for_group(None),
        );
        assert!(supervisor.watched["room-a"].1);

        let _client = connect(&session, &fixture.displays);
        supervisor.sync(
            &mut fixture.displays,
            &live(&["room-a"]),
            now,
            &mut |_| sleeper(),
            &mut ComposerClients::for_group(None),
        );
        assert!(
            !supervisor.watched["room-a"].1,
            "a running bridge accepts its own clients"
        );

        supervisor.sync(
            &mut fixture.displays,
            &live(&[]),
            now,
            &mut |_| sleeper(),
            &mut ComposerClients::for_group(None),
        );
        assert!(supervisor.watched.is_empty());
        drop(supervisor);
        assert!(
            fake.live.is_empty(),
            "closed workspaces leave no wake source"
        );
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
            .poll_with(start, &mut |_| sleeper(), &mut composer);
        assert!(composer.requests.is_empty());

        let client = connect(&session, &fixture.displays);
        let mut composer = ComposerClients::for_group(composer_group);
        fixture
            .displays
            .poll_with(start, &mut |_| sleeper(), &mut composer);
        assert!(
            composer.requests.is_empty(),
            "a server that just started is given time to accept"
        );

        let later = start + COMPOSER_CONNECT_DELAY;
        let mut composer = ComposerClients::for_group(composer_group);
        fixture
            .displays
            .poll_with(later, &mut |_| sleeper(), &mut composer);
        assert_eq!(composer.requests, std::slice::from_ref(&session.display));
        let mut composer = ComposerClients::for_group(composer_group);
        fixture.displays.poll_with(
            later + IDLE_CHECK_INTERVAL,
            &mut |_| sleeper(),
            &mut composer,
        );
        assert!(composer.requests.is_empty(), "asked once per server");

        let mut restarted = ComposerClients::for_group(Some(4343));
        fixture.displays.poll_with(
            later + IDLE_CHECK_INTERVAL * 2,
            &mut |_| sleeper(),
            &mut restarted,
        );
        assert_eq!(
            restarted.requests,
            std::slice::from_ref(&session.display),
            "a restarted composer is asked again"
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
                ["-layout", "us", "-option", ""].map(OsString::from)
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

    #[test]
    fn keyboard_update_waits_for_a_running_server_and_coalesces_to_the_latest() {
        let mut fixture = fixture();
        let (session, _) = fixture.displays.session_for("room-a").unwrap();
        let temp = tempfile::tempdir().unwrap();
        let slow_setxkbmap = temp.path().join("setxkbmap");
        fs::write(&slow_setxkbmap, "#!/bin/sh\nexec /usr/bin/sleep 30\n").unwrap();
        fs::set_permissions(&slow_setxkbmap, fs::Permissions::from_mode(0o700)).unwrap();
        let german = KeyboardSettings {
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
