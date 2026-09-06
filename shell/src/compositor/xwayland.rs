//! Session-wide rootless Xwayland compatibility via `xwayland-satellite`.
//!
//! qOS remains a Wayland compositor and does not grow an in-process XWM. The
//! satellite is itself a Wayland client and maps each legacy X11 window to an
//! ordinary `xdg_toplevel`, so existing host applications participate in the
//! same layout. Failure is non-fatal: native Wayland software remains usable.

use std::ffi::{OsStr, OsString};
use std::fs::{self, File, OpenOptions};
use std::io::Write as _;
use std::os::unix::fs::{
    FileTypeExt as _, MetadataExt as _, OpenOptionsExt as _, PermissionsExt as _,
};
use std::os::unix::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::{Duration, Instant};

use rand_core::{OsRng, RngCore as _};

const DISPLAY_ATTEMPTS: usize = 64;
const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
/// Restarting a compatibility bridge forever turns one crashing helper into a
/// busy loop. Three attempts give a transient host/service failure time to
/// settle while making the eventual unavailable state deterministic.
const MAX_RECOVERY_ATTEMPTS: u8 = 3;
const STABLE_UPTIME: Duration = Duration::from_secs(30);

/// Keep the compatibility server out of the security-hardened qOS image.
pub(crate) const fn enabled_for(mode: super::InstallMode) -> bool {
    matches!(mode, super::InstallMode::HostDesktop)
}

pub(crate) struct SessionXwayland {
    child: Child,
    runtime: PathBuf,
    display: String,
    authority: PathBuf,
    reservation: PathBuf,
    /// Publication is a compositor-thread action. A startup worker must never
    /// make its private result visible after its supervisor has gone away.
    published: bool,
}

/// Owns the one session-wide Xwayland bridge and its bounded recovery policy.
///
/// There is deliberately no second launcher or per-app bridge here. The
/// existing host-launch slot is unpublished as soon as the child exits, and a
/// successful replacement republishes one fresh authenticated connection.
pub(crate) struct XwaylandSupervisor {
    bridge: Option<SessionXwayland>,
    starting: Option<Receiver<Result<SessionXwayland, String>>>,
    consecutive_failures: u8,
    retry_at: Option<Instant>,
    ready_since: Option<Instant>,
    unavailable_announced: bool,
}

impl XwaylandSupervisor {
    pub(crate) fn start(wayland_socket: &OsStr) -> Self {
        let mut supervisor = Self {
            bridge: None,
            starting: None,
            consecutive_failures: 0,
            retry_at: None,
            ready_since: None,
            unavailable_announced: false,
        };
        supervisor.start_async(wayland_socket, "starting");
        supervisor
    }

    /// Poll from the compositor's frame loop. Retrying only happens after the
    /// old child has exited and its X11 launch environment has been revoked.
    /// That ordering matters for Electron: a renderer launched after a crash
    /// must never inherit an authority file for the dead display.
    pub(crate) fn poll(
        &mut self,
        wayland_socket: &OsStr,
        chrome: &crate::shell::chrome_state::Chrome,
    ) {
        if let Some(starting) = self.starting.as_ref() {
            match starting.try_recv() {
                Ok(Ok(mut bridge)) => {
                    self.starting = None;
                    // This is the sole point that publishes DISPLAY and
                    // XAUTHORITY. If the receiver was dropped, the worker's
                    // unpublished bridge is simply dropped and cleaned up.
                    bridge.publish();
                    self.bridge = Some(bridge);
                    self.retry_at = None;
                    self.ready_since = Some(Instant::now());
                    self.unavailable_announced = false;
                    tracing::info!("rootless Xwayland compatibility bridge ready");
                }
                Ok(Err(error)) => {
                    self.starting = None;
                    tracing::warn!(%error, "rootless Xwayland compatibility startup failed");
                    self.record_failure(Instant::now(), chrome);
                }
                Err(TryRecvError::Empty) => return,
                Err(TryRecvError::Disconnected) => {
                    self.starting = None;
                    tracing::warn!("rootless Xwayland startup worker disconnected");
                    self.record_failure(Instant::now(), chrome);
                }
            }
        }

        if let Some(bridge) = self.bridge.as_mut() {
            match bridge.poll_exit() {
                Ok(Some(status)) => {
                    tracing::warn!(%status, "rootless Xwayland compatibility bridge exited");
                    self.bridge = None;
                    self.record_failure(Instant::now(), chrome);
                }
                Ok(None) => return,
                Err(error) => {
                    // Do not start a second server when the kernel cannot tell
                    // us whether the first one is alive. The already-published
                    // launch environment remains valid until an actual exit.
                    tracing::warn!(%error, "could not inspect rootless Xwayland compatibility bridge");
                    return;
                }
            }
        }

        let Some(retry_at) = self.retry_at else {
            return;
        };
        if Instant::now() < retry_at {
            return;
        }
        self.retry_at = None;
        self.start_async(wayland_socket, "recovering");
    }

    /// Launch the potentially five-second readiness probe off the compositor
    /// thread. `poll` only uses `try_recv`, so input and rendering continue
    /// while satellite waits for Xwayland to create its socket.
    fn start_async(&mut self, wayland_socket: &OsStr, phase: &'static str) {
        debug_assert!(self.bridge.is_none());
        debug_assert!(self.starting.is_none());
        let wayland_socket = wayland_socket.to_os_string();
        let (sender, receiver) = mpsc::channel();
        let worker_sender = sender.clone();
        match std::thread::Builder::new()
            .name("qos-xwayland-start".into())
            .spawn(move || {
                let result = SessionXwayland::start_unpublished(&wayland_socket);
                // If the supervisor was dropped during startup, `send` gives
                // the unpublished bridge back in its error and dropping it
                // kills/reaps the child without touching the launcher slot.
                let _ = worker_sender.send(result);
            }) {
            Ok(_worker) => self.starting = Some(receiver),
            Err(error) => {
                tracing::warn!(%error, %phase, "could not start Xwayland recovery worker");
                let _ = sender.send(Err(format!("starting Xwayland recovery worker: {error}")));
                self.starting = Some(receiver);
            }
        }
    }

    fn record_failure(&mut self, now: Instant, chrome: &crate::shell::chrome_state::Chrome) {
        if self
            .ready_since
            .is_some_and(|ready_since| now.saturating_duration_since(ready_since) >= STABLE_UPTIME)
        {
            // A bridge that stayed up for a normal session interval proved the
            // prior recovery healthy. Its later exit starts a fresh budget.
            self.consecutive_failures = 0;
        }
        self.ready_since = None;
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        if self.consecutive_failures >= MAX_RECOVERY_ATTEMPTS {
            self.retry_at = None;
            self.announce_unavailable(chrome);
            return;
        }
        self.retry_at = now.checked_add(retry_delay(self.consecutive_failures));
    }

    fn announce_unavailable(&mut self, chrome: &crate::shell::chrome_state::Chrome) {
        if self.unavailable_announced {
            return;
        }
        self.unavailable_announced = true;
        let mut notification = crate::shell::notification::Notification::new(
            "X11 compatibility unavailable",
            "Legacy X11 apps can’t open right now. qOS could not restart the compatibility service; use a Wayland app or restart the desktop session.",
        )
            .with_severity(crate::shell::notification::NotificationSeverity::Error)
            .with_app_name("System")
            .with_dismiss_after(crate::shell::notification::PERSISTENT_DISMISS);
        // A broken compatibility service is machine state, not an app's
        // social notification. Do Not Disturb must not conceal the only
        // explanation for why legacy applications no longer launch.
        notification.bypass_dnd = true;
        chrome.borrow_mut().push_notification_full(notification);
    }
}

fn retry_delay(consecutive_failures: u8) -> Duration {
    // 1s, 2s; the third failure stops and tells the person what to do.
    Duration::from_secs(1_u64 << u32::from(consecutive_failures.saturating_sub(1).min(1)))
}

impl SessionXwayland {
    fn start_unpublished(wayland_socket: &OsStr) -> Result<Self, String> {
        let binary = satellite_binary().ok_or_else(|| {
            "xwayland-satellite is not installed; X11-only host applications are unavailable"
                .to_string()
        })?;
        let runtime_root = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .ok_or_else(|| "XDG_RUNTIME_DIR is not an absolute path".to_string())?;
        let runtime = runtime_root.join("qos-session/xwayland");
        fs::create_dir_all(&runtime)
            .map_err(|error| format!("creating {}: {error}", runtime.display()))?;
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))
            .map_err(|error| format!("protecting {}: {error}", runtime.display()))?;

        let (display_number, reservation_file, reservation) = reserve_display(&runtime)?;
        let authority = runtime.join(format!("Xauthority-{display_number}"));
        if let Err(error) = write_xauthority(&authority, display_number) {
            drop(reservation_file);
            let _ = fs::remove_file(&reservation);
            return Err(error);
        }
        let x_display = format!(":{display_number}");
        let keyboard = super::backend::drm::input_config::KeyboardSettings::load();
        let runtime_path = satellite_runtime_path(&binary, std::env::var_os("PATH").as_deref())?;

        let mut command = Command::new(&binary);
        command
            .arg(&x_display)
            .args(["-auth", authority.to_string_lossy().as_ref()])
            .args(["-nolisten", "tcp"])
            .env("WAYLAND_DISPLAY", wayland_socket)
            // Xwayland compiles its own XKB map before it receives any input.
            // In a display-manager session there need not be PAM-provided
            // XKB_DEFAULT_* variables, so inheriting the process environment
            // silently gives every legacy host app US regardless of the qOS
            // seat keymap. Keep both sides on the same validated settings.
            .env("XKB_DEFAULT_LAYOUT", &keyboard.layouts)
            .env("XKB_DEFAULT_VARIANT", &keyboard.variants)
            // Satellite deliberately execs a bare `Xwayland`. Resolve the
            // sibling shipped in the same runtime first so a container/host
            // PATH mix (or a user-local shadow) cannot pair it with an ABI-
            // incompatible X server. Development fallbacks remain available
            // after that first directory.
            .env("PATH", runtime_path)
            .env(
                "XKB_DEFAULT_OPTIONS",
                keyboard.options.as_deref().unwrap_or(""),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        // SAFETY: `pre_exec` runs after fork and before exec. `prctl` is one
        // async-signal-safe syscall and captures no heap-backed state. The
        // death signal prevents a compositor crash from leaving an X server
        // accepting connections after its Wayland parent is gone.
        unsafe {
            command.pre_exec(|| {
                if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = match command.spawn() {
            Ok(child) => child,
            Err(error) => {
                let _ = fs::remove_file(&authority);
                let _ = fs::remove_file(&reservation);
                return Err(format!("starting {}: {error}", binary.display()));
            }
        };
        drop(reservation_file);

        if let Err(error) = wait_until_ready(&mut child, display_number) {
            if matches!(child.try_wait(), Ok(None)) {
                let _ = child.kill();
            }
            let _ = child.wait();
            let _ = fs::remove_file(&authority);
            let _ = fs::remove_file(&reservation);
            return Err(error);
        }

        tracing::info!(display = %x_display, binary = %binary.display(), "rootless Xwayland compatibility bridge starting");
        Ok(Self {
            child,
            runtime,
            display: x_display,
            authority,
            reservation,
            published: false,
        })
    }

    fn publish(&mut self) {
        debug_assert!(!self.published);
        crate::shell::host_apps::set_x11_session(Some(crate::shell::host_apps::X11Session {
            display: self.display.clone(),
            authority: self.authority.clone(),
        }));
        self.published = true;
    }

    /// Reap an exited bridge and immediately revoke its connection details
    /// from future host launches. `None` means the child is still alive.
    fn poll_exit(&mut self) -> Result<Option<String>, String> {
        match self.child.try_wait() {
            Ok(Some(status)) => {
                self.clear_launch_environment();
                self.remove_runtime_files();
                Ok(Some(status.to_string()))
            }
            Ok(None) => Ok(None),
            Err(error) => Err(format!("checking xwayland-satellite: {error}")),
        }
    }

    fn clear_launch_environment(&self) {
        if self.published {
            crate::shell::host_apps::set_x11_session(None);
        }
    }

    fn remove_runtime_files(&self) {
        let _ = fs::remove_file(&self.authority);
        let _ = fs::remove_file(&self.reservation);
        let _ = fs::remove_dir(&self.runtime);
    }
}

fn wait_until_ready(child: &mut Child, display: u16) -> Result<(), String> {
    let socket = PathBuf::from(format!("/tmp/.X11-unix/X{display}"));
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return Err(format!(
                    "xwayland-satellite exited before opening display :{display} ({status})"
                ));
            }
            Ok(None) => {}
            Err(error) => return Err(format!("checking xwayland-satellite: {error}")),
        }
        if x_socket_ready(&socket) {
            return Ok(());
        }
        if started.elapsed() >= STARTUP_TIMEOUT {
            return Err(format!(
                "xwayland-satellite did not open display :{display} within {} seconds",
                STARTUP_TIMEOUT.as_secs()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Refuse a symlink, regular file, or another user's socket at the globally
/// shared X11 path. The display number is reserved inside this user's runtime
/// directory, but `/tmp/.X11-unix` is intentionally shared by every login.
fn x_socket_ready(path: &Path) -> bool {
    // SAFETY: `geteuid` is a side-effect-free process credential query.
    let current_uid = unsafe { libc::geteuid() };
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_socket() && metadata.uid() == current_uid)
}

impl Drop for SessionXwayland {
    fn drop(&mut self) {
        self.clear_launch_environment();
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
        self.remove_runtime_files();
    }
}

fn satellite_binary() -> Option<PathBuf> {
    // The native binary never accepts an arbitrary satellite from its
    // environment.  Hosted integration tests, however, need to validate a
    // candidate upstream satellite against the same private compositor they
    // start, without replacing the installed host binary.
    #[cfg(feature = "hosted-test-control")]
    if let Some(candidate) = std::env::var_os("QOS_TEST_XWAYLAND_SATELLITE") {
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

fn reserve_display(runtime: &Path) -> Result<(u16, File, PathBuf), String> {
    let mut random = [0_u8; 2];
    OsRng.fill_bytes(&mut random);
    let first = 1000_u16.saturating_add(u16::from_be_bytes(random) % 20_000);
    for offset in 0..DISPLAY_ATTEMPTS {
        let display = first.wrapping_add(u16::try_from(offset).unwrap_or(0));
        if Path::new(&format!("/tmp/.X11-unix/X{display}")).exists()
            || Path::new(&format!("/tmp/.X{display}-lock")).exists()
        {
            continue;
        }
        let reservation = runtime.join(format!("display-{display}.lock"));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&reservation)
        {
            Ok(file) => return Ok((display, file, reservation)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(format!("reserving X display: {error}")),
        }
    }
    Err("no unused X display number is available".to_string())
}

fn write_xauthority(path: &Path, display: u16) -> Result<(), String> {
    let mut cookie = [0_u8; 16];
    OsRng.fill_bytes(&mut cookie);
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
    use std::os::unix::net::UnixListener;

    #[test]
    fn compatibility_is_confined_to_add_on_desktop_sessions() {
        assert!(enabled_for(super::super::InstallMode::HostDesktop));
        assert!(!enabled_for(super::super::InstallMode::FullSystem));
    }

    #[test]
    fn keyboard_settings_retain_layout_variant_and_options_for_xwayland() {
        let section = serde_json::json!({
            "keyboard_layout": "de:nodeadkeys",
            "keyboard_layouts": ["de:nodeadkeys"],
            "compose_key": "rwin"
        })
        .as_object()
        .unwrap()
        .clone();
        let settings =
            crate::compositor::backend::drm::input_config::KeyboardSettings::from_section(&section);
        assert_eq!(settings.layouts, "de");
        assert_eq!(settings.variants, "nodeadkeys");
        assert_eq!(settings.options.as_deref(), Some("compose:rwin"));
    }

    #[test]
    fn satellite_and_xwayland_are_resolved_from_one_runtime_first() {
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

    #[test]
    fn authority_is_private_and_contains_no_text_cookie() {
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

    #[test]
    fn readiness_requires_an_owned_unix_socket() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("X1234");
        fs::write(&path, b"not a socket").unwrap();
        assert!(!x_socket_ready(&path));
        fs::remove_file(&path).unwrap();

        let listener = UnixListener::bind(&path).unwrap();
        assert!(x_socket_ready(&path));
        drop(listener);
    }

    #[test]
    fn pending_startup_never_blocks_the_compositor_poll() {
        let (_sender, receiver) = mpsc::channel::<Result<SessionXwayland, String>>();
        let mut supervisor = XwaylandSupervisor {
            bridge: None,
            starting: Some(receiver),
            consecutive_failures: 0,
            retry_at: None,
            ready_since: None,
            unavailable_announced: false,
        };
        let chrome = crate::shell::chrome_state::ChromeState::new(
            crate::shell::state::WorkspaceTree::default(),
        );
        let started = Instant::now();
        supervisor.poll(OsStr::new("wayland-test"), &chrome);
        assert!(
            started.elapsed() < Duration::from_millis(50),
            "poll must only try_recv while the startup worker waits for readiness"
        );
    }

    #[test]
    fn stable_uptime_resets_the_rapid_failure_budget_but_crash_loops_exhaust_it() {
        let chrome = crate::shell::chrome_state::ChromeState::new(
            crate::shell::state::WorkspaceTree::default(),
        );
        let now = Instant::now();
        let mut supervisor = XwaylandSupervisor {
            bridge: None,
            starting: None,
            consecutive_failures: MAX_RECOVERY_ATTEMPTS - 1,
            retry_at: None,
            ready_since: now.checked_sub(STABLE_UPTIME),
            unavailable_announced: false,
        };
        supervisor.record_failure(now, &chrome);
        assert_eq!(supervisor.consecutive_failures, 1);
        assert_eq!(supervisor.retry_at, now.checked_add(retry_delay(1)));

        supervisor.ready_since = Some(now);
        supervisor.consecutive_failures = MAX_RECOVERY_ATTEMPTS - 1;
        supervisor.record_failure(now, &chrome);
        assert_eq!(supervisor.consecutive_failures, MAX_RECOVERY_ATTEMPTS);
        assert!(supervisor.retry_at.is_none());
        assert!(supervisor.unavailable_announced);
    }

    #[test]
    #[serial_test::serial]
    fn abandoned_startup_result_cannot_clear_a_newer_x11_session() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = temp.path().join("xwayland");
        fs::create_dir(&runtime).unwrap();
        let newer_authority = temp.path().join("newer-authority");
        crate::shell::host_apps::set_x11_session(Some(crate::shell::host_apps::X11Session {
            display: ":5678".into(),
            authority: newer_authority.clone(),
        }));

        let child = Command::new("/bin/sleep").arg("30").spawn().unwrap();
        let unpublished = SessionXwayland {
            child,
            runtime,
            display: ":1234".into(),
            authority: temp.path().join("old-authority"),
            reservation: temp.path().join("old-reservation"),
            published: false,
        };
        let (sender, receiver) = mpsc::channel::<Result<SessionXwayland, String>>();
        drop(receiver);
        assert!(
            sender.send(Ok(unpublished)).is_err(),
            "dropping a supervisor disconnects its pending startup result"
        );
        assert_eq!(
            crate::shell::host_apps::x11_session(),
            Some(crate::shell::host_apps::X11Session {
                display: ":5678".into(),
                authority: newer_authority,
            }),
            "an unpublished result must not revoke a newer live bridge"
        );
        crate::shell::host_apps::set_x11_session(None);
    }

    #[test]
    #[serial_test::serial]
    fn an_exited_ready_bridge_revokes_its_x11_launch_environment() {
        let temp = tempfile::tempdir().unwrap();
        let runtime = temp.path().join("xwayland");
        fs::create_dir(&runtime).unwrap();
        let authority = runtime.join("Xauthority-1234");
        let reservation = runtime.join("display-1234.lock");
        fs::write(&authority, b"test authority").unwrap();
        fs::write(&reservation, b"reserved").unwrap();
        crate::shell::host_apps::set_x11_session(Some(crate::shell::host_apps::X11Session {
            display: ":1234".into(),
            authority: authority.clone(),
        }));

        // This child represents a bridge that completed readiness and then
        // died. Polling is the production liveness path, not a destructor-only
        // cleanup, so a later launch cannot race stale credentials.
        let child = Command::new("/bin/sh")
            .args(["-c", "exit 23"])
            .spawn()
            .unwrap();
        let mut bridge = SessionXwayland {
            child,
            runtime,
            display: ":1234".into(),
            authority: authority.clone(),
            reservation: reservation.clone(),
            published: true,
        };
        let status = (0..50)
            .find_map(|_| match bridge.poll_exit().unwrap() {
                Some(status) => Some(status),
                None => {
                    std::thread::sleep(Duration::from_millis(10));
                    None
                }
            })
            .expect("the ready bridge child exits");
        assert!(status.contains("23"));
        assert!(crate::shell::host_apps::x11_session().is_none());
        assert!(!authority.exists(), "dead bridge authority is removed");
        assert!(!reservation.exists(), "dead bridge reservation is removed");
    }
}
