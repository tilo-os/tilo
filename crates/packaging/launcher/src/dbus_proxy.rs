//! Filtered D-Bus proxy for sandboxed apps.
//!
//! Motivation. When a manifest declares a `via-host` XDG portal, the
//! sandboxed app needs to reach a well-defined slice of the host
//! session bus — the portal service and (usually) fd.o notifications
//! plus MPRIS. Bind-mounting the raw session-bus socket exposes the
//! FULL bus though — every daemon the user's session registered,
//! `org.gnome.SessionManager`, evolution, whatever — and any of those
//! becomes reachable from inside the sandbox. That's the surface
//! Flatpak solved with `xdg-dbus-proxy --filter`: a small forwarder
//! that only relays messages matching an allowlist, running as a
//! per-sandbox helper.
//!
//! What we do here:
//! - Locate `xdg-dbus-proxy` (Flatpak's binary, packaged in
//!   `flatpak-xdg-utils`). If it's missing, callers fall back to the
//!   legacy raw-bind path.
//! - Spawn one proxy per sandbox: allow `org.freedesktop.DBus` (the
//!   bus itself; without this the Hello handshake fails), the entire
//!   `org.freedesktop.portal.*` tree, `org.freedesktop.Notifications`,
//!   and the `org.mpris.MediaPlayer2.*` tree.
//! - Listen on a per-launcher socket under `/run/user/<uid>/qos/
//!   dbus-proxy/<pid>.sock`. The socket path is stable for the
//!   lifetime of the launcher process and unique across concurrent
//!   launches.
//! - Return an RAII guard. When the guard drops (i.e. after the
//!   sandbox exits) we `SIGTERM` the proxy and unlink the socket.
//!
//! Upstream bus address. `xdg-dbus-proxy` takes the upstream as its
//! first positional arg and understands every transport libdbus
//! understands — including `unix:abstract=...`. That's why routing
//! through the proxy is strictly more capable than the raw-bind
//! path, which can only mount `unix:path=...` addresses.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Filter args passed after `SOURCE DEST` to `xdg-dbus-proxy`.
///
/// Explicit allowlist. The only wildcard is `org.freedesktop.portal.*`
/// because portals are a growing family of well-known names under a
/// stable prefix. Everything else is a fully-qualified name.
///
/// * `--filter` — turn on filtering; without this everything passes
///   through (the exact behaviour we're trying to prevent).
/// * `--see=org.freedesktop.DBus` — the bus daemon itself, needed for
///   `Hello`, `AddMatch`, `NameHasOwner`, etc.
/// * `--talk=org.freedesktop.portal.*` — every XDG portal (file
///   chooser, screencast, secret, notification, print, …).
/// * `--talk=org.freedesktop.Notifications` — fd.o notifications.
/// * `--talk=org.mpris.MediaPlayer2.*` — media playback control.
pub const DEFAULT_FILTER_ARGS: &[&str] = &[
    "--filter",
    "--see=org.freedesktop.DBus",
    "--talk=org.freedesktop.portal.*",
    "--talk=org.freedesktop.Notifications",
    "--talk=org.mpris.MediaPlayer2.*",
];

/// Env-var override for the `xdg-dbus-proxy` binary path. Used by:
/// - the test suite (points at a mock proxy fixture);
/// - installations that ship the binary in a non-standard location
///   (e.g. via a container image where the flatpak-xdg-utils package
///   went under `/usr/libexec`).
pub const PROXY_BIN_ENV: &str = "QOS_LAUNCHER_DBUS_PROXY_BIN";

/// Locate the `xdg-dbus-proxy` binary. Returns `None` when nothing
/// matches — callers log a warning and fall back to the raw-bind
/// path.
///
/// Lookup order:
/// 1. `$QOS_LAUNCHER_DBUS_PROXY_BIN` — test/override.
/// 2. `xdg-dbus-proxy` on `$PATH`.
/// 3. `/usr/libexec/flatpak-xdg-utils/xdg-dbus-proxy` — Fedora's
///    packaging location.
/// 4. `/usr/lib/flatpak-xdg-utils/xdg-dbus-proxy` — some Debian
///    derivatives.
pub fn find_proxy_binary() -> Option<PathBuf> {
    if let Some(bin) = std::env::var_os(PROXY_BIN_ENV) {
        let p = PathBuf::from(bin);
        if p.is_file() {
            return Some(p);
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let cand = dir.join("xdg-dbus-proxy");
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    for cand in [
        "/usr/libexec/flatpak-xdg-utils/xdg-dbus-proxy",
        "/usr/lib/flatpak-xdg-utils/xdg-dbus-proxy",
    ] {
        let p = PathBuf::from(cand);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}

/// Host-side path where the per-launcher proxy listens.
///
/// `/run/user/<uid>/qos/dbus-proxy/<pid>.sock`. Directory-tree
/// created on demand by [`spawn_dbus_proxy`]. The `<pid>` disambiguates
/// concurrent launches (a launcher-per-app-launch is one instance of
/// this process).
pub fn proxy_socket_path(uid: u32, launcher_pid: u32) -> PathBuf {
    PathBuf::from(format!(
        "/run/user/{uid}/qos/dbus-proxy/{launcher_pid}.sock"
    ))
}

/// In-sandbox bus path. Matches Flatpak's convention: the sandboxed
/// app looks for `$DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/<uid>/bus`.
pub fn sandbox_bus_path(uid: u32) -> PathBuf {
    PathBuf::from(format!("/run/user/{uid}/bus"))
}

/// In-sandbox `DBUS_SESSION_BUS_ADDRESS` value for the proxy path.
pub fn sandbox_bus_env(uid: u32) -> String {
    format!("unix:path=/run/user/{uid}/bus")
}

/// Errors from [`spawn_dbus_proxy`].
#[derive(Debug, thiserror::Error)]
pub enum ProxyError {
    #[error("creating proxy socket directory {path}: {source}")]
    Mkdir {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("spawning xdg-dbus-proxy: {0}")]
    Spawn(#[source] io::Error),
    #[error(
        "xdg-dbus-proxy did not create its listener socket at {path} within {timeout:?}. \
         The proxy binary may have exited before setting up the socket — check its stderr."
    )]
    NotReady { path: PathBuf, timeout: Duration },
}

/// Spawn `xdg-dbus-proxy` with the standard filter set.
///
/// * `binary`: path returned by [`find_proxy_binary`].
/// * `bus_addr`: upstream address, i.e. the current
///   `$DBUS_SESSION_BUS_ADDRESS`. Can be `unix:path=...` OR
///   `unix:abstract=...` — the proxy resolves both.
/// * `socket_path`: where the proxy should listen (typically
///   [`proxy_socket_path`]).
///
/// Returns a [`DBusProxyGuard`] whose `Drop` `SIGTERM`s the child and
/// unlinks the socket. Callers should keep the guard alive until the
/// sandbox exits.
pub fn spawn_dbus_proxy(
    binary: &Path,
    bus_addr: &str,
    socket_path: &Path,
) -> Result<DBusProxyGuard, ProxyError> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ProxyError::Mkdir {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    // A stale socket from a previous run would make `bind()` fail
    // with EADDRINUSE. Remove best-effort.
    let _ = std::fs::remove_file(socket_path);

    let mut cmd = Command::new(binary);
    cmd.arg(bus_addr).arg(socket_path);
    for arg in DEFAULT_FILTER_ARGS {
        cmd.arg(arg);
    }
    let child = cmd.spawn().map_err(ProxyError::Spawn)?;
    let guard = DBusProxyGuard {
        child: Some(child),
        socket_path: socket_path.to_path_buf(),
    };

    // The socket appears asynchronously. Poll for its existence
    // with a bounded timeout — 2 s is generous; the real proxy is
    // ready in ~10 ms, and a mock proxy in a test even faster.
    match wait_for_socket(socket_path, Duration::from_secs(2)) {
        Ok(()) => Ok(guard),
        Err(_) => Err(ProxyError::NotReady {
            path: socket_path.to_path_buf(),
            timeout: Duration::from_secs(2),
        }),
    }
}

fn wait_for_socket(path: &Path, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        // `try_exists` catches both regular files (mock proxy) and
        // socket inodes (real proxy). `Path::exists` would too but
        // hides `EACCES`; the runtime dir is per-user so we expect
        // the check to succeed.
        if path.try_exists().unwrap_or(false) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("socket {} did not appear within {timeout:?}", path.display()),
    ))
}

/// RAII wrapper around a spawned `xdg-dbus-proxy`. `Drop` `SIGTERM`s
/// the child, waits briefly, and unlinks the socket. Idempotent —
/// callers that want to observe status can call [`Self::terminate`]
/// explicitly before drop.
pub struct DBusProxyGuard {
    child: Option<Child>,
    socket_path: PathBuf,
}

impl DBusProxyGuard {
    /// Path of the socket the proxy is listening on. Bind this into
    /// the sandbox at [`sandbox_bus_path`].
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// PID of the proxy child, or `None` if it was already reaped.
    /// Used by tests to assert the process is gone after Drop.
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(|c| c.id())
    }

    /// SIGTERM the proxy, wait up to 500 ms for it, then SIGKILL if
    /// it hasn't exited. Unlinks the socket unconditionally.
    ///
    /// Safe to call more than once; second and later calls are no-ops.
    pub fn terminate(&mut self) {
        if let Some(mut child) = self.child.take() {
            // SAFETY: `kill(2)` with SIGTERM on a live pid we own
            // is well-defined. Sending to a reaped pid returns
            // ESRCH which we ignore.
            unsafe {
                libc::kill(child.id() as i32, libc::SIGTERM);
            }
            let deadline = Instant::now() + Duration::from_millis(500);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    Err(_) => break,
                }
            }
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl Drop for DBusProxyGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proxy_socket_path_uses_uid_and_pid() {
        let p = proxy_socket_path(1000, 4242);
        assert_eq!(
            p,
            PathBuf::from("/run/user/1000/qos/dbus-proxy/4242.sock")
        );
    }

    #[test]
    fn sandbox_bus_env_matches_sandbox_bus_path() {
        assert_eq!(sandbox_bus_env(1000), "unix:path=/run/user/1000/bus");
        assert_eq!(sandbox_bus_path(1000), PathBuf::from("/run/user/1000/bus"));
    }

    #[test]
    fn find_proxy_binary_honours_env_override() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("xdg-dbus-proxy-mock");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        // Env var wins.
        unsafe { std::env::set_var(PROXY_BIN_ENV, &bin) };
        let found = find_proxy_binary().expect("env override");
        assert_eq!(found, bin);
        unsafe { std::env::remove_var(PROXY_BIN_ENV) };
    }

    #[test]
    fn find_proxy_binary_returns_none_when_env_points_at_nonexistent() {
        unsafe {
            std::env::set_var(PROXY_BIN_ENV, "/nonexistent/xdg-dbus-proxy");
            // Nuke PATH so the fallback can't find a real binary.
            std::env::set_var("PATH", "");
        }
        let found = find_proxy_binary();
        assert!(
            found.is_none(),
            "expected None with bad env + empty PATH, got {found:?}"
        );
        unsafe {
            std::env::remove_var(PROXY_BIN_ENV);
        }
    }

    /// Mock proxy fixture: writes a marker to `argv[2]` (the socket
    /// path) so callers waiting on socket existence see it, then
    /// installs a SIGTERM trap and sleeps. The `sh` inherits the
    /// signal disposition of the launcher (`SIG_DFL`), so SIGTERM
    /// kills it. We use plain `sleep 30` so `wait_for_socket`
    /// doesn't wait past a real test's timeout.
    ///
    /// Returns the path to the mock script (marked executable).
    fn write_mock_proxy(dir: &Path) -> PathBuf {
        let script = dir.join("mock-xdg-dbus-proxy");
        // Use absolute paths for coreutils — bwrap-adjacent test envs
        // sometimes have a minimal PATH inherited that lacks
        // /usr/bin. `read` on a fifo blocks without needing sleep.
        // Use `/usr/bin/sleep` via absolute path — this test's parent
        // (the launcher lib test binary) may inherit a minimal PATH
        // that doesn't include /usr/bin. `sleep infinity` blocks
        // until SIGTERM interrupts it (the trap then fires).
        std::fs::write(
            &script,
            r#"#!/bin/sh
# args: SOURCE DEST [filter args...]
: >"$2"
trap 'rm -f "$2"; exit 0' TERM
if [ -x /usr/bin/sleep ]; then
    /usr/bin/sleep infinity &
elif [ -x /bin/sleep ]; then
    /bin/sleep infinity &
else
    exit 1
fi
wait $!
"#,
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
        script
    }

    #[test]
    fn spawn_and_drop_reaps_the_child() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = write_mock_proxy(tmp.path());
        let socket = tmp.path().join("dbus-proxy.sock");
        let mut guard =
            spawn_dbus_proxy(&mock, "unix:path=/dev/null/nope", &socket).expect("spawn");
        let pid = guard.pid().expect("proxy pid");
        assert!(socket.exists(), "socket marker should be present");
        // /proc/<pid> is present as long as the process is running.
        assert!(
            PathBuf::from(format!("/proc/{pid}")).exists(),
            "proxy pid {pid} should be running"
        );
        guard.terminate();
        // After terminate the socket should be gone and the pid
        // should have been reaped (no zombie).
        assert!(!socket.exists(), "socket should be removed on terminate");
    }

    #[test]
    fn drop_terminates_when_terminate_not_called() {
        let tmp = tempfile::tempdir().unwrap();
        let mock = write_mock_proxy(tmp.path());
        let socket = tmp.path().join("dbus-proxy.sock");
        let pid = {
            let guard =
                spawn_dbus_proxy(&mock, "unix:path=/dev/null/nope", &socket).expect("spawn");
            guard.pid().expect("proxy pid")
        };
        // Give Drop's SIGTERM + wait a moment to propagate.
        std::thread::sleep(Duration::from_millis(200));
        assert!(!socket.exists(), "socket should be removed on drop");
        // /proc/<pid> disappears once the kernel reaps the exited
        // process. Poll briefly to avoid a schedule race.
        let deadline = Instant::now() + Duration::from_secs(2);
        while PathBuf::from(format!("/proc/{pid}")).exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !PathBuf::from(format!("/proc/{pid}")).exists(),
            "proxy pid {pid} should have been reaped after Drop"
        );
    }
}
