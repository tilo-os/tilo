//! Filtered D-Bus proxy for sandboxed apps.
//!
//! Motivation. When a manifest declares a `via-host` XDG portal, the
//! sandboxed app needs a well-defined slice of the host session bus —
//! the portal service and usually fd.o notifications plus MPRIS.
//! Bind-mounting the raw session-bus socket exposes the FULL bus
//! though — every daemon the user's session registered — reachable
//! from inside the sandbox. Flatpak's `xdg-dbus-proxy --filter` solves
//! this: a small forwarder that only relays messages matching an
//! allowlist, running as a per-sandbox helper.
//!
//! What we do here: locate `xdg-dbus-proxy` (from `flatpak-xdg-utils`,
//! falling back to the raw-bind path if missing); spawn one proxy per
//! sandbox allowing `org.freedesktop.DBus` (needed for the `Hello`
//! handshake), the `org.freedesktop.portal.*` tree,
//! `org.freedesktop.Notifications`, and `org.mpris.MediaPlayer2.*`;
//! listen on a per-launcher socket under
//! `/run/user/<uid>/tilo/dbus-proxy/<pid>.sock`; and return an RAII
//! guard that `SIGTERM`s the proxy and unlinks the socket on drop.
//!
//! `xdg-dbus-proxy` takes the upstream bus address as its first
//! positional arg and understands every transport libdbus does,
//! including `unix:abstract=...` — strictly more capable than the
//! raw-bind path, which can only mount `unix:path=...` addresses.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

/// Filter args passed after `SOURCE DEST` to `xdg-dbus-proxy`.
///
/// Explicit allowlist. The only wildcard is `org.freedesktop.portal.*`,
/// since portals are a growing family of well-known names under a
/// stable prefix; everything else is fully-qualified: `--filter` turns
/// on filtering, `--see=org.freedesktop.DBus` is the bus daemon itself
/// (needed for `Hello`), `--talk=org.freedesktop.portal.*` covers every
/// XDG portal, and `--talk=org.freedesktop.Notifications` /
/// `--talk=org.mpris.MediaPlayer2.*` cover notifications and media
/// control.
pub const DEFAULT_FILTER_ARGS: &[&str] = &[
    "--filter",
    "--see=org.freedesktop.DBus",
    "--talk=org.freedesktop.portal.*",
    "--talk=org.freedesktop.Notifications",
    "--talk=org.mpris.MediaPlayer2.*",
];

/// Env-var override for the `xdg-dbus-proxy` binary path — used by the
/// test suite (mock fixture) and installs that ship the binary
/// somewhere non-standard.
///
/// Honoured only by developer builds (see [`crate::dev_env`]): the proxy is
/// what stands between an app and the unfiltered session bus.
pub const PROXY_BIN_ENV: &str = crate::dev_env::DBUS_PROXY_BIN_ENV;

/// Locate the `xdg-dbus-proxy` binary, `None` if nothing matches. Production
/// then leaves D-Bus unavailable; it never falls back to binding the raw
/// session bus into an untrusted sandbox.
///
/// Lookup order: `$TILO_LAUNCHER_DBUS_PROXY_BIN` (test/override, developer
/// builds only),
/// `xdg-dbus-proxy` on `$PATH`,
/// `/usr/libexec/flatpak-xdg-utils/xdg-dbus-proxy` (Fedora), then
/// `/usr/lib/flatpak-xdg-utils/xdg-dbus-proxy` (some Debian derivatives).
pub fn find_proxy_binary() -> Option<PathBuf> {
    if let Some(bin) = crate::dev_env::dev_override(PROXY_BIN_ENV) {
        let p = PathBuf::from(bin);
        return p.is_file().then_some(p);
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

/// Host-side path where the per-launcher proxy listens:
/// `/run/user/<uid>/tilo/dbus-proxy/<pid>.sock`. The `<pid>`
/// disambiguates concurrent launches.
pub fn proxy_socket_path(uid: u32, launcher_pid: u32) -> PathBuf {
    PathBuf::from(format!(
        "/run/user/{uid}/tilo/dbus-proxy/{launcher_pid}.sock"
    ))
}

/// In-sandbox bus path, matching Flatpak's convention: the app looks
/// for `$DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/<uid>/bus`.
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
/// `bus_addr` is the upstream address (`unix:path=...` or
/// `unix:abstract=...`, the proxy resolves both); `socket_path` is
/// where the proxy should listen.
///
/// Returns a [`DBusProxyGuard`] whose `Drop` `SIGTERM`s the child and
/// unlinks the socket. Callers should keep it alive until the sandbox
/// exits.
///
/// # Errors
///
/// Returns [`ProxyError::Mkdir`] if the socket's parent directory can't be
/// created, [`ProxyError::Spawn`] if `binary` can't be spawned, or
/// [`ProxyError::NotReady`] if its listener socket doesn't appear within
/// [`PROXY_READY_TIMEOUT`].
pub fn spawn_dbus_proxy(
    binary: &Path,
    bus_addr: &str,
    socket_path: &Path,
) -> Result<DBusProxyGuard, ProxyError> {
    spawn_dbus_proxy_within(binary, bus_addr, socket_path, PROXY_READY_TIMEOUT)
}

/// How long a launch waits for the proxy's listener socket. The real proxy
/// is ready in ~10ms; a launch does not stall on one that never comes up.
pub const PROXY_READY_TIMEOUT: Duration = Duration::from_secs(2);

/// [`spawn_dbus_proxy`], waiting up to `ready_within` for the listener
/// socket.
///
/// # Errors
///
/// As [`spawn_dbus_proxy`], with [`ProxyError::NotReady`] after
/// `ready_within`.
pub fn spawn_dbus_proxy_within(
    binary: &Path,
    bus_addr: &str,
    socket_path: &Path,
    ready_within: Duration,
) -> Result<DBusProxyGuard, ProxyError> {
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| ProxyError::Mkdir {
            path: parent.to_path_buf(),
            source,
        })?;
    }
    // A stale socket from a previous run would make `bind()` fail with
    // EADDRINUSE. Remove best-effort.
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

    // The socket appears asynchronously; poll with a bounded timeout.
    match wait_for_socket(socket_path, ready_within) {
        Ok(()) => Ok(guard),
        Err(_) => Err(ProxyError::NotReady {
            path: socket_path.to_path_buf(),
            timeout: ready_within,
        }),
    }
}

fn wait_for_socket(path: &Path, timeout: Duration) -> io::Result<()> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .unwrap_or_else(Instant::now);
    while Instant::now() < deadline {
        // `try_exists` catches both regular files (mock proxy) and socket
        // inodes (real proxy); `Path::exists` would too but hides `EACCES`.
        if path.try_exists().unwrap_or(false) {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!(
            "socket {} did not appear within {timeout:?}",
            path.display()
        ),
    ))
}

/// RAII wrapper around a spawned `xdg-dbus-proxy`. `Drop` `SIGTERM`s
/// the child, waits briefly, and unlinks the socket. Idempotent —
/// callers can call [`Self::terminate`] explicitly before drop.
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

    /// PID of the proxy child, or `None` if already reaped — used by tests
    /// to assert the process is gone after Drop.
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(std::process::Child::id)
    }

    /// SIGTERM the proxy, wait up to 500ms, then SIGKILL if it hasn't
    /// exited. Unlinks the socket unconditionally; safe to call more than
    /// once.
    pub fn terminate(&mut self) {
        if let Some(mut child) = self.child.take() {
            // SAFETY: `kill(2)` with SIGTERM on a live pid we own
            // is well-defined. Sending to a reaped pid returns
            // ESRCH which we ignore.
            unsafe {
                libc::kill(i32::try_from(child.id()).unwrap_or(i32::MAX), libc::SIGTERM);
            }
            let deadline = Instant::now()
                .checked_add(Duration::from_millis(500))
                .unwrap_or_else(Instant::now);
            loop {
                match child.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) => {
                        if Instant::now() >= deadline {
                            let _ = child.kill();
                            let _ = child.wait();
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                    }
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
        assert_eq!(p, PathBuf::from("/run/user/1000/tilo/dbus-proxy/4242.sock"));
    }

    #[test]
    fn sandbox_bus_env_matches_sandbox_bus_path() {
        assert_eq!(sandbox_bus_env(1000), "unix:path=/run/user/1000/bus");
        assert_eq!(sandbox_bus_path(1000), PathBuf::from("/run/user/1000/bus"));
    }

    // The proxy binary is located through `PROXY_BIN_ENV` and `PATH`, which
    // is the interface these two pin.

    #[test]
    fn find_proxy_binary_honours_env_override() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("xdg-dbus-proxy-mock");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        // Env var wins.
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(PROXY_BIN_ENV, &bin);
        let found = find_proxy_binary().expect("env override");
        assert_eq!(found, bin);
    }

    #[test]
    fn find_proxy_binary_returns_none_when_env_points_at_nonexistent() {
        let mut env = tilo_test_env::ScopedEnv::new();
        // Empty PATH so the fallback can't find a real binary.
        env.set(PROXY_BIN_ENV, "/nonexistent/xdg-dbus-proxy")
            .set("PATH", "");
        let found = find_proxy_binary();
        // Put `PATH` back before asserting.
        drop(env);
        assert!(
            found.is_none(),
            "expected None with bad env + empty PATH, got {found:?}"
        );
    }

    /// Mock proxy fixture: writes a marker to `argv[2]` (the socket path)
    /// so callers waiting on socket existence see it, then becomes a
    /// `sleep` that the guard's SIGTERM ends. Removing the marker is left
    /// to the guard, which is what production relies on. Returns the path
    /// to the mock script (marked executable).
    fn write_mock_proxy(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let script = dir.join("mock-xdg-dbus-proxy");
        // Absolute paths for coreutils — bwrap-adjacent test envs sometimes
        // have a minimal PATH lacking /usr/bin.
        std::fs::write(
            &script,
            r#"#!/bin/sh
# args: SOURCE DEST [filter args...]
: >"$2"
# exec, not a backgrounded child: the guard signals and reaps this pid,
# and a child of a shell it killed would be orphaned, never reaped.
if [ -x /usr/bin/sleep ]; then
    exec /usr/bin/sleep infinity
fi
if [ -x /bin/sleep ]; then
    exec /bin/sleep infinity
fi
exit 1
"#,
        )
        .unwrap();
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
        // After terminate the socket should be gone and the pid should have
        // been reaped (no zombie).
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
        // /proc/<pid> disappears once the kernel reaps the exited process; poll
        // briefly to avoid a schedule race.
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
