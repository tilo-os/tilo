//! Filtered D-Bus proxies for sandboxed apps.
//!
//! Two buses reach a graphical app, each only through its own
//! `xdg-dbus-proxy`, and each filtered to an explicit allowlist:
//!
//! - **Session bus.** Portal-adjacent names when the manifest declares a
//!   `via-host` portal, and for every graphical app exactly the two
//!   read-only questions an accessibility client must ask
//!   `org.a11y.Bus`: where the AT-SPI bus is (`GetAddress`) and whether
//!   assistive technology is on (the `org.a11y.Status` properties).
//! - **AT-SPI bus.** Flatpak's filter, verbatim: the app may embed its
//!   accessibility root in the registry, read which events listeners want,
//!   and notify device-event listeners. Assistive technology then calls
//!   *into* the app; the app cannot call any other client on that bus,
//!   so it cannot read another app's tree or synthesise its events.
//!
//! Without the second proxy a sandboxed app is invisible to every screen
//! reader; without the first it never learns accessibility is enabled.
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
//! leaving the bus unavailable if missing); spawn one proxy per
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

use std::io::{self, Read};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;

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

/// The session-bus rules every graphical app gets, and the only ones a
/// portal-less app gets: `org.a11y.Bus.GetAddress`, reading the
/// `org.a11y.Status` properties (`IsEnabled`, `ScreenReaderEnabled`) and
/// their change signal, all on `/org/a11y/bus`. Not `--talk`: setting those
/// properties, or any other method of the accessibility bus launcher, stays
/// denied.
pub const A11Y_STATUS_FILTER_ARGS: &[&str] = &[
    "--call=org.a11y.Bus=org.a11y.Bus.GetAddress@/org/a11y/bus",
    "--call=org.a11y.Bus=org.freedesktop.DBus.Properties.Get@/org/a11y/bus",
    "--call=org.a11y.Bus=org.freedesktop.DBus.Properties.GetAll@/org/a11y/bus",
    "--broadcast=org.a11y.Bus=org.freedesktop.DBus.Properties.PropertiesChanged@/org/a11y/bus",
];

/// The session-bus filter for one launch: the bus daemon for `Hello`, the
/// portal names when the manifest declares a `via-host` portal, and the
/// accessibility status rules when the app has a display.
#[must_use]
pub fn session_filter_args(portals: bool, graphical: bool) -> Vec<&'static str> {
    let mut args = vec!["--filter", "--see=org.freedesktop.DBus"];
    if portals {
        args.extend(DEFAULT_FILTER_ARGS.iter().skip(2));
    }
    if graphical {
        args.extend(A11Y_STATUS_FILTER_ARGS);
    }
    args
}

/// The AT-SPI bus filter: Flatpak's (`flatpak_run_add_a11y_dbus_args`),
/// verbatim. Every rule names the registry and an exact object path.
pub const ATSPI_FILTER_ARGS: &[&str] = &[
    "--filter",
    "--sloppy-names",
    "--broadcast=org.a11y.atspi.Registry=org.a11y.atspi.Registry.EventListenerRegistered@/org/a11y/atspi/registry",
    "--broadcast=org.a11y.atspi.Registry=org.a11y.atspi.Registry.EventListenerDeregistered@/org/a11y/atspi/registry",
    "--call=org.a11y.atspi.Registry=org.a11y.atspi.Socket.Embed@/org/a11y/atspi/accessible/root",
    "--call=org.a11y.atspi.Registry=org.a11y.atspi.Socket.Unembed@/org/a11y/atspi/accessible/root",
    "--call=org.a11y.atspi.Registry=org.a11y.atspi.Registry.GetRegisteredEvents@/org/a11y/atspi/registry",
    "--call=org.a11y.atspi.Registry=org.a11y.atspi.DeviceEventController.GetKeystrokeListeners@/org/a11y/atspi/registry/deviceeventcontroller",
    "--call=org.a11y.atspi.Registry=org.a11y.atspi.DeviceEventController.GetDeviceEventListeners@/org/a11y/atspi/registry/deviceeventcontroller",
    "--call=org.a11y.atspi.Registry=org.a11y.atspi.DeviceEventController.NotifyListenersSync@/org/a11y/atspi/registry/deviceeventcontroller",
    "--call=org.a11y.atspi.Registry=org.a11y.atspi.DeviceEventController.NotifyListenersAsync@/org/a11y/atspi/registry/deviceeventcontroller",
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
/// builds only; when set it is authoritative, and an invalid or empty path
/// leaves D-Bus unavailable rather than falling back),
/// `xdg-dbus-proxy` on `$PATH`,
/// `/usr/libexec/flatpak-xdg-utils/xdg-dbus-proxy` (Fedora), then
/// `/usr/lib/flatpak-xdg-utils/xdg-dbus-proxy` (some Debian derivatives).
pub fn find_proxy_binary() -> Option<PathBuf> {
    if let Some(bin) = package_store::dev_env::dev_override(PROXY_BIN_ENV) {
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

/// The session's runtime directory, where a launcher's host-side sockets
/// live: the one the session that launches it runs in (`XDG_RUNTIME_DIR`),
/// so a nested session's proxies stay in its own directory and never in the
/// host desktop's, whose launchers number their sockets by pid too.
#[must_use]
pub fn session_runtime_dir() -> PathBuf {
    tilo_paths::xdg::runtime_dir_or_default()
}

/// Host-side path where the per-launcher proxy listens:
/// `<runtime>/tilo/dbus-proxy/<pid>.sock`, in the session's runtime
/// directory ([`session_runtime_dir`]). The `<pid>` disambiguates
/// concurrent launches.
pub fn proxy_socket_path(runtime: &Path, launcher_pid: u32) -> PathBuf {
    runtime.join(format!("tilo/dbus-proxy/{launcher_pid}.sock"))
}

/// Host-side path of the per-launcher system-bus proxy socket, for the
/// packages [`crate::system_authority`] grants it.
pub fn system_proxy_socket_path(runtime: &Path, launcher_pid: u32) -> PathBuf {
    runtime.join(format!("tilo/dbus-proxy/{launcher_pid}-system.sock"))
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

/// Host-side path of the per-launcher AT-SPI proxy socket.
pub fn atspi_proxy_socket_path(runtime: &Path, launcher_pid: u32) -> PathBuf {
    runtime.join(format!("tilo/dbus-proxy/{launcher_pid}-atspi.sock"))
}

/// In-sandbox AT-SPI bus path, where at-spi2's own launcher puts it.
pub fn sandbox_atspi_path(uid: u32) -> PathBuf {
    PathBuf::from(format!("/run/user/{uid}/at-spi/bus"))
}

/// In-sandbox `AT_SPI_BUS_ADDRESS`.
pub fn sandbox_atspi_env(uid: u32) -> String {
    format!("unix:path=/run/user/{uid}/at-spi/bus")
}

/// The host AT-SPI bus address: `AT_SPI_BUS_ADDRESS` when the session set it,
/// else `org.a11y.Bus.GetAddress` on the session bus. `None` when the session
/// runs no accessibility bus.
pub fn host_atspi_address(session_bus: &str) -> Option<String> {
    if let Ok(address) = std::env::var("AT_SPI_BUS_ADDRESS")
        && !address.is_empty()
    {
        return Some(address);
    }
    let address: zbus::Address = session_bus.try_into().ok()?;
    let connection = zbus::blocking::connection::Builder::address(address)
        .ok()?
        .build()
        .ok()?;
    let reply = connection
        .call_method(
            Some("org.a11y.Bus"),
            "/org/a11y/bus",
            Some("org.a11y.Bus"),
            "GetAddress",
            &(),
        )
        .ok()?;
    let address: String = reply.body().deserialize().ok()?;
    (!address.is_empty()).then_some(address)
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
        "xdg-dbus-proxy did not report its listener ready at {path} within {timeout:?}. \
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
/// [`ProxyError::NotReady`] if its listener is not reported ready within
/// [`PROXY_READY_TIMEOUT`].
pub fn spawn_dbus_proxy(
    binary: &Path,
    bus_addr: &str,
    socket_path: &Path,
) -> Result<DBusProxyGuard, ProxyError> {
    spawn_dbus_proxy_within(binary, bus_addr, socket_path, PROXY_READY_TIMEOUT)
}

/// [`spawn_dbus_proxy`] with an explicit filter (one of this module's
/// allowlists, never a caller-built one).
///
/// # Errors
///
/// As [`spawn_dbus_proxy`].
pub fn spawn_filtered_proxy(
    binary: &Path,
    bus_addr: &str,
    socket_path: &Path,
    filter: &[&str],
) -> Result<DBusProxyGuard, ProxyError> {
    spawn_proxy(binary, bus_addr, socket_path, filter, PROXY_READY_TIMEOUT)
}

/// Maximum wait for the proxy's listener readiness notification. A launch
/// must not stall indefinitely on a helper that never comes up.
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
    spawn_proxy(
        binary,
        bus_addr,
        socket_path,
        DEFAULT_FILTER_ARGS,
        ready_within,
    )
}

fn spawn_proxy(
    binary: &Path,
    bus_addr: &str,
    socket_path: &Path,
    filter: &[&str],
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

    let (ready_channel, child_channel) = UnixStream::pair().map_err(ProxyError::Spawn)?;
    let mut cmd = Command::new(binary);
    // Upstream writes `x` after all listeners start and watches this channel
    // for HUP. Keep our endpoint alive for the entire proxy lifetime. Passing
    // the duplex endpoint as stdin preserves stdout/stderr for diagnostics.
    cmd.arg("--fd=0").arg(bus_addr).arg(socket_path);
    cmd.stdin(Stdio::from(OwnedFd::from(child_channel)));
    for arg in filter {
        cmd.arg(arg);
    }
    let child = cmd.spawn().map_err(ProxyError::Spawn)?;
    let mut guard = DBusProxyGuard {
        child: Some(child),
        socket_path: socket_path.to_path_buf(),
        ready_channel,
    };

    match wait_for_ready(&mut guard.ready_channel, socket_path, ready_within) {
        Ok(()) => Ok(guard),
        Err(_) => Err(ProxyError::NotReady {
            path: socket_path.to_path_buf(),
            timeout: ready_within,
        }),
    }
}

fn wait_for_ready(channel: &mut UnixStream, path: &Path, timeout: Duration) -> io::Result<()> {
    channel.set_read_timeout(Some(timeout))?;
    let mut token = [0];
    channel.read_exact(&mut token)?;
    if token == *b"x" && path.try_exists()? {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "proxy readiness notification or listener is missing",
    ))
}

/// RAII wrapper around a spawned `xdg-dbus-proxy`. `Drop` `SIGTERM`s
/// the child, waits briefly, and unlinks the socket. Idempotent —
/// callers can call [`Self::terminate`] explicitly before drop.
pub struct DBusProxyGuard {
    child: Option<Child>,
    socket_path: PathBuf,
    ready_channel: UnixStream,
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
            let _ = bounded_command::wait_or_kill(&mut child, Duration::from_millis(500));
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
    fn filters_grant_exactly_the_listed_rules() {
        // A portal-less graphical app: the bus daemon and the accessibility
        // status questions, nothing else.
        assert_eq!(
            session_filter_args(false, true),
            [
                "--filter",
                "--see=org.freedesktop.DBus",
                "--call=org.a11y.Bus=org.a11y.Bus.GetAddress@/org/a11y/bus",
                "--call=org.a11y.Bus=org.freedesktop.DBus.Properties.Get@/org/a11y/bus",
                "--call=org.a11y.Bus=org.freedesktop.DBus.Properties.GetAll@/org/a11y/bus",
                "--broadcast=org.a11y.Bus=org.freedesktop.DBus.Properties.PropertiesChanged@/org/a11y/bus",
            ]
        );
        // A portal app adds exactly the portal names.
        assert_eq!(
            session_filter_args(true, true),
            [
                "--filter",
                "--see=org.freedesktop.DBus",
                "--talk=org.freedesktop.portal.*",
                "--talk=org.freedesktop.Notifications",
                "--talk=org.mpris.MediaPlayer2.*",
                "--call=org.a11y.Bus=org.a11y.Bus.GetAddress@/org/a11y/bus",
                "--call=org.a11y.Bus=org.freedesktop.DBus.Properties.Get@/org/a11y/bus",
                "--call=org.a11y.Bus=org.freedesktop.DBus.Properties.GetAll@/org/a11y/bus",
                "--broadcast=org.a11y.Bus=org.freedesktop.DBus.Properties.PropertiesChanged@/org/a11y/bus",
            ]
        );
        assert_eq!(session_filter_args(true, false), DEFAULT_FILTER_ARGS);
        // No rule anywhere grants a whole name on the accessibility side, and
        // nothing ever widens to ownership or to the raw bus.
        for arg in session_filter_args(true, true)
            .iter()
            .chain(ATSPI_FILTER_ARGS)
        {
            assert!(!arg.starts_with("--own"), "{arg}");
            assert!(
                !arg.contains("org.a11y") || !arg.starts_with("--talk"),
                "{arg}"
            );
        }
        // The AT-SPI filter is Flatpak's: every rule is a call or broadcast to
        // the registry at an exact object path.
        assert_eq!(&ATSPI_FILTER_ARGS[..2], ["--filter", "--sloppy-names"]);
        assert_eq!(ATSPI_FILTER_ARGS.len(), 11);
        for rule in &ATSPI_FILTER_ARGS[2..] {
            assert!(
                rule.starts_with("--call=org.a11y.atspi.Registry=org.a11y.atspi.")
                    || rule.starts_with("--broadcast=org.a11y.atspi.Registry=org.a11y.atspi."),
                "{rule}"
            );
            let path = rule
                .rsplit_once('@')
                .expect("every rule names an object path")
                .1;
            assert!(
                path.starts_with("/org/a11y/atspi/") && !path.contains('*'),
                "{rule}"
            );
            assert!(!rule.contains(".*@"), "no wildcard method: {rule}");
        }
        assert_eq!(
            atspi_proxy_socket_path(Path::new("/run/user/1000"), 7),
            PathBuf::from("/run/user/1000/tilo/dbus-proxy/7-atspi.sock")
        );
        assert_eq!(
            sandbox_atspi_path(1000),
            PathBuf::from("/run/user/1000/at-spi/bus")
        );
        assert_eq!(
            sandbox_atspi_env(1000),
            "unix:path=/run/user/1000/at-spi/bus"
        );
    }

    #[test]
    fn proxy_paths_env_and_binary_lookup() {
        // Phase `proxy_socket_path_uses_uid_and_pid`.
        {
            let p = proxy_socket_path(Path::new("/run/user/1000"), 4242);
            assert_eq!(p, PathBuf::from("/run/user/1000/tilo/dbus-proxy/4242.sock"));
        }
        // Phase `a_nested_session_s_proxies_stay_in_its_runtime_directory`.
        {
            let nested = tempfile::tempdir().unwrap();
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set("XDG_RUNTIME_DIR", nested.path());
            let runtime = session_runtime_dir();
            assert_eq!(runtime, nested.path());
            for socket in [
                proxy_socket_path(&runtime, 4242),
                atspi_proxy_socket_path(&runtime, 4242),
                system_proxy_socket_path(&runtime, 4242),
            ] {
                assert!(socket.starts_with(nested.path()), "{socket:?}");
            }
        }
        // Phase `sandbox_bus_env_matches_sandbox_bus_path`.
        {
            assert_eq!(sandbox_bus_env(1000), "unix:path=/run/user/1000/bus");
            assert_eq!(sandbox_bus_path(1000), PathBuf::from("/run/user/1000/bus"));
        }
        // Phase `find_proxy_binary_honours_env_override`.
        {
            let tmp = tempfile::tempdir().unwrap();
            let bin = tmp.path().join("xdg-dbus-proxy-mock");
            std::fs::write(&bin, "#!/bin/sh\n").unwrap();
            // Env var wins.
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(PROXY_BIN_ENV, &bin);
            let found = find_proxy_binary().expect("env override");
            assert_eq!(found, bin);
        }
        // Phase `find_proxy_binary_returns_none_when_env_points_at_nonexistent`.
        {
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
# args: [--fd=0] SOURCE DEST [filter args...]
notify=0
if [ "$1" = --fd=0 ]; then notify=1; shift; fi
: >"$2"
if [ "$notify" = 1 ]; then printf x >&0; fi
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
    fn proxy_marker_without_readiness_is_refused() {
        use std::os::unix::fs::PermissionsExt;

        for notification in ["", "?"] {
            let tmp = tempfile::tempdir().unwrap();
            let script = tmp.path().join("unready-proxy");
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\nnotify=0\nif [ \"$1\" = --fd=0 ]; then notify=1; shift; fi\n: >\"$2\"\nif [ \"$notify\" = 1 ]; then printf '%s' '{notification}' >&0; fi\nexec /usr/bin/sleep infinity\n"
                ),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
            let socket = tmp.path().join("proxy.sock");
            let result = spawn_dbus_proxy_within(
                &script,
                "unix:path=/dev/null/nope",
                &socket,
                Duration::from_millis(100),
            );
            assert!(
                matches!(&result, Err(ProxyError::NotReady { .. })),
                "an inode with notification {notification:?} must not admit an unready proxy"
            );
            assert!(!socket.exists(), "a refused helper must be cleaned up");
        }
    }

    #[test]
    fn proxy_child_is_reaped_on_drop() {
        // Phase `spawn_and_drop_reaps_the_child`.
        {
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
        // Phase `drop_terminates_when_terminate_not_called`.
        {
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
            let deadline = tilo_test_env::hang_deadline();
            while PathBuf::from(format!("/proc/{pid}")).exists()
                && std::time::Instant::now() < deadline
            {
                std::thread::sleep(Duration::from_millis(20));
            }
            assert!(
                !PathBuf::from(format!("/proc/{pid}")).exists(),
                "proxy pid {pid} should have been reaped after Drop"
            );
        }
    }

    // The proxy binary is located through `PROXY_BIN_ENV` and `PATH`, which
    // is the interface these two pin.

    #[test]
    fn find_proxy_binary_does_not_fall_back_past_an_invalid_override() {
        use std::os::unix::ffi::OsStringExt;

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("xdg-dbus-proxy"), "#!/bin/sh\n").unwrap();
        let invalid_paths = [
            tmp.path().join("missing").into_os_string(),
            std::ffi::OsString::new(),
            tmp.path()
                .join(std::ffi::OsString::from_vec(vec![0xff]))
                .into_os_string(),
        ];
        for invalid in invalid_paths {
            let mut env = tilo_test_env::ScopedEnv::new();
            env.set(PROXY_BIN_ENV, &invalid).set("PATH", tmp.path());
            let found = find_proxy_binary();
            drop(env);
            assert!(
                found.is_none(),
                "an invalid explicit override must not use the available PATH proxy: {found:?}"
            );
        }
    }

    #[test]
    fn find_proxy_binary_uses_path_when_override_is_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let bin = tmp.path().join("xdg-dbus-proxy");
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.remove(PROXY_BIN_ENV).set("PATH", tmp.path());
        let found = find_proxy_binary();
        drop(env);
        assert_eq!(found, Some(bin));
    }
}
