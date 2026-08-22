//! Service-socket discovery + SCM_RIGHTS handoff.
//!
//! Each declared service maps to a Unix socket at
//! `$XDG_RUNTIME_DIR/qos/services/<name>.sock`. The launcher:
//!
//! 1. Looks up the socket per the manifest's service requirements.
//! 2. For `required@<v>` whose socket is absent → exit 70 with a
//!    descriptive error.
//! 3. For `optional@<v>` whose socket is absent — or present but
//!    refusing connections, which is what a session that died without
//!    unlinking leaves behind → log a warning, continue.
//! 4. For sockets that exist → open the socket as a `UnixStream` and
//!    take its raw fd. The descriptor reaches the app the way
//!    descriptors normally do: `run::place_service_fds` dup2s it onto
//!    the agreed `child_fd` and clears CLOEXEC, so it survives bwrap's
//!    exec and the `__sandbox-exec` re-exec at that number. The app
//!    reads the number from `QOS_SVC_<NAME>_FD`.
//!
//! bwrap is deliberately not told about the socket. It has no flag
//! that hands a live socket to the sandboxed process, and the one that
//! looks like it does is a trap: `--file FD DEST` copies the
//! descriptor's *contents* into a regular file, so pointing it at a
//! connected socket makes bwrap read until an EOF that never arrives.
//! The launch then hangs before it ever execs the app, and the only
//! symptom is an app that never appears.
//!
//! ### Open design question — where to mediate the handoff?
//!
//! There are three plausible mediation points:
//!
//! - Inherited descriptors (this module's choice): simplest, no extra
//!   broker process, fd ends up at a known child-side number.
//! - A dedicated broker that listens on a per-cluster socket and
//!   accepts a connection per launch, sending fds via SCM_RIGHTS:
//!   richer (broker can revoke, audit, hot-swap), but adds latency
//!   and a dependency on a running daemon.
//! - The compositor itself, with the launcher receiving fds inherited
//!   from its parent: tightest coupling, hardest to test.
//!
//! Today we pick option 1. If/when revocation/auditing matters, the
//! launcher's API stays the same — only `resolve_service_binds`
//! changes to dial the broker instead of opening the socket directly.

use std::collections::BTreeMap;
use std::os::fd::{IntoRawFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use crate::manifest::{ServiceReq, ServiceReqParseError};

/// Exit code for "required service missing". Mirrors `EX_SOFTWARE` (70)
/// from sysexits.h — a service the package declared isn't available.
pub const EXIT_REQUIRED_SERVICE_MISSING: i32 = 70;

/// Manifest name of the CRDT fan-out service (`[sandbox.services] collab`).
///
/// This constant is the single point that decides where the collab
/// socket lives, and it has two owners on opposite sides of a
/// namespace boundary: the compositor *binds* [`socket_for`] of this
/// name, and the launcher *connects* it to hand the app a descriptor.
/// They were once two independently-written path expressions, so the
/// compositor bound a socket nobody dialled and every app quietly ran
/// with collaboration switched off. Route both sides through here.
///
/// `shell::shell::collab_broker` binds it; see
/// `collab_service_socket_is_the_path_the_compositor_binds` in the
/// compositor for the test that fails if the two drift apart again.
pub const COLLAB_SERVICE: &str = "collab";

/// One resolved service handoff: the open fd + the in-sandbox child fd
/// number it should take.
#[derive(Debug)]
pub struct ResolvedService {
    pub name: String,
    pub host_fd: RawFd,
    pub child_fd: i32,
}

/// Errors during service resolution.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(
        "required service `{name}` (protocol qos.svc.{name}@{version}) was not found at `{path}`"
    )]
    RequiredMissing {
        name: String,
        version: u32,
        path: PathBuf,
    },
    #[error("malformed service requirement `{0}`: {1}")]
    BadRequirement(String, ServiceReqParseError),
    #[error("failed to open service socket `{path}`: {source}")]
    OpenFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

/// Discover `$XDG_RUNTIME_DIR/qos/services/`.
pub fn services_root() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let uid = unsafe { libc::geteuid() };
            PathBuf::from(format!("/run/user/{uid}"))
        });
    runtime.join("qos/services")
}

pub fn socket_for(name: &str) -> PathBuf {
    services_root().join(format!("{name}.sock"))
}

/// Where the session's CRDT fan-out broker listens.
///
/// The compositor binds this; the launcher dials it. Both call this
/// function rather than spelling the path out, which is the whole
/// point — see [`COLLAB_SERVICE`].
pub fn collab_socket() -> PathBuf {
    socket_for(COLLAB_SERVICE)
}

/// Create `$XDG_RUNTIME_DIR/qos/services/` if it does not exist.
///
/// Called by whoever binds a service socket — nothing else creates
/// this directory, and a `bind` into a missing parent is `ENOENT`.
/// Mode 0700: the directory is a set of live connections into the
/// session's services, and the sandbox boundary assumes only this
/// user's session can reach them.
pub fn ensure_services_root() -> std::io::Result<PathBuf> {
    let root = services_root();
    std::fs::create_dir_all(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Best-effort tighten: an inherited 0755 from an earlier
        // umask would widen who can reach the session's services.
        let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700));
    }
    Ok(root)
}

/// Walk the manifest's service requirements + open sockets. Returns
/// the list of resolved bindings (skipping absent optional services
/// with a warning).
///
/// `no_services` short-circuits the lookup: any required service that
/// would otherwise fail simply logs a warning and is skipped, and no
/// fds are opened. Used by tests and by `--no-services`.
pub fn resolve_service_binds(
    services: &BTreeMap<String, String>,
    no_services: bool,
) -> Result<Vec<ResolvedService>, ServiceError> {
    // Start child fds well above stdio (3) — using 17, 18, 19, … keeps
    // them out of the way of bwrap's internal fds (bwrap allocates from
    // ~10 upwards for `--seccomp` etc. in our pattern).
    let mut next_child_fd: i32 = 17;
    let mut out = Vec::with_capacity(services.len());

    for (name, raw_req) in services {
        let req = ServiceReq::parse(raw_req)
            .map_err(|e| ServiceError::BadRequirement(raw_req.clone(), e))?;
        let path = socket_for(name);

        if no_services {
            tracing::warn!(
                "service `{name}` skipped: --no-services in effect (required={})",
                req.is_required()
            );
            continue;
        }

        if !path.exists() {
            if req.is_required() {
                return Err(ServiceError::RequiredMissing {
                    name: name.clone(),
                    version: req.version(),
                    path,
                });
            }
            tracing::warn!(
                "optional service `{name}` not available at {} — continuing without it",
                path.display()
            );
            continue;
        }

        // A socket file that exists is not the same as a service that
        // answers. A session that died without unlinking leaves the
        // inode behind, and connecting to it gets ECONNREFUSED. That
        // is the *same* condition as an absent socket as far as the
        // app is concerned — the service is not there — so it has to
        // be treated the same way, or a leftover file from a crash
        // turns every subsequent launch into a failed one.
        let stream = match UnixStream::connect(&path) {
            Ok(stream) => stream,
            Err(source) => {
                if req.is_required() {
                    return Err(ServiceError::OpenFailed { path, source });
                }
                tracing::warn!(
                    "optional service `{name}` has a socket at {} that refuses connections \
                     ({source}) — continuing without it",
                    path.display()
                );
                continue;
            }
        };
        let host_fd = stream.into_raw_fd();
        // Clear FD_CLOEXEC so the fd survives the bwrap exec. bwrap
        // sets up the child explicitly anyway, but stripping CLOEXEC
        // here is harmless and avoids the "fd vanished post-exec"
        // class of bug if the spawn path changes.
        unsafe {
            let flags = libc::fcntl(host_fd, libc::F_GETFD);
            if flags != -1 {
                let _ = libc::fcntl(host_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
            }
        }

        let child_fd = next_child_fd;
        next_child_fd += 1;

        out.push(ResolvedService {
            name: name.clone(),
            host_fd,
            child_fd,
        });
    }

    Ok(out)
}

/// Convert resolved services into `ServiceBind`s consumable by the
/// sandbox argv builder.
pub fn to_service_binds(resolved: &[ResolvedService]) -> Vec<crate::sandbox::ServiceBind> {
    resolved
        .iter()
        .map(|s| crate::sandbox::ServiceBind {
            name: s.name.clone(),
            host_fd: s.host_fd,
            child_fd: s.child_fd,
        })
        .collect()
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A socket file with nothing listening on it — exactly what a
    /// session that exited without unlinking leaves in the services
    /// directory. Binding and dropping the listener is what produces
    /// it: `std`'s `UnixListener` does not remove the inode on drop.
    fn dead_socket(root: &std::path::Path, name: &str) -> PathBuf {
        let path = root.join(format!("{name}.sock"));
        drop(std::os::unix::net::UnixListener::bind(&path).expect("bind the fixture socket"));
        assert!(path.exists(), "a stale socket must still be on disk");
        assert!(
            UnixStream::connect(&path).is_err(),
            "the fixture must actually refuse connections, or it proves nothing"
        );
        path
    }

    #[test]
    #[serial_test::serial]
    fn optional_service_whose_socket_refuses_is_skipped_not_fatal() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let root = ensure_services_root().unwrap();
        dead_socket(&root, "collab");

        let mut svcs = BTreeMap::new();
        svcs.insert("collab".to_string(), "optional@1".to_string());
        let out = resolve_service_binds(&svcs, false)
            .expect("a stale optional socket must degrade, not fail the launch");
        assert!(
            out.is_empty(),
            "nothing can be handed to the app for a service that does not answer"
        );
    }

    #[test]
    #[serial_test::serial]
    fn required_service_whose_socket_refuses_is_still_an_error() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let root = ensure_services_root().unwrap();
        dead_socket(&root, "collab");

        let mut svcs = BTreeMap::new();
        svcs.insert("collab".to_string(), "required@1".to_string());
        let err = resolve_service_binds(&svcs, false)
            .expect_err("a required service that does not answer must fail the launch");
        assert!(matches!(err, ServiceError::OpenFailed { .. }), "got {err:?}");
    }

    #[test]
    #[serial_test::serial]
    fn a_live_optional_socket_is_handed_over() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let root = ensure_services_root().unwrap();
        // Kept alive for the duration of the call, so `connect` succeeds.
        let _listener =
            std::os::unix::net::UnixListener::bind(root.join("collab.sock")).unwrap();

        let mut svcs = BTreeMap::new();
        svcs.insert("collab".to_string(), "optional@1".to_string());
        let out = resolve_service_binds(&svcs, false).expect("a live socket resolves");
        assert_eq!(out.len(), 1, "the one live service must be handed over");
        assert_eq!(out[0].name, "collab");
        assert_eq!(
            out[0].child_fd, 17,
            "the app is told to read fd 17, so that is where it must land"
        );
    }

    #[test]
    fn no_services_flag_skips_required_silently() {
        let mut svcs = BTreeMap::new();
        svcs.insert("collab".to_string(), "required@1".to_string());
        let out = resolve_service_binds(&svcs, true).expect("no-services path is infallible");
        assert!(out.is_empty(), "required service must be skipped");
    }

    #[test]
    fn malformed_requirement_is_an_error() {
        let mut svcs = BTreeMap::new();
        svcs.insert("collab".to_string(), "wrong-form".to_string());
        let err = resolve_service_binds(&svcs, false).unwrap_err();
        assert!(matches!(err, ServiceError::BadRequirement(_, _)));
    }

    #[test]
    #[serial_test::serial]
    fn required_missing_returns_required_missing_error() {
        let mut svcs = BTreeMap::new();
        svcs.insert("collab".to_string(), "required@1".to_string());
        // Point XDG_RUNTIME_DIR at a tempdir with nothing in it.
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let err = resolve_service_binds(&svcs, false).unwrap_err();
        match err {
            ServiceError::RequiredMissing { name, version, .. } => {
                assert_eq!(name, "collab");
                assert_eq!(version, 1);
            }
            other => panic!("expected RequiredMissing, got {other:?}"),
        }
    }
}
