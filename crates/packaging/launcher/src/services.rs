//! Service-socket discovery + `SCM_RIGHTS` handoff.
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
//!    keep it, owned, for the length of the launch. `run::place_service_fds`
//!    dup2s it onto the agreed `child_fd` in the forked child, and `dup2`
//!    leaves that copy without CLOEXEC, so it survives bwrap's exec and
//!    the `__sandbox-exec` re-exec at that number. The app reads the
//!    number from `QOS_SVC_<NAME>_FD`. **The launcher's own copy keeps
//!    CLOEXEC** — nothing else it starts has any business holding an
//!    app's connection to a session service — and is released once the
//!    child has been forked.
//!
//! bwrap is deliberately not told about the socket: `--file FD DEST`
//! looks like the flag for this and isn't — it copies the descriptor's
//! *contents* into a regular file, so pointing it at a connected socket
//! makes bwrap block reading for an EOF that never arrives, hanging the
//! launch before it ever execs the app.
//!
//! ### Open design question — where to mediate the handoff?
//!
//! Three plausible mediation points: inherited descriptors (this
//! module's choice — simplest, no broker, fd at a known number), a
//! dedicated broker over `SCM_RIGHTS` (richer — revoke/audit/hot-swap —
//! but adds latency and a daemon dependency), or the compositor itself
//! handing fds to an inherited launcher (tightest coupling, hardest to
//! test).
//!
//! Today we pick option 1. If revocation/auditing matters later, the
//! launcher's API stays the same — only `resolve_service_binds` changes
//! to dial the broker instead of opening the socket directly.

use std::collections::BTreeMap;
use std::io::Write;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use crate::manifest::{ServiceReq, ServiceReqParseError};

/// Exit code for "required service missing" — mirrors `EX_SOFTWARE`
/// (70) from sysexits.h.
pub const EXIT_REQUIRED_SERVICE_MISSING: i32 = 70;

/// Manifest name of the CRDT fan-out service (`[sandbox.services] collab`).
///
/// This constant is the single point that decides where the collab
/// socket lives, with two owners on opposite sides of a namespace
/// boundary: the compositor *binds* [`socket_for`] of this name, and
/// the launcher *connects* it. They were once two independently-written
/// path expressions, so the compositor bound a socket nobody dialled
/// and every app quietly ran with collaboration off. Route both sides
/// through here — see
/// `collab_service_socket_is_the_path_the_compositor_binds` in the
/// compositor for the test that fails if they drift apart again.
pub const COLLAB_SERVICE: &str = "collab";
pub const IDENTITY_SERVICE: &str = "identity";
pub const SHELL_UI_SERVICE: &str = "shell-ui";
pub const COMPAT_SERVICE: &str = "compat";
/// Hosted model planning is performed by this per-user, desktop-owned
/// gateway. The launcher authenticates the consuming package before handing
/// it the connected descriptor, so a same-UID process cannot manufacture a
/// Settings credential operation by merely connecting to the socket.
pub const MODEL_GATEWAY_SERVICE: &str = "model-gateway";
const COMPAT_PROTOCOL_VERSION: u16 = 1;
/// Length-prefixed launcher handoff consumed before Cap'n Proto starts on a
/// collaboration descriptor. The broker binds it to `SO_PEERCRED` and a
/// compositor-created one-shot launch record.
pub const COLLAB_LAUNCH_PROTOCOL_VERSION: u16 = 1;
/// Prefix that separates a launcher-authenticated connection from a raw
/// Cap'n Proto diagnostic client. It is deliberately not an authority: the
/// broker still verifies the peer process against the compositor's one-shot
/// record before it accepts the following frame.
pub const COLLAB_LAUNCH_MAGIC: [u8; 4] = *b"QCL1";

/// One resolved service handoff: the launcher's own end of the
/// connection, plus the in-sandbox fd number the app should find it at.
#[derive(Debug)]
pub struct ResolvedService {
    pub name: String,
    /// The launcher's end, **owned**, closed when the launch is over. The
    /// app's descriptor and this one are two descriptors on one socket, and
    /// the far end can't see the connection end until the last goes.
    pub host: OwnedFd,
    /// Where the app is told to look for it.
    pub child_fd: i32,
}

impl ResolvedService {
    /// The number to `dup2` onto [`Self::child_fd`] in the child. Borrowed
    /// for the spawn: the copy that outlives this one is the one `dup2`
    /// makes on the other side of the fork.
    pub fn host_fd(&self) -> RawFd {
        self.host.as_raw_fd()
    }
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
    #[error("required compatibility service has no authenticated workspace launch context")]
    CompatContextMissing,
    #[error("failed to authenticate the launch to compatibility service `{path}`: {source}")]
    CompatHandoffFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("required collaboration service has no authenticated workspace launch context")]
    CollabContextMissing,
    #[error("failed to authenticate the launch to collaboration service `{path}`: {source}")]
    CollabHandoffFailed {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct ServiceLaunchContext<'a> {
    pub app_id: &'a str,
    pub workspace_id: Option<&'a str>,
}

/// Discover `$XDG_RUNTIME_DIR/qos/services/`.
pub fn services_root() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map_or_else(
        || {
            let uid = unsafe { libc::geteuid() };
            PathBuf::from(format!("/run/user/{uid}"))
        },
        PathBuf::from,
    );
    runtime.join("qos/services")
}

pub fn socket_for(name: &str) -> PathBuf {
    services_root().join(format!("{name}.sock"))
}

/// Where the session's CRDT fan-out broker listens.
///
/// The compositor binds this; the launcher dials it. Both call this
/// function rather than spelling the path out — see [`COLLAB_SERVICE`].
pub fn collab_socket() -> PathBuf {
    socket_for(COLLAB_SERVICE)
}

pub fn identity_socket() -> PathBuf {
    socket_for(IDENTITY_SERVICE)
}

/// Authenticate the optional shell-UI channel before the child enters its
/// sandbox. The app cannot forge or race this first frame: the launcher writes
/// the canonical package/app id on the connected descriptor before `fork`,
/// and the compositor refuses lifecycle messages until it has consumed it.
///
/// # Errors
///
/// Returns [`ServiceError`] if the service descriptor cannot be duplicated,
/// the authentication frame cannot be serialized, or the frame cannot be
/// written and flushed to the shell-UI socket.
pub fn authenticate_shell_ui_bind(
    resolved: &[ResolvedService],
    app_id: &str,
) -> Result<(), ServiceError> {
    let Some(service) = resolved
        .iter()
        .find(|service| service.name == SHELL_UI_SERVICE)
    else {
        return Ok(());
    };
    let path = socket_for(SHELL_UI_SERVICE);
    // Duplicate only for a temporary writer; the original descriptor remains
    // owned by `ResolvedService` for the child handoff.
    let borrowed = unsafe { BorrowedFd::borrow_raw(service.host_fd()) };
    let owned = borrowed
        .try_clone_to_owned()
        .map_err(|source| ServiceError::OpenFailed {
            path: path.clone(),
            source,
        })?;
    let mut stream = UnixStream::from(owned);
    let mut frame = serde_json::to_vec(&serde_json::json!({
        "protocol": 1,
        "app_id": app_id,
    }))
    .map_err(|error| ServiceError::OpenFailed {
        path: path.clone(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
    })?;
    frame.push(b'\n');
    stream
        .write_all(&frame)
        .and_then(|()| stream.flush())
        .map_err(|source| ServiceError::OpenFailed { path, source })
}

/// Connect the per-session identity broker and authenticate the launch
/// context before the sandboxed app can write to the stream.
///
/// # Errors
///
/// Returns [`ServiceError`] if the handshake can't be serialized, if it's
/// too large to fit a `u32` length prefix, or if writing/flushing it to the
/// socket fails. A missing service or a refused connection is not an error
/// — both degrade to `Ok(None)`.
pub fn resolve_identity_bind(
    app_id: &str,
    workspace_id: Option<&str>,
    no_services: bool,
) -> Result<Option<ResolvedService>, ServiceError> {
    let Some(workspace_id) = workspace_id else {
        return Ok(None);
    };
    if no_services {
        return Ok(None);
    }
    let path = identity_socket();
    if !path.exists() {
        tracing::warn!("identity service is unavailable; app receives public metadata only");
        return Ok(None);
    }
    let mut stream = match UnixStream::connect(&path) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "identity service refused connection");
            return Ok(None);
        }
    };
    let handshake = serde_json::json!({
        "workspace_id": workspace_id,
        "app_id": app_id,
    });
    let bytes = serde_json::to_vec(&handshake).map_err(|error| ServiceError::OpenFailed {
        path: path.clone(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
    })?;
    let len = u32::try_from(bytes.len()).map_err(|_| ServiceError::OpenFailed {
        path: path.clone(),
        source: std::io::Error::new(std::io::ErrorKind::InvalidInput, "handshake too large"),
    })?;
    stream
        .write_all(&len.to_le_bytes())
        .and_then(|()| stream.write_all(&bytes))
        .and_then(|()| stream.flush())
        .map_err(|source| ServiceError::OpenFailed { path, source })?;
    Ok(Some(ResolvedService {
        name: IDENTITY_SERVICE.to_string(),
        host: OwnedFd::from(stream),
        // Normal manifest services start at 17. Keep the automatic service in
        // a reserved slot so it cannot collide as the manifest evolves.
        child_fd: 31,
    }))
}

/// Create `$XDG_RUNTIME_DIR/qos/services/` if it does not exist.
///
/// Called by whoever binds a service socket — a `bind` into a missing
/// parent is `ENOENT`. Mode 0700: the sandbox boundary assumes only
/// this user's session can reach these live connections.
///
/// # Errors
///
/// Returns an error if the directory cannot be created.
pub fn ensure_services_root() -> std::io::Result<PathBuf> {
    let root = services_root();
    std::fs::create_dir_all(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // Best-effort tighten: an inherited 0755 from an earlier umask would
        // widen who can reach the session's services.
        let _ = std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700));
    }
    Ok(root)
}

/// Walk the manifest's service requirements + open sockets. Returns the
/// resolved bindings (skipping absent optional services with a warning).
///
/// `no_services` short-circuits the lookup: any required service that
/// would otherwise fail simply logs a warning and is skipped, and no
/// fds are opened.
///
/// # Errors
///
/// Returns [`ServiceError::BadRequirement`] if a manifest entry doesn't
/// parse, [`ServiceError::RequiredMissing`] if a required service's socket
/// doesn't exist, or [`ServiceError::OpenFailed`] if a required service's
/// socket exists but refuses the connection.
pub fn resolve_service_binds(
    services: &BTreeMap<String, String>,
    no_services: bool,
) -> Result<Vec<ResolvedService>, ServiceError> {
    resolve_service_binds_inner(services, no_services, None)
}

/// Resolve service descriptors and write the compositor-authenticated
/// compatibility preamble before the descriptor enters the app sandbox.
///
/// # Errors
///
/// Returns the same service resolution errors as [`resolve_service_binds`],
/// plus a context or handoff error when the `compat` service is connected.
pub fn resolve_service_binds_for_launch(
    services: &BTreeMap<String, String>,
    no_services: bool,
    context: ServiceLaunchContext<'_>,
) -> Result<Vec<ResolvedService>, ServiceError> {
    resolve_service_binds_inner(services, no_services, Some(context))
}

fn resolve_service_binds_inner(
    services: &BTreeMap<String, String>,
    no_services: bool,
    context: Option<ServiceLaunchContext<'_>>,
) -> Result<Vec<ResolvedService>, ServiceError> {
    // Start child fds well above stdio (3) — 17, 18, 19… keeps them out of
    // the way of bwrap's own fds (allocated from ~10 upwards).
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
        // answers. A session that died without unlinking leaves the inode
        // behind, and connecting to it gets ECONNREFUSED — the same condition
        // as an absent socket, so it has to be treated the same way, or a
        // leftover file from a crash turns every subsequent launch into a
        // failed one.
        let mut stream = match UnixStream::connect(&path) {
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
        if name == COLLAB_SERVICE {
            let Some((context, workspace_id)) = context
                .and_then(|context| context.workspace_id.map(|workspace| (context, workspace)))
            else {
                if req.is_required() {
                    return Err(ServiceError::CollabContextMissing);
                }
                tracing::warn!(
                    "optional collaboration service skipped without authenticated workspace context"
                );
                continue;
            };
            if let Err(source) = write_collab_handoff(&mut stream, context.app_id, workspace_id) {
                if req.is_required() {
                    return Err(ServiceError::CollabHandoffFailed { path, source });
                }
                tracing::warn!(%source, "optional collaboration service rejected launch handoff");
                continue;
            }
        }
        if name == COMPAT_SERVICE {
            let Some((context, workspace_id)) = context
                .and_then(|context| context.workspace_id.map(|workspace| (context, workspace)))
            else {
                if req.is_required() {
                    return Err(ServiceError::CompatContextMissing);
                }
                tracing::warn!("optional compatibility service skipped without workspace context");
                continue;
            };
            if let Err(source) = write_compat_handoff(&mut stream, context.app_id, workspace_id) {
                if req.is_required() {
                    return Err(ServiceError::CompatHandoffFailed { path, source });
                }
                tracing::warn!(%source, "optional compatibility service rejected launch handoff");
                continue;
            }
        }
        if name == MODEL_GATEWAY_SERVICE {
            let Some(context) = context else {
                if req.is_required() {
                    return Err(ServiceError::OpenFailed {
                        path,
                        source: std::io::Error::other("model gateway requires launcher context"),
                    });
                }
                tracing::warn!("optional model gateway skipped without launcher context");
                continue;
            };
            if let Err(source) = write_model_gateway_handoff(&mut stream, context.app_id) {
                if req.is_required() {
                    return Err(ServiceError::OpenFailed { path, source });
                }
                tracing::warn!(%source, "optional model gateway rejected launch handoff");
                continue;
            }
        }
        // Owned from here, and deliberately left `FD_CLOEXEC`. The fork+dup2
        // never sets CLOEXEC on the descriptor it creates, so the app's copy
        // survives the exec whatever this one says. Clearing the flag here was
        // described as harmless and was not: it handed this descriptor to
        // every process started later in the same launch, including the D-Bus
        // proxy and the egress gate.
        let host = OwnedFd::from(stream);

        if next_child_fd == 31 {
            next_child_fd = next_child_fd.saturating_add(1);
        }
        let child_fd = next_child_fd;
        next_child_fd = next_child_fd.saturating_add(1);

        out.push(ResolvedService {
            name: name.clone(),
            host,
            child_fd,
        });
    }

    Ok(out)
}

fn write_collab_handoff(
    stream: &mut UnixStream,
    app_id: &str,
    workspace_id: &str,
) -> std::io::Result<()> {
    let value = serde_json::json!({
        "protocol": COLLAB_LAUNCH_PROTOCOL_VERSION,
        "app_id": app_id,
        "workspace_id": workspace_id,
    });
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let length = u32::try_from(bytes.len())
        .map_err(|_| std::io::Error::other("collaboration handoff is too large"))?;
    stream.write_all(&COLLAB_LAUNCH_MAGIC)?;
    stream.write_all(&length.to_le_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()
}

fn write_compat_handoff(
    stream: &mut UnixStream,
    app_id: &str,
    workspace_id: &str,
) -> std::io::Result<()> {
    let value = serde_json::json!({
        "protocol": COMPAT_PROTOCOL_VERSION,
        "app_id": app_id,
        "workspace_id": workspace_id,
    });
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let length = u32::try_from(bytes.len())
        .map_err(|_| std::io::Error::other("compatibility handoff is too large"))?;
    stream.write_all(&length.to_le_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()
}

fn write_model_gateway_handoff(stream: &mut UnixStream, app_id: &str) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "protocol": 1,
        "app_id": app_id,
    }))
    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let length = u32::try_from(bytes.len())
        .map_err(|_| std::io::Error::other("model gateway handoff is too large"))?;
    stream.write_all(&length.to_le_bytes())?;
    stream.write_all(&bytes)?;
    stream.flush()
}

/// Convert resolved services into `ServiceBind`s consumable by the
/// sandbox argv builder.
pub fn to_service_binds(resolved: &[ResolvedService]) -> Vec<crate::sandbox::ServiceBind> {
    resolved
        .iter()
        .map(|s| crate::sandbox::ServiceBind {
            name: s.name.clone(),
            host_fd: s.host_fd(),
            child_fd: s.child_fd,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    /// A socket file with nothing listening on it — what a session that
    /// exited without unlinking leaves behind. Binding and dropping the
    /// listener produces it: `std`'s `UnixListener` doesn't remove the
    /// inode on drop.
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
    #[serial_test::serial(process_env)]
    fn identity_handoff_is_bound_to_launcher_app_and_workspace_context() {
        use std::io::Read;

        let dir = tempfile::tempdir().expect("runtime");
        let previous = std::env::var_os("XDG_RUNTIME_DIR");
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let root = ensure_services_root().expect("services root");
        let path = root.join("identity.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).expect("listen");
        let reader = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut len = [0_u8; 4];
            stream.read_exact(&mut len).expect("length");
            let mut bytes = vec![0_u8; u32::from_le_bytes(len) as usize];
            stream.read_exact(&mut bytes).expect("payload");
            serde_json::from_slice::<serde_json::Value>(&bytes).expect("json")
        });
        let bind = resolve_identity_bind("dev.qos.notes", Some("room-7"), false)
            .expect("resolve")
            .expect("identity bind");
        assert_eq!(bind.name, IDENTITY_SERVICE);
        assert_eq!(bind.child_fd, 31);
        let handshake = reader.join().expect("reader");
        assert_eq!(handshake["app_id"], "dev.qos.notes");
        assert_eq!(handshake["workspace_id"], "room-7");

        unsafe {
            match previous {
                Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn collab_handoff_is_framed_for_broker_authentication() {
        use std::io::Read;

        let dir = tempfile::tempdir().expect("runtime");
        let previous = std::env::var_os("XDG_RUNTIME_DIR");
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let root = ensure_services_root().expect("services root");
        let listener =
            std::os::unix::net::UnixListener::bind(root.join("collab.sock")).expect("listen");
        let reader = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            let mut magic = [0_u8; COLLAB_LAUNCH_MAGIC.len()];
            stream.read_exact(&mut magic).expect("magic");
            let mut len = [0_u8; 4];
            stream.read_exact(&mut len).expect("length");
            let mut bytes = vec![0_u8; u32::from_le_bytes(len) as usize];
            stream.read_exact(&mut bytes).expect("payload");
            (
                magic,
                serde_json::from_slice::<serde_json::Value>(&bytes).expect("json"),
            )
        });
        let mut services = BTreeMap::new();
        services.insert(COLLAB_SERVICE.to_string(), "required@1".to_string());
        let resolved = resolve_service_binds_for_launch(
            &services,
            false,
            ServiceLaunchContext {
                app_id: "dev.qos.notes",
                workspace_id: Some("room-7"),
            },
        )
        .expect("resolve");
        drop(resolved);
        let (magic, handoff) = reader.join().expect("reader");
        assert_eq!(magic, COLLAB_LAUNCH_MAGIC);
        assert_eq!(handoff["protocol"], COLLAB_LAUNCH_PROTOCOL_VERSION);
        assert_eq!(handoff["app_id"], "dev.qos.notes");
        assert_eq!(handoff["workspace_id"], "room-7");

        unsafe {
            match previous {
                Some(value) => std::env::set_var("XDG_RUNTIME_DIR", value),
                None => std::env::remove_var("XDG_RUNTIME_DIR"),
            }
        }
    }

    #[test]
    #[serial_test::serial(process_env)]
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
    #[serial_test::serial(process_env)]
    fn required_service_whose_socket_refuses_is_still_an_error() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let root = ensure_services_root().unwrap();
        dead_socket(&root, "collab");

        let mut svcs = BTreeMap::new();
        svcs.insert("collab".to_string(), "required@1".to_string());
        let err = resolve_service_binds(&svcs, false)
            .expect_err("a required service that does not answer must fail the launch");
        assert!(
            matches!(err, ServiceError::OpenFailed { .. }),
            "got {err:?}"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn a_live_optional_socket_is_handed_over() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let root = ensure_services_root().unwrap();
        // Kept alive for the duration of the call, so `connect` succeeds.
        let _listener =
            std::os::unix::net::UnixListener::bind(root.join("test-lifecycle.sock")).unwrap();

        let mut svcs = BTreeMap::new();
        svcs.insert("test-lifecycle".to_string(), "optional@1".to_string());
        let out = resolve_service_binds(&svcs, false).expect("a live socket resolves");
        assert_eq!(out.len(), 1, "the one live service must be handed over");
        assert_eq!(out[0].name, "test-lifecycle");
        assert_eq!(
            out[0].child_fd, 17,
            "the app is told to read fd 17, so that is where it must land"
        );
    }

    #[test]
    #[serial_test::serial(process_env)]
    fn shell_ui_connection_is_authenticated_before_child_handoff() {
        use std::io::{BufRead, BufReader};

        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let root = ensure_services_root().unwrap();
        let listener = std::os::unix::net::UnixListener::bind(root.join("shell-ui.sock")).unwrap();
        let mut services = BTreeMap::new();
        services.insert(SHELL_UI_SERVICE.to_string(), "optional@1".to_string());
        let resolved = resolve_service_binds(&services, false).unwrap();
        authenticate_shell_ui_bind(&resolved, "dev.qos.chat").unwrap();

        let (stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).unwrap();
        let handshake: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
        assert_eq!(handshake["protocol"], 1);
        assert_eq!(handshake["app_id"], "dev.qos.chat");
    }

    /// What `/proc/self/fd/<n>` says a descriptor points at — `socket:[<inode>]`
    /// for a connected socket, unique to that connection. Identifying
    /// descriptors this way rather than counting the process's is the
    /// difference between a check and a race.
    fn fd_target(fd: RawFd) -> Option<String> {
        std::fs::read_link(format!("/proc/self/fd/{fd}"))
            .ok()
            .map(|p| p.display().to_string())
    }

    /// Every socket this process still holds a descriptor for. Failing to
    /// read `/proc/self/fd` fails the test — a check that can't see its
    /// subject must not report the subject is fine.
    fn open_sockets() -> std::collections::BTreeSet<String> {
        std::fs::read_dir("/proc/self/fd")
            .expect(
                "/proc/self/fd is how this test observes descriptors; without \
                 it there is nothing here to assert",
            )
            .filter_map(std::result::Result::ok)
            .filter_map(|entry| std::fs::read_link(entry.path()).ok())
            .map(|target| target.display().to_string())
            .filter(|target| target.starts_with("socket:"))
            .collect()
    }

    /// A launch that is over must leave no descriptor behind.
    ///
    /// Nothing in this repository notices a leaked descriptor, which is why
    /// one sat in the launch path unremarked. Stands in for `run_with`'s
    /// cycle at the only part that touches descriptors: resolve, read the
    /// numbers the fork will need, let the resolution go.
    #[test]
    #[serial_test::serial(process_env)]
    fn a_finished_launch_leaves_no_descriptor_behind() {
        const LAUNCHES: usize = 8;

        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let root = ensure_services_root().unwrap();
        let _listener =
            std::os::unix::net::UnixListener::bind(root.join("test-lifecycle.sock")).unwrap();

        let mut svcs = BTreeMap::new();
        svcs.insert("test-lifecycle".to_string(), "optional@1".to_string());

        let mut handed_out: Vec<String> = Vec::with_capacity(LAUNCHES);
        for _ in 0..LAUNCHES {
            let resolved = resolve_service_binds(&svcs, false).expect("a live socket resolves");
            let target = fd_target(resolved[0].host_fd())
                .expect("the connection a launch was handed must be open");
            assert!(
                target.starts_with("socket:"),
                "the launch was handed {target}, which is not a connection",
            );
            handed_out.push(target);
            // What the spawn does with them: read the numbers for the
            // `pre_exec` dup2 list, fork, and have no further use for this
            // side of the pair.
            let _pairs: Vec<(RawFd, i32)> =
                resolved.iter().map(|r| (r.host_fd(), r.child_fd)).collect();
            drop(resolved);
        }

        let still_open = open_sockets();
        let survivors: Vec<&String> = handed_out
            .iter()
            .filter(|target| still_open.contains(*target))
            .collect();
        assert!(
            survivors.is_empty(),
            "{} of {LAUNCHES} finished launches left their service connection \
             open ({survivors:?}); one per launch is a process that \
             accumulates live connections to its session's services for as \
             long as it runs",
            survivors.len(),
        );
    }

    /// Asserted from the *service's* side of the socket: "we called close"
    /// isn't the claim, "the far end can tell" is. Taken as a raw fd,
    /// nothing owned or closed this descriptor, so every caller held a live
    /// connection nothing would ever release.
    #[test]
    #[serial_test::serial(process_env)]
    fn the_launchers_end_is_released_when_the_launch_is_over() {
        use std::io::Read;

        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let root = ensure_services_root().unwrap();
        let listener =
            std::os::unix::net::UnixListener::bind(root.join("test-lifecycle.sock")).unwrap();

        let mut svcs = BTreeMap::new();
        svcs.insert("test-lifecycle".to_string(), "optional@1".to_string());
        let resolved = resolve_service_binds(&svcs, false).expect("a live socket resolves");
        assert_eq!(
            resolved.len(),
            1,
            "the one live service must be handed over"
        );

        let (mut served, _) = listener
            .accept()
            .expect("the launcher's connection has to arrive");
        served
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .expect("a bounded wait, so a leak fails the test instead of hanging it");

        // The launch is over: either bwrap was spawned and holds the `dup2`
        // copy, or the spawn failed and there is no child at all.
        drop(resolved);

        let mut buf = [0u8; 1];
        let n = served.read(&mut buf).unwrap_or_else(|e| {
            panic!(
                "the service never saw the launcher's end close ({e}), so it is \
                 still holding a connection nothing will ever release"
            )
        });
        assert_eq!(
            n, 0,
            "the read returned data on a connection nobody is on the other end of",
        );
    }

    /// The other order, driven explicitly: a fix that released the
    /// descriptor too early would close the app's connection at the moment
    /// it was handed over, and a test only watching for EOF would call
    /// that a pass.
    #[test]
    #[serial_test::serial(process_env)]
    fn the_apps_copy_outlives_the_launchers() {
        use std::io::Read;
        use std::os::fd::FromRawFd;

        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let root = ensure_services_root().unwrap();
        let listener =
            std::os::unix::net::UnixListener::bind(root.join("test-lifecycle.sock")).unwrap();

        let mut svcs = BTreeMap::new();
        svcs.insert("test-lifecycle".to_string(), "optional@1".to_string());
        let resolved = resolve_service_binds(&svcs, false).expect("a live socket resolves");
        let (mut served, _) = listener.accept().expect("the connection has to arrive");

        // What `place_service_fds` makes on the other side of the fork:
        // another descriptor on the same socket. It stands in for the app.
        let app_copy = unsafe {
            let raw = libc::dup(resolved[0].host_fd());
            assert!(raw >= 0, "dup of the handed-over descriptor failed");
            OwnedFd::from_raw_fd(raw)
        };
        drop(resolved);

        served
            .set_read_timeout(Some(std::time::Duration::from_millis(250)))
            .unwrap();
        let mut buf = [0u8; 1];
        match served.read(&mut buf) {
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Ok(0) => panic!(
                "the app's connection was closed the moment the launcher let go \
                 of its own copy, so the app is handed a descriptor to a socket \
                 nobody is serving"
            ),
            other => panic!("unexpected read from a live connection: {other:?}"),
        }

        drop(app_copy);
        served
            .set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        assert_eq!(
            served.read(&mut buf).expect("the service must see the end"),
            0,
            "every copy is gone and the service still sees a live connection",
        );
    }

    /// The launcher's copy must not be inheritable — with CLOEXEC cleared,
    /// every process started after the fork (D-Bus proxy, egress gate)
    /// would hold a live connection to a session service opened on an
    /// app's behalf.
    #[test]
    #[serial_test::serial(process_env)]
    fn the_launchers_copy_is_not_inherited_by_the_next_child() {
        let dir = tempfile::tempdir().unwrap();
        unsafe { std::env::set_var("XDG_RUNTIME_DIR", dir.path()) };
        let root = ensure_services_root().unwrap();
        let _listener =
            std::os::unix::net::UnixListener::bind(root.join("test-lifecycle.sock")).unwrap();

        let mut svcs = BTreeMap::new();
        svcs.insert("test-lifecycle".to_string(), "optional@1".to_string());
        let resolved = resolve_service_binds(&svcs, false).expect("a live socket resolves");
        let raw = resolved[0].host_fd();

        let flags = unsafe { libc::fcntl(raw, libc::F_GETFD) };
        assert!(flags != -1, "the handed-over descriptor is not open");
        assert!(
            flags & libc::FD_CLOEXEC != 0,
            "the launcher's end is inheritable, so every process started after \
             it gets a live copy of it",
        );

        // The property itself rather than the flag that decides it.
        let seen = std::process::Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("readlink /proc/self/fd/{raw} 2>/dev/null || true"))
            .output()
            .expect("spawning an unrelated child");
        let seen = String::from_utf8_lossy(&seen.stdout);
        assert!(
            !seen.trim_start().starts_with("socket:"),
            "an unrelated child inherited the service socket at fd {raw}: {seen:?}",
        );
        drop(resolved);
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
    #[serial_test::serial(process_env)]
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
