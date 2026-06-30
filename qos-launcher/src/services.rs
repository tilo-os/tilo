//! Service-socket discovery + SCM_RIGHTS handoff.
//!
//! Each declared service maps to a Unix socket at
//! `$XDG_RUNTIME_DIR/qos/services/<name>.sock`. The launcher:
//!
//! 1. Looks up the socket per the manifest's service requirements.
//! 2. For `required@<v>` whose socket is absent → exit 70 with a
//!    descriptive error.
//! 3. For `optional@<v>` whose socket is absent → log a warning,
//!    continue.
//! 4. For sockets that exist → open the socket as a `UnixStream`, take
//!    its raw fd, and hand it to bwrap via the `--file <host_fd>
//!    /<in-sandbox-path>` argv triple (see [`sandbox::build_bwrap_argv`]).
//!    `bwrap` copies the descriptor into the child via the kernel's
//!    fd-passing mechanism — the same mechanism Linux exposes as
//!    `SCM_RIGHTS` over a Unix socket. We don't need a per-launcher
//!    SCM_RIGHTS dance: bwrap already plumbs the fd across the
//!    namespace/exec boundary.
//!
//! ### Open design question — where to mediate the handoff?
//!
//! There are three plausible mediation points:
//!
//! - bwrap `--file <fd>` (this module's choice): simplest, no extra
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

        let stream = UnixStream::connect(&path).map_err(|e| ServiceError::OpenFailed {
            path: path.clone(),
            source: e,
        })?;
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
