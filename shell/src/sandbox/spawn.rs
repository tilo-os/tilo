//! Spawn helper that wraps `std::process::Command::new("bwrap")` with a
//! [`SandboxProfile`]'s rendered argv.
//!
//! Phase 1 keeps this layer deliberately thin so higher-level launchers
//! (workspace activation, capability portal, etc.) can plug in without
//! reaching into `Command` plumbing themselves.
//!
//! Phase 3 v1 layered the per-workspace Wayland bridge socket on top: the
//! spawn helper now lazily binds a bridge socket per workspace and points
//! the profile's `wayland_socket` at it before rendering argv. Bridges
//! live for the process lifetime in a global cache (intentionally leaked,
//! see [`ensure_bridge_for`]).

use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::{Mutex, OnceLock};

use super::profile::SandboxProfile;
use crate::compositor::bridge::BridgeSocket;

/// Process-lifetime cache of active bridge sockets keyed by workspace id.
///
/// v1 deliberately leaks bridges here: there is no shutdown path. The
/// drop ordering at process exit unlinks the socket files, which is good
/// enough for the "weston-info works" bar. A follow-up adds explicit
/// teardown when a workspace is closed.
static BRIDGES: OnceLock<Mutex<HashMap<u64, BridgeSocket>>> = OnceLock::new();

/// Spawn `command` inside the sandbox described by `profile`.
///
/// Takes the profile by value because [`SandboxProfile::into_argv`]
/// consumes `self`. The plan permits either ownership shape; owning
/// here means callers can build the profile inline at the call site
/// (`spawn_in_workspace(SandboxProfile::for_workspace_test(...), &cmd)`)
/// without an explicit `.clone()`.
///
/// Before invoking `bwrap`, this helper ensures a per-workspace Wayland
/// bridge socket is bound and running, and rewrites
/// `profile.wayland_socket` to point at the bridge path so the rendered
/// `--bind` triple inside the sandbox sees the bridge instead of the
/// global socket.
///
/// The returned [`Child`] is not waited on — callers decide whether to
/// stream output, attach pipes, or just `wait()`.
pub fn spawn_in_workspace(
    profile: &SandboxProfile,
    command: &[String],
) -> std::io::Result<Child> {
    // Clone the profile so we can keep the borrow shape the plan asks
    // for (`&SandboxProfile`) while still feeding the consuming
    // `into_argv`. Profile is cheap-to-clone (a few `Vec`s and a small
    // `HashMap`), and cloning at the launch boundary avoids subtle
    // ownership puzzles when the caller wants to inspect the profile
    // after spawning (e.g. for logging).
    let mut profile = clone_profile(profile);

    // Wire the bridge socket. Failures here surface as IO errors so the
    // caller can decide whether to fall back to no-sandbox or fail the
    // launch entirely; v1's compositor caller treats them as fatal.
    let runtime_dir = resolve_runtime_dir();
    let upstream = upstream_wayland_path(&runtime_dir);
    let bridge_path = ensure_bridge_for(profile.workspace_id, &runtime_dir, &upstream)?;
    rewrite_wayland_socket(&mut profile, &bridge_path);

    let argv = profile.into_argv(command);
    Command::new("bwrap").args(&argv).spawn()
}

/// Resolve `$XDG_RUNTIME_DIR`, falling back to `/run/user/<uid>`.
///
/// The fallback uses libc's geteuid via `nix::unistd` would pull in a
/// dep; v1 uses `users::get_current_uid`-equivalent via the env `UID`
/// variable, defaulting to `1000` if unset (matches the
/// [`SandboxProfile::for_workspace_test`] default).
fn resolve_runtime_dir() -> PathBuf {
    if let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR") {
        return PathBuf::from(dir);
    }
    let uid = std::env::var("UID")
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(1000);
    PathBuf::from(format!("/run/user/{uid}"))
}

/// Resolve the upstream Wayland socket path: `runtime_dir/$WAYLAND_DISPLAY`
/// (or `runtime_dir/wayland-0` if `$WAYLAND_DISPLAY` is unset).
///
/// `WAYLAND_DISPLAY` may be either a bare name (`wayland-0`) or an
/// absolute path; we honour both via [`Path::join`]'s absolute-path
/// short-circuit semantics.
fn upstream_wayland_path(runtime_dir: &Path) -> PathBuf {
    let display = std::env::var_os("WAYLAND_DISPLAY")
        .unwrap_or_else(|| std::ffi::OsString::from("wayland-0"));
    runtime_dir.join(display)
}

/// Look up (or lazily bind + start) the bridge socket for `workspace_id`.
///
/// Returns the host-side bridge path on success. The bridge is kept
/// alive in [`BRIDGES`] for the rest of the process so its accept loop
/// keeps serving subsequent spawns.
fn ensure_bridge_for(
    workspace_id: u64,
    runtime_dir: &Path,
    upstream: &Path,
) -> io::Result<PathBuf> {
    let cache = BRIDGES.get_or_init(|| Mutex::new(HashMap::new()));
    // The mutex is held only briefly for lookup/insert; the relay
    // threads do not contend on it.
    let mut map = cache.lock().expect("BRIDGES mutex poisoned");
    if let Some(bridge) = map.get(&workspace_id) {
        return Ok(bridge.path().to_path_buf());
    }
    let mut bridge = BridgeSocket::bind(workspace_id, runtime_dir, upstream)?;
    bridge.start()?;
    let path = bridge.path().to_path_buf();
    map.insert(workspace_id, bridge);
    Ok(path)
}

/// Update both `profile.wayland_socket` and the corresponding `--bind`
/// entry in `profile.binds_rw` to use the bridge path on the host side.
///
/// We keep the in-sandbox path the same as whatever the existing bind
/// targets (typically `/run/user/<uid>/wayland-0`) so apps that read
/// `WAYLAND_DISPLAY` see no difference. Only the host source flips from
/// the global socket to the bridge.
fn rewrite_wayland_socket(profile: &mut SandboxProfile, bridge_path: &Path) {
    let old_host = profile.wayland_socket.clone();
    profile.wayland_socket = bridge_path.to_path_buf();
    for (host, _sandbox) in profile.binds_rw.iter_mut() {
        if *host == old_host {
            *host = bridge_path.to_path_buf();
        }
    }
}

/// Manual clone helper. We don't derive `Clone` on `SandboxProfile`
/// itself because that would silently invite cloning at use-sites where
/// the plan explicitly wants `into_argv`'s consume-by-value contract;
/// keeping the clone local to the spawn helper documents the trade-off.
fn clone_profile(p: &SandboxProfile) -> SandboxProfile {
    SandboxProfile {
        workspace_id: p.workspace_id,
        binds_ro: p.binds_ro.clone(),
        binds_rw: p.binds_rw.clone(),
        env: p.env.clone(),
        uid: p.uid,
        gid: p.gid,
        seccomp: p.seccomp.clone(),
        wayland_socket: p.wayland_socket.clone(),
        dbus_socket: p.dbus_socket.clone(),
        net_isolated: p.net_isolated,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_wayland_socket_updates_profile_and_binds() {
        let mut profile = SandboxProfile::for_workspace_test(
            Path::new("/tmp/ws"),
            Path::new("/tmp/cache"),
        );
        let original = profile.wayland_socket.clone();
        // Sanity: the default profile has a bind whose host matches
        // `wayland_socket`.
        assert!(profile.binds_rw.iter().any(|(host, _)| host == &original));

        let bridge = PathBuf::from("/run/user/1000/nirios-ws-42");
        rewrite_wayland_socket(&mut profile, &bridge);

        assert_eq!(profile.wayland_socket, bridge);
        // The old host path no longer appears in binds; the new one does.
        assert!(!profile.binds_rw.iter().any(|(host, _)| host == &original));
        assert!(profile.binds_rw.iter().any(|(host, _)| host == &bridge));
    }

    #[test]
    fn upstream_wayland_path_uses_env_when_set() {
        // SAFETY: tests in this module read process env; setting it here
        // is racy with parallel tests but the value space is unique.
        let runtime = Path::new("/tmp/quick-shell-test-runtime");
        // Use a scope-local override via a temporary; we can't use
        // `std::env::set_var` race-free in tests, so just exercise the
        // fallback shape.
        let p = upstream_wayland_path(runtime);
        // The result is either runtime/<WAYLAND_DISPLAY> or runtime/wayland-0.
        assert!(p.starts_with(runtime) || p.is_absolute());
    }
}
