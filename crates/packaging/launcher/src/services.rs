//! Service-socket discovery + `SCM_RIGHTS` handoff.
//!
//! Each declared service maps to a Unix socket at
//! `$XDG_RUNTIME_DIR/tilo/services/<name>.sock`. The launcher:
//!
//! 1. Looks up the socket per the manifest's service requirements.
//! 2. For `required@<v>` whose socket is absent → exit 70 with a
//!    descriptive error.
//! 3. For `optional@<v>` whose socket is absent — or present but
//!    refusing connections, which is what a session that died without
//!    unlinking leaves behind → log a warning, continue.
//! 4. For sockets that exist → open the socket as a `UnixStream` and
//!    keep it, owned, for the length of the launch. `run::place_child_fds`
//!    dup2s it onto the agreed `child_fd` in the forked child, and `dup2`
//!    leaves that copy without CLOEXEC, so it survives bwrap's exec and
//!    the `__sandbox-exec` re-exec at that number. The app reads the
//!    number from `TILO_SVC_<NAME>_FD`. **The launcher's own copy keeps
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
//! The launcher receives inherited descriptors directly: this keeps the
//! handoff local, avoids another daemon, and gives each service a fixed fd.

/// Native startup protocol selector; it conveys no authority.
pub use runtime_portal::process_binding::ENV as COLLAB_EXECUTING_PEER_ENV;
#[cfg(target_os = "linux")]
pub use runtime_portal::process_binding::ExecutingPeer;

use std::collections::BTreeMap;
use std::io::Write;
use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd, RawFd};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};

use wire_frame::Framing;

use crate::manifest::ServiceReq;

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
/// Version of the launcher-written shell-UI authentication envelope.
/// This names the app before descriptor handoff; it is independent of the
/// versioned requests the authenticated app subsequently sends.
pub const SHELL_UI_LAUNCH_PROTOCOL: u16 = 1;
/// The session shell's authenticated file chooser (`tilo_dialogs::shell`),
/// served alongside any desktop portal. Not declared by manifests: every app
/// launched into a room is handed a connection naming itself and the room.
pub const CHOOSER_SERVICE: &str = "chooser";
const CHOOSER_PROTOCOL: u16 = 1;
pub const COMPAT_SERVICE: &str = "compat";
/// The system interface: the image-owned grant through which the device's
/// control panel reaches the system services it drives
/// ([`crate::system_authority`]). Declared like a service so a package names
/// the version it was built against, but it has no socket.
pub const SYSTEM_SERVICE: &str = "system";
/// Hosted model planning is performed by this per-user, desktop-owned
/// gateway. The launcher authenticates the consuming package before handing
/// it the connected descriptor, so a same-UID process cannot manufacture a
/// Settings credential operation by merely connecting to the socket.
pub const MODEL_GATEWAY_SERVICE: &str = "model-gateway";
pub const APP_HOST_SERVICE: &str = "app-host";
/// The shell runtime's supervisor. Its connection is stamped with the app id
/// before the app holds it, so the supervisor knows which app is asking —
/// the device's own shell is answered for Terminal and nobody else.
pub const USH_SERVICE: &str = "ush";
/// Prefix of the `ush` handoff; the supervisor reads it before any request.
pub const USH_LAUNCH_MAGIC: [u8; 4] = *b"QUS1";
/// After [`USH_LAUNCH_MAGIC`]: a little-endian `u32` length, then
/// `{"protocol":1,"app_id":"…"}`.
const USH_HANDOFF_FRAMING: Framing = Framing::little_endian(1024).reject_empty();
/// Document media. Not declared by manifests: every app is handed a
/// capability scoped to itself and its room, so a document's images live in
/// the device content store rather than in the app's sandbox.
pub const BLOB_SERVICE: &str = "blobs";
/// The filesystem daemon's document-media socket (`runtime_fs::blobs`).
pub const DEFAULT_BLOB_SOCKET: &str = "/run/tilo/fs-blobs.sock";
/// Overrides [`DEFAULT_BLOB_SOCKET`] for sessions whose daemon runs elsewhere.
pub const BLOB_SOCKET_ENV: &str = "TILO_FS_BLOB_SOCKET";
/// Prefix of the handoff that fixes the connection's scope before the app
/// holds the descriptor; the daemon reads it before any Cap'n Proto.
pub const BLOB_HANDOFF_MAGIC: [u8; 4] = *b"QBL1";
const BLOB_HANDOFF_PROTOCOL: u16 = 1;
const BLOB_HANDOFF_FRAMING: Framing = Framing::little_endian(4096).reject_empty();
/// In-sandbox descriptor slot of the automatic document-media service.
const BLOB_CHILD_FD: i32 = 30;
/// Slot of the automatic identity service.
const IDENTITY_CHILD_FD: i32 = 31;
/// Slot of the automatic chooser service.
const CHOOSER_CHILD_FD: i32 = 32;
/// Slots bwrap reads its two seccomp programs from (`--add-seccomp-fd`).
/// They sit above bwrap's own descriptors and outside the numbers declared
/// services are handed on, so no service can replace a filter.
pub(crate) const CLONE3_COMPAT_SECCOMP_CHILD_FD: i32 = 40;
pub(crate) const BASELINE_SECCOMP_CHILD_FD: i32 = 41;
/// Synthetic sandbox account file, consumed by bwrap before app execution.
pub(crate) const ACCOUNT_CHILD_FD: i32 = 42;
/// Bwrap private-service namespace information, closed before app execution.
pub(crate) const SERVICE_NAMESPACE_CHILD_FD: i32 = 43;
/// Every fixed child slot; declared services are numbered around them.
/// Retained-home workers and views exist only in the Linux sandbox. Keep
/// portable service discovery independent of those modules.
pub(crate) const RESERVED_CHILD_FDS: &[i32] = &[
    runtime_identity::broker::BROKER_CHILD_FD,
    BLOB_CHILD_FD,
    IDENTITY_CHILD_FD,
    CHOOSER_CHILD_FD,
    CLONE3_COMPAT_SECCOMP_CHILD_FD,
    BASELINE_SECCOMP_CHILD_FD,
    ACCOUNT_CHILD_FD,
    SERVICE_NAMESPACE_CHILD_FD,
    #[cfg(target_os = "linux")]
    crate::owned_service::capture::SOURCE_FD,
    #[cfg(target_os = "linux")]
    crate::owned_service::capture::SHIM_FD,
    #[cfg(target_os = "linux")]
    crate::owned_service::capture::OUTPUT_FD,
    #[cfg(target_os = "linux")]
    crate::owned_service::capture::executable::BWRAP_FD,
    #[cfg(target_os = "linux")]
    crate::owned_service::capture::executable::LOADER_FD,
    #[cfg(target_os = "linux")]
    crate::owned_service::capture::original::BOOTSTRAP_FD,
    #[cfg(target_os = "linux")]
    crate::owned_service::capture::original::CLAIM_FD,
    #[cfg(target_os = "linux")]
    crate::owned_service::target_projection::TOKEN_FD,
    #[cfg(target_os = "linux")]
    crate::owned_service::target_projection::FUSE_FD,
    #[cfg(target_os = "linux")]
    crate::owned_service::target_projection::READY_FD,
    #[cfg(target_os = "linux")]
    crate::retained_view::VIEW_FD,
    #[cfg(target_os = "linux")]
    crate::retained_view::ALIAS_VIEW_FD,
    #[cfg(target_os = "linux")]
    crate::retained_view::ALIAS_ARGS_FD,
    #[cfg(target_os = "linux")]
    crate::system_stats::CHILD_FD,
    #[cfg(target_os = "linux")]
    crate::retained_view::SELECTED_PARENT_FD,
    #[cfg(target_os = "linux")]
    crate::owned_service::RETAINED_SELECTED_REPLACEMENT_PERMIT_FD,
    #[cfg(target_os = "linux")]
    crate::owned_service::RETAINED_NATIVE_HISTORY_VERIFIER_FD,
];

fn child_fd_reserved(fd: i32) -> bool {
    if RESERVED_CHILD_FDS.contains(&fd) {
        return true;
    }
    #[cfg(target_os = "linux")]
    {
        if crate::owned_service::capture::archive_reserved_fd(fd) {
            return true;
        }
    }
    false
}
const STORE_APP_ID: &str = "so.tilo.store";
/// The Browser holds `app-host` too, but is served only the request for a
/// runtime its own manifest declares (its engine), never the Store's
/// package-management interface: the host decides by the app id stamped on
/// the connection.
const BROWSER_APP_ID: &str = "so.tilo.browser";
pub const APP_HOST_LAUNCH_MAGIC: [u8; 4] = *b"QAH1";
/// After [`APP_HOST_LAUNCH_MAGIC`]: a big-endian `u32` length, then the JSON
/// handoff.
const APP_HOST_HANDOFF_FRAMING: Framing = Framing::big_endian(1024).reject_empty();
const COMPAT_PROTOCOL_VERSION: u16 = 1;
/// The compatibility service's handoff: a little-endian `u32` length, then
/// JSON.
pub const COMPAT_HANDOFF_FRAMING: Framing = Framing::little_endian(4 * 1024).reject_empty();
/// The model gateway's handoff: a little-endian `u32` length, then JSON.
const MODEL_GATEWAY_HANDOFF_FRAMING: Framing = Framing::little_endian(4 * 1024);
/// Length-prefixed launcher handoff consumed before Cap'n Proto starts on a
/// collaboration descriptor. The broker binds it to `SO_PEERCRED` and a
/// compositor-created one-shot launch record.
pub const COLLAB_LAUNCH_PROTOCOL_VERSION: u16 = 1;
/// Prefix that separates a launcher-authenticated connection from a raw
/// Cap'n Proto diagnostic client. It is deliberately not an authority: the
/// broker still verifies the peer process against the compositor's one-shot
/// record before it accepts the following frame.
pub const COLLAB_LAUNCH_MAGIC: [u8; 4] = *b"QCL1";
/// After [`COLLAB_LAUNCH_MAGIC`]: a little-endian `u32` length, then the JSON
/// handoff. It is deliberately tiny: it only binds a socket to the shell's
/// already-created process record, while the certificate the app presents
/// still decides workspace membership and delegated-agent authority.
pub const COLLAB_LAUNCH_FRAMING: Framing = Framing::little_endian(1024).reject_empty();

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
        "required service `{name}` (protocol tilo.svc.{name}@{version}) was not found at `{path}`"
    )]
    RequiredMissing {
        name: String,
        version: u32,
        path: PathBuf,
    },
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
    #[error("service `app-host` is restricted to the signed Tilo Store package")]
    AppHostUnauthorized,
}

#[derive(Clone, Copy, Debug)]
pub struct ServiceLaunchContext<'a> {
    pub app_id: &'a str,
    pub workspace_id: Option<&'a str>,
}

/// Discover `$XDG_RUNTIME_DIR/tilo/services/`.
pub fn services_root() -> PathBuf {
    tilo_paths::xdg::runtime_dir_or_default().join("tilo/services")
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
    egress: &crate::netgate::EgressDecision,
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
        "protocol": SHELL_UI_LAUNCH_PROTOCOL,
        "app_id": app_id,
        "network_status_allowed": !matches!(egress, crate::netgate::EgressDecision::Blocked),
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
    resolve_identity_bind_for_contract(app_id, workspace_id, no_services, None)
}

/// Validate a signed service requirement and use the canonical identity
/// handshake. Profile metadata remains owned by the current identity broker.
///
/// # Errors
///
/// Returns [`ServiceError`] if serializing or sending the bounded handshake fails.
/// A missing service or refused connection returns `Ok(None)`.
pub fn resolve_identity_bind_for_contract(
    app_id: &str,
    workspace_id: Option<&str>,
    no_services: bool,
    requirement: Option<&ServiceReq>,
) -> Result<Option<ResolvedService>, ServiceError> {
    let presentation = requirement.is_some_and(|requirement| {
        requirement.version == runtime_identity::wire::PRESENTATION_SERVICE_VERSION
    });
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
    write_identity_handshake_negotiated(&mut stream, app_id, workspace_id, presentation)
        .map_err(|source| ServiceError::OpenFailed { path, source })?;
    Ok(Some(ResolvedService {
        name: IDENTITY_SERVICE.to_string(),
        host: OwnedFd::from(stream),
        // Normal manifest services start at 17. Keep the automatic service in
        // a reserved slot so it cannot collide as the manifest evolves.
        child_fd: IDENTITY_CHILD_FD,
    }))
}

/// Name the app and room an identity connection signs for, as the first
/// thing on it.
///
/// # Errors
/// The stream refuses the write.
pub fn write_identity_handshake(
    stream: &mut UnixStream,
    app_id: &str,
    workspace_id: &str,
) -> std::io::Result<()> {
    write_identity_handshake_negotiated(stream, app_id, workspace_id, false)
}

fn write_identity_handshake_negotiated(
    stream: &mut UnixStream,
    app_id: &str,
    workspace_id: &str,
    presentation: bool,
) -> std::io::Result<()> {
    let handshake = runtime_identity::wire::PresentationHandshake {
        handshake: runtime_identity::wire::Handshake {
            workspace_id: workspace_id.to_owned(),
            app_id: app_id.to_owned(),
            surface: None,
        },
        presentation_version: if presentation {
            runtime_identity::wire::PRESENTATION_VERSION
        } else {
            0
        },
    };
    let bytes = serde_json::to_vec(&handshake)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    runtime_identity::wire::HANDSHAKE_FRAMING
        .write(stream, &bytes)
        .map_err(std::io::Error::from)?;
    stream.flush()
}

/// The first line the launcher writes on a chooser connection: the app and
/// the room it runs in, read by the shell before anything the app sends.
///
/// # Errors
///
/// Returns an error if the frame cannot be serialized.
pub fn chooser_handshake(app_id: &str, workspace_id: &str) -> serde_json::Result<Vec<u8>> {
    let mut frame = serde_json::to_vec(&serde_json::json!({
        "protocol": CHOOSER_PROTOCOL,
        "app_id": app_id,
        "workspace_id": workspace_id,
    }))?;
    frame.push(b'\n');
    Ok(frame)
}

/// Connect the session shell's chooser service on the app's behalf and name
/// the app and its room before the sandboxed app can write to it.
///
/// A launch outside a room, or a session whose shell serves no chooser,
/// degrades to `Ok(None)`.
///
/// # Errors
///
/// Returns [`ServiceError::OpenFailed`] if the handshake cannot be written.
pub fn resolve_chooser_bind(
    app_id: &str,
    workspace_id: Option<&str>,
    no_services: bool,
) -> Result<Option<ResolvedService>, ServiceError> {
    resolve_chooser_bind_at(
        &socket_for(CHOOSER_SERVICE),
        app_id,
        workspace_id,
        no_services,
    )
}

fn resolve_chooser_bind_at(
    path: &Path,
    app_id: &str,
    workspace_id: Option<&str>,
    no_services: bool,
) -> Result<Option<ResolvedService>, ServiceError> {
    let Some(workspace_id) = workspace_id else {
        return Ok(None);
    };
    if no_services || !path.exists() {
        return Ok(None);
    }
    let mut stream = match UnixStream::connect(path) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "chooser service refused connection");
            return Ok(None);
        }
    };
    chooser_handshake(app_id, workspace_id)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
        .and_then(|frame| stream.write_all(&frame))
        .and_then(|()| stream.flush())
        .map_err(|source| ServiceError::OpenFailed {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(Some(ResolvedService {
        name: CHOOSER_SERVICE.to_string(),
        host: OwnedFd::from(stream),
        child_fd: CHOOSER_CHILD_FD,
    }))
}

/// Where the filesystem daemon serves document media.
pub fn blob_socket() -> PathBuf {
    std::env::var_os(BLOB_SOCKET_ENV)
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from(DEFAULT_BLOB_SOCKET), PathBuf::from)
}

/// Connect the filesystem daemon's document-media service on the app's
/// behalf and fix the connection's scope — this app in this room — before
/// the sandboxed app can write to it. The app therefore never names its own
/// scope, and cannot reach the socket itself.
///
/// A missing daemon degrades to `Ok(None)`: the app keeps document media in
/// memory for the session, as it does outside Tilo.
///
/// # Errors
///
/// Returns [`ServiceError::OpenFailed`] if the handoff cannot be written.
pub fn resolve_blob_bind(
    app_id: &str,
    workspace_id: Option<&str>,
    no_services: bool,
) -> Result<Option<ResolvedService>, ServiceError> {
    resolve_blob_bind_at(&blob_socket(), app_id, workspace_id, no_services)
}

fn blob_scope_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
        && !value.bytes().all(|byte| byte == b'.')
}

fn resolve_blob_bind_at(
    path: &Path,
    app_id: &str,
    workspace_id: Option<&str>,
    no_services: bool,
) -> Result<Option<ResolvedService>, ServiceError> {
    if no_services {
        return Ok(None);
    }
    // The same partition the app's private directory uses, so a launch with
    // no workspace lands in the personal room.
    let room = crate::appdir::room_slug(workspace_id);
    if !blob_scope_component(&room) || !blob_scope_component(app_id) {
        tracing::warn!(app_id, room, "no document media scope for this launch");
        return Ok(None);
    }
    // Say which: a missing socket is the media service not running, a
    // refused lookup is this person not being allowed to reach it.
    match std::fs::symlink_metadata(path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            tracing::warn!(
                path = %path.display(),
                "document media service is not running; the app keeps media in memory"
            );
            return Ok(None);
        }
        Err(error) => {
            tracing::warn!(
                %error,
                path = %path.display(),
                "document media service is out of this person's reach; the app keeps media in memory"
            );
            return Ok(None);
        }
    }
    let mut stream = match UnixStream::connect(path) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "document media service refused connection");
            return Ok(None);
        }
    };
    write_blob_handoff(&mut stream, app_id, &room).map_err(|source| ServiceError::OpenFailed {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(ResolvedService {
        name: BLOB_SERVICE.to_string(),
        host: OwnedFd::from(stream),
        child_fd: BLOB_CHILD_FD,
    }))
}

fn write_blob_handoff(stream: &mut UnixStream, app_id: &str, room: &str) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "protocol": BLOB_HANDOFF_PROTOCOL,
        "principal": "app",
        "room": room,
        "app_id": app_id,
    }))
    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    stream.write_all(&BLOB_HANDOFF_MAGIC)?;
    BLOB_HANDOFF_FRAMING.write(stream, &bytes)?;
    stream.flush()
}

/// Create `$XDG_RUNTIME_DIR/tilo/services/` if it does not exist.
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
/// Returns [`ServiceError::RequiredMissing`] if a required service's socket
/// doesn't exist, or [`ServiceError::OpenFailed`] if a required service's
/// socket exists but refuses the connection.
pub fn resolve_service_binds(
    services: &BTreeMap<String, ServiceReq>,
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
    services: &BTreeMap<String, ServiceReq>,
    no_services: bool,
    context: ServiceLaunchContext<'_>,
) -> Result<Vec<ResolvedService>, ServiceError> {
    resolve_service_binds_inner(services, no_services, Some(context))
}

enum HandoffOutcome {
    Ready,
    SkipOptional,
}

fn handoff_failure(
    required: bool,
    error: ServiceError,
    message: &str,
) -> Result<HandoffOutcome, ServiceError> {
    if required {
        Err(error)
    } else {
        tracing::warn!(%error, "{message}");
        Ok(HandoffOutcome::SkipOptional)
    }
}

fn authenticate_service(
    name: &str,
    requirement: &ServiceReq,
    path: &Path,
    stream: &mut UnixStream,
    context: Option<ServiceLaunchContext<'_>>,
) -> Result<HandoffOutcome, ServiceError> {
    let required = requirement.is_required();
    if name == COLLAB_SERVICE || name == COMPAT_SERVICE {
        let Some((launch, workspace_id)) =
            context.and_then(|launch| launch.workspace_id.map(|workspace| (launch, workspace)))
        else {
            let error = if name == COLLAB_SERVICE {
                ServiceError::CollabContextMissing
            } else {
                ServiceError::CompatContextMissing
            };
            return handoff_failure(required, error, "optional workspace service skipped");
        };
        let result = if name == COLLAB_SERVICE {
            write_collab_executing_handoff(stream, launch.app_id, workspace_id).map_err(|source| {
                ServiceError::CollabHandoffFailed {
                    path: path.to_path_buf(),
                    source,
                }
            })
        } else {
            write_compat_handoff(stream, launch.app_id, workspace_id).map_err(|source| {
                ServiceError::CompatHandoffFailed {
                    path: path.to_path_buf(),
                    source,
                }
            })
        };
        return match result {
            Ok(()) => Ok(HandoffOutcome::Ready),
            Err(error) => handoff_failure(required, error, "optional workspace handoff rejected"),
        };
    }

    if name == MODEL_GATEWAY_SERVICE {
        let Some(launch) = context else {
            let error = ServiceError::OpenFailed {
                path: path.to_path_buf(),
                source: std::io::Error::other("model gateway requires launcher context"),
            };
            return handoff_failure(required, error, "optional model gateway skipped");
        };
        return match write_model_gateway_handoff(stream, launch.app_id) {
            Ok(()) => Ok(HandoffOutcome::Ready),
            Err(source) => handoff_failure(
                required,
                ServiceError::OpenFailed {
                    path: path.to_path_buf(),
                    source,
                },
                "optional model gateway rejected launch handoff",
            ),
        };
    }

    if name == USH_SERVICE {
        let Some(launch) = context else {
            let error = ServiceError::OpenFailed {
                path: path.to_path_buf(),
                source: std::io::Error::other("the ush service requires launcher context"),
            };
            return handoff_failure(required, error, "optional ush service skipped");
        };
        return match write_ush_handoff(stream, launch.app_id) {
            Ok(()) => Ok(HandoffOutcome::Ready),
            Err(source) => handoff_failure(
                required,
                ServiceError::OpenFailed {
                    path: path.to_path_buf(),
                    source,
                },
                "optional ush service rejected launch handoff",
            ),
        };
    }

    if name == APP_HOST_SERVICE {
        let launch = context.ok_or(ServiceError::AppHostUnauthorized)?;
        write_app_host_handoff(stream, launch.app_id).map_err(|source| {
            ServiceError::OpenFailed {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    Ok(HandoffOutcome::Ready)
}

fn write_ush_handoff(stream: &mut UnixStream, app_id: &str) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "protocol": 1,
        "app_id": app_id,
    }))
    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let frame = USH_HANDOFF_FRAMING.encode(&bytes)?;
    stream.write_all(&USH_LAUNCH_MAGIC)?;
    stream.write_all(&frame)?;
    stream.flush()
}

fn write_app_host_handoff(stream: &mut UnixStream, app_id: &str) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "protocol": 1,
        "app_id": app_id,
    }))
    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let frame = APP_HOST_HANDOFF_FRAMING.encode(&bytes)?;
    stream.write_all(&APP_HOST_LAUNCH_MAGIC)?;
    stream.write_all(&frame)?;
    stream.flush()
}

fn resolve_service_binds_inner(
    services: &BTreeMap<String, ServiceReq>,
    no_services: bool,
    context: Option<ServiceLaunchContext<'_>>,
) -> Result<Vec<ResolvedService>, ServiceError> {
    // Start child fds well above stdio (3) — 17, 18, 19… keeps them out of
    // the way of bwrap's own fds (allocated from ~10 upwards).
    let mut next_child_fd: i32 = 17;
    let mut out = Vec::with_capacity(services.len());

    for (name, req) in services {
        if name == SYSTEM_SERVICE || name == IDENTITY_SERVICE {
            continue;
        }
        let path = socket_for(name);

        if name == APP_HOST_SERVICE
            && context.is_none_or(|launch| {
                launch.app_id != STORE_APP_ID && launch.app_id != BROWSER_APP_ID
            })
        {
            return Err(ServiceError::AppHostUnauthorized);
        }

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
                    version: req.version,
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
        if matches!(
            authenticate_service(name, req, &path, &mut stream, context)?,
            HandoffOutcome::SkipOptional
        ) {
            continue;
        }
        // Owned from here, and deliberately left `FD_CLOEXEC`. The fork+dup2
        // never sets CLOEXEC on the descriptor it creates, so the app's copy
        // survives the exec whatever this one says. Clearing the flag here was
        // described as harmless and was not: it handed this descriptor to
        // every process started later in the same launch, including the D-Bus
        // proxy and the egress gate.
        let host = OwnedFd::from(stream);

        while child_fd_reserved(next_child_fd) {
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

/// Name the app and room a collaboration connection is for, as the first
/// thing on it: what the broker attests against the shell's launch record.
///
/// # Errors
/// The stream refuses the write.
pub fn write_collab_handoff(
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
    let frame = COLLAB_LAUNCH_FRAMING.encode(&bytes)?;
    stream.write_all(&COLLAB_LAUNCH_MAGIC)?;
    stream.write_all(&frame)?;
    stream.flush()
}

/// Request exact executing-task custody on the existing collaboration socket.
/// The original launcher handoff remains the sole application/room admission.
/// # Errors
/// The stream refuses the write.
pub fn write_collab_executing_handoff(
    stream: &mut UnixStream,
    app_id: &str,
    workspace_id: &str,
) -> std::io::Result<()> {
    #[cfg(not(target_os = "linux"))]
    return write_collab_handoff(stream, app_id, workspace_id);
    #[cfg(target_os = "linux")]
    {
        let bytes = serde_json::to_vec(&serde_json::json!({
            "protocol": COLLAB_LAUNCH_PROTOCOL_VERSION,
            "app_id": app_id,
            "workspace_id": workspace_id,
            "executing_peer": true,
        }))
        .map_err(std::io::Error::other)?;
        stream.write_all(&COLLAB_LAUNCH_MAGIC)?;
        COLLAB_LAUNCH_FRAMING.write(stream, &bytes)?;
        stream.flush()
    }
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
    COMPAT_HANDOFF_FRAMING.write(stream, &bytes)?;
    stream.flush()
}

fn write_model_gateway_handoff(stream: &mut UnixStream, app_id: &str) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "protocol": 1,
        "app_id": app_id,
    }))
    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    MODEL_GATEWAY_HANDOFF_FRAMING.write(stream, &bytes)?;
    stream.flush()
}

/// Convert resolved services into `ServiceBind`s consumable by the
/// sandbox argv builder.
#[cfg(target_os = "linux")]
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
mod image_interface_tests {
    use super::*;

    /// The image's preinstalled copy of an app is the fallback for an update
    /// the OS cannot serve, so it must always fit the image it ships in:
    /// every app in the repository requires only interfaces the image's
    /// declaration serves, and the declaration names every service a
    /// manifest can be handed.
    #[test]
    fn every_shipped_app_fits_the_interfaces_the_image_declares() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("workspace root above crates/packaging/launcher");
        let provided = package_store::interfaces::read_interfaces(
            &repo.join("os/usr/share/tilo/system-interfaces.toml"),
        )
        .expect("the image's declaration parses")
        .expect("the image declares its interfaces");

        for service in [
            COLLAB_SERVICE,
            IDENTITY_SERVICE,
            SHELL_UI_SERVICE,
            COMPAT_SERVICE,
            SYSTEM_SERVICE,
            MODEL_GATEWAY_SERVICE,
            APP_HOST_SERVICE,
            USH_SERVICE,
            "tools",
            "models",
            "tilo-fs",
        ] {
            assert!(
                !provided.versions(service).is_empty(),
                "the image does not declare the {service} interface"
            );
        }

        let mut checked = 0;
        for entry in std::fs::read_dir(repo.join("apps")).expect("apps/ readable") {
            let path = entry.expect("dir entry").path().join("tilo-app.toml");
            let Ok(raw) = std::fs::read_to_string(&path) else {
                continue;
            };
            let manifest = tilo_app_package::Manifest::from_toml_str(&raw)
                .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
            let unmet = provided.unmet(&manifest.required_interfaces());
            assert!(
                unmet.is_empty(),
                "{} requires {}",
                path.display(),
                tilo_app_package::interfaces::describe_unmet(&unmet)
            );
            checked += 1;
        }
        assert!(checked > 40, "only {checked} app manifests found");

        let settings = tilo_app_package::Manifest::from_toml_str(
            &std::fs::read_to_string(repo.join("apps/settings/tilo-app.toml")).unwrap(),
        )
        .unwrap();
        assert!(
            settings
                .required_interface_wire()
                .iter()
                .any(|requirement| requirement.starts_with("system@")),
            "Settings names the system interface it was built against"
        );
    }
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
    fn identity_handshake_uses_only_current_canonical_bound_subjects() {
        let (mut writer, mut reader) = UnixStream::pair().unwrap();
        write_identity_handshake(&mut writer, "so.tilo.notes", "room-7").unwrap();
        drop(writer);
        let bytes = runtime_identity::wire::HANDSHAKE_FRAMING
            .read(&mut reader)
            .unwrap()
            .unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "workspace_id": "room-7", "app_id": "so.tilo.notes"
            })
        );
        let actual: runtime_identity::wire::Handshake = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(actual.workspace_id, "room-7");
        assert_eq!(actual.app_id, "so.tilo.notes");
    }

    #[test]
    fn identity_presentation_negotiation_preserves_legacy_default_bytes() {
        for enabled in [false, true] {
            let (mut writer, mut reader) = UnixStream::pair().unwrap();
            write_identity_handshake_negotiated(&mut writer, "so.tilo.notes", "room-7", enabled)
                .unwrap();
            drop(writer);
            let bytes = runtime_identity::wire::HANDSHAKE_FRAMING
                .read(&mut reader)
                .unwrap()
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            if enabled {
                assert_eq!(
                    value["presentation_version"],
                    runtime_identity::wire::PRESENTATION_VERSION
                );
                assert_eq!(value.as_object().unwrap().len(), 3);
            } else {
                assert_eq!(
                    bytes,
                    serde_json::to_vec(&runtime_identity::wire::Handshake {
                        workspace_id: "room-7".into(),
                        app_id: "so.tilo.notes".into(),
                        surface: None,
                    })
                    .unwrap()
                );
                assert_eq!(value.as_object().unwrap().len(), 2);
            }
        }
    }

    #[test]
    fn malformed_identity_contract_is_rejected_before_service_resolution() {
        assert!(ServiceReq::parse(IDENTITY_SERVICE, "required@not-a-version").is_err());
    }

    /// Services are found under `$XDG_RUNTIME_DIR`; point it at `dir` for
    /// as long as the returned scope lives.
    fn runtime_dir_env(dir: &std::path::Path) -> tilo_test_env::ScopedEnv {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", dir);
        env
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn service_handoffs_bind_the_launch_context() {
        // Phase `identity_handoff_is_bound_to_launcher_app_and_workspace_context`.
        {
            use std::io::Read;

            let dir = tempfile::tempdir().expect("runtime");
            let _env = runtime_dir_env(dir.path());
            let root = ensure_services_root().expect("services root");
            let path = root.join("identity.sock");
            let listener = std::os::unix::net::UnixListener::bind(&path).expect("listen");
            let reader = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut bytes = Vec::new();
                stream.read_to_end(&mut bytes).expect("handshake");
                // Little-endian length of the 50-byte JSON handshake.
                assert_eq!(bytes[..4], [50, 0, 0, 0]);
                serde_json::from_slice::<serde_json::Value>(&bytes[4..]).expect("json")
            });
            let bind = resolve_identity_bind("so.tilo.notes", Some("room-7"), false)
                .expect("resolve")
                .expect("identity bind");
            assert_eq!(bind.name, IDENTITY_SERVICE);
            assert_eq!(bind.child_fd, 31);
            drop(bind);
            let handshake = reader.join().expect("reader");
            assert_eq!(handshake["app_id"], "so.tilo.notes");
            assert_eq!(handshake["workspace_id"], "room-7");
        }
        // Phase `handoff_wire_bytes_are_pinned`.
        // Each service's handoff bytes. The services on the other end parse
        // them independently, so the magic, prefix byte order and length must not
        // drift. JSON key order is not pinned (`serde_json`'s `preserve_order` is
        // feature-unified), only the value.
        {
            let fields = serde_json::json!({
                "protocol": 1,
                "app_id": "so.tilo.notes",
                "workspace_id": "room-7",
            });
            let bytes = written(|stream| write_collab_handoff(stream, "so.tilo.notes", "room-7"));
            assert_eq!(bytes[..8], *b"QCL1\x3f\0\0\0");
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes[8..]).unwrap(),
                fields
            );

            let bytes = written(|stream| write_compat_handoff(stream, "so.tilo.notes", "room-7"));
            assert_eq!(bytes[..4], [63, 0, 0, 0]);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes[4..]).unwrap(),
                fields
            );

            let app_only = serde_json::json!({"protocol": 1, "app_id": "so.tilo.notes"});
            let bytes = written(|stream| write_model_gateway_handoff(stream, "so.tilo.notes"));
            assert_eq!(bytes[..4], [39, 0, 0, 0]);
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes[4..]).unwrap(),
                app_only
            );

            let bytes = written(|stream| write_ush_handoff(stream, "so.tilo.notes"));
            assert_eq!(bytes[..8], *b"QUS1\x27\0\0\0");
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes[8..]).unwrap(),
                app_only
            );

            let bytes = written(|stream| write_app_host_handoff(stream, "so.tilo.notes"));
            assert_eq!(bytes[..8], *b"QAH1\0\0\0\x27");
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes[8..]).unwrap(),
                app_only
            );
        }
        // Phase `blob_handoff_fixes_the_app_and_room_before_the_app_holds_the_descriptor`.
        {
            use std::io::Read;

            let dir = tempfile::tempdir().expect("socket dir");
            let path = dir.path().join("fs-blobs.sock");
            let listener = std::os::unix::net::UnixListener::bind(&path).expect("listen");
            let reader = std::thread::spawn(move || {
                let mut frames = Vec::new();
                for _ in 0..2 {
                    let (mut stream, _) = listener.accept().expect("accept");
                    let mut magic = [0_u8; 4];
                    stream.read_exact(&mut magic).expect("magic");
                    let mut len = [0_u8; 4];
                    stream.read_exact(&mut len).expect("length");
                    let mut bytes = vec![0_u8; u32::from_le_bytes(len) as usize];
                    stream.read_exact(&mut bytes).expect("payload");
                    frames.push((
                        magic,
                        serde_json::from_slice::<serde_json::Value>(&bytes).expect("json"),
                    ));
                }
                frames
            });
            let room = resolve_blob_bind_at(&path, "so.tilo.notes", Some("room-7"), false)
                .expect("resolve")
                .expect("blob bind");
            assert_eq!(room.name, BLOB_SERVICE);
            assert_eq!(room.child_fd, BLOB_CHILD_FD);
            let personal = resolve_blob_bind_at(&path, "so.tilo.notes", None, false)
                .expect("resolve")
                .expect("personal blob bind");
            let frames = reader.join().expect("reader");
            drop((room, personal));
            assert_eq!(frames[0].0, BLOB_HANDOFF_MAGIC);
            assert_eq!(
                frames[0].1,
                serde_json::json!({
                    "protocol": 1,
                    "principal": "app",
                    "room": "room-7",
                    "app_id": "so.tilo.notes",
                })
            );
            assert_eq!(frames[1].1["room"], crate::appdir::PERSONAL_ROOM);

            assert!(
                resolve_blob_bind_at(
                    &dir.path().join("absent.sock"),
                    "so.tilo.notes",
                    None,
                    false
                )
                .expect("absent daemon degrades")
                .is_none()
            );
            assert!(
                resolve_blob_bind_at(&path, "so.tilo.notes", None, true)
                    .expect("no services")
                    .is_none()
            );
            assert!(
                resolve_blob_bind_at(&path, "../escape", None, false)
                    .expect("invalid app id degrades")
                    .is_none()
            );
        }
        // Phase `chooser_handshake_names_the_app_and_room_before_the_app_holds_the_descriptor`.
        {
            use std::io::BufRead;

            let dir = tempfile::tempdir().expect("socket dir");
            let path = dir.path().join("chooser.sock");
            let listener = std::os::unix::net::UnixListener::bind(&path).expect("listen");
            let reader = std::thread::spawn(move || {
                let (stream, _) = listener.accept().expect("accept");
                let mut line = String::new();
                std::io::BufReader::new(stream)
                    .read_line(&mut line)
                    .expect("handshake");
                line
            });
            let bind = resolve_chooser_bind_at(&path, "so.tilo.sheets", Some("room-7"), false)
                .expect("resolve")
                .expect("chooser bind");
            assert_eq!(bind.name, CHOOSER_SERVICE);
            assert_eq!(bind.child_fd, CHOOSER_CHILD_FD);
            assert_eq!(
                crate::sandbox::svc_env_var(&bind.name),
                "TILO_SVC_CHOOSER_FD"
            );
            let line = reader.join().expect("reader");
            assert!(line.ends_with('\n'));
            assert_eq!(
                serde_json::from_str::<serde_json::Value>(&line).expect("json"),
                serde_json::json!({
                    "protocol": 1,
                    "app_id": "so.tilo.sheets",
                    "workspace_id": "room-7",
                })
            );

            assert!(
                resolve_chooser_bind_at(&path, "so.tilo.sheets", None, false)
                    .expect("no room")
                    .is_none(),
                "the shell cannot scope a launch outside a room"
            );
            assert!(
                resolve_chooser_bind_at(&path, "so.tilo.sheets", Some("room-7"), true)
                    .expect("no services")
                    .is_none()
            );
            assert!(
                resolve_chooser_bind_at(
                    &dir.path().join("absent.sock"),
                    "so.tilo.sheets",
                    Some("room-7"),
                    false
                )
                .expect("an absent chooser service gives no connection")
                .is_none()
            );
        }
        // Phase `collab_handoff_is_framed_for_broker_authentication`.
        {
            use std::io::Read;

            let dir = tempfile::tempdir().expect("runtime");
            let _env = runtime_dir_env(dir.path());
            let root = ensure_services_root().expect("services root");
            let listener =
                std::os::unix::net::UnixListener::bind(root.join("collab.sock")).expect("listen");
            let reader = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().expect("accept");
                let mut magic = [0_u8; COLLAB_LAUNCH_MAGIC.len()];
                stream.read_exact(&mut magic).expect("magic");
                let bytes = COLLAB_LAUNCH_FRAMING
                    .read(&mut stream)
                    .expect("handoff")
                    .expect("frame");
                (
                    magic,
                    serde_json::from_slice::<serde_json::Value>(&bytes).expect("json"),
                )
            });
            let mut services = BTreeMap::new();
            services.insert(
                COLLAB_SERVICE.to_string(),
                crate::manifest::service_req("required@1"),
            );
            let resolved = resolve_service_binds_for_launch(
                &services,
                false,
                ServiceLaunchContext {
                    app_id: "so.tilo.notes",
                    workspace_id: Some("room-7"),
                },
            )
            .expect("resolve");
            drop(resolved);
            let (magic, handoff) = reader.join().expect("reader");
            assert_eq!(magic, COLLAB_LAUNCH_MAGIC);
            assert_eq!(handoff["protocol"], COLLAB_LAUNCH_PROTOCOL_VERSION);
            assert_eq!(handoff["app_id"], "so.tilo.notes");
            assert_eq!(handoff["workspace_id"], "room-7");
        }
        // Phase `shell_ui_connection_is_authenticated_before_child_handoff`.
        {
            use std::io::{BufRead, BufReader};

            let dir = tempfile::tempdir().unwrap();
            let _env = runtime_dir_env(dir.path());
            let root = ensure_services_root().unwrap();
            let listener =
                std::os::unix::net::UnixListener::bind(root.join("shell-ui.sock")).unwrap();
            let mut services = BTreeMap::new();
            services.insert(
                SHELL_UI_SERVICE.to_string(),
                crate::manifest::service_req("optional@1"),
            );
            for (mode, hosts, allowed) in [
                ("off", Vec::new(), false),
                ("host", Vec::new(), true),
                ("allowlist", vec!["example.com".to_owned()], true),
                ("allowlist", Vec::new(), false),
            ] {
                let egress = crate::netgate::EgressDecision::resolve(mode, &hosts, &[]);
                let resolved = resolve_service_binds(&services, false).unwrap();
                authenticate_shell_ui_bind(&resolved, "so.tilo.chat", &egress).unwrap();

                let (stream, _) = listener.accept().unwrap();
                let mut line = String::new();
                BufReader::new(stream).read_line(&mut line).unwrap();
                let handshake: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
                assert_eq!(handshake["protocol"], SHELL_UI_LAUNCH_PROTOCOL);
                assert_eq!(handshake["app_id"], "so.tilo.chat");
                assert_eq!(
                    handshake["network_status_allowed"], allowed,
                    "{mode}: {hosts:?}"
                );
            }
        }
    }

    fn written(write: impl FnOnce(&mut UnixStream) -> std::io::Result<()>) -> Vec<u8> {
        use std::io::Read;
        let (mut launcher, mut service) = UnixStream::pair().expect("pair");
        write(&mut launcher).expect("handoff");
        drop(launcher);
        let mut bytes = Vec::new();
        service.read_to_end(&mut bytes).expect("read");
        bytes
    }

    #[test]
    fn declared_services_cannot_replace_selected_parent_view_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let _env = runtime_dir_env(dir.path());
        let root = ensure_services_root().unwrap();
        let mut services = BTreeMap::new();
        let mut listeners = Vec::new();
        for index in 0..48 {
            let name = format!("selected-parent-{index:02}");
            listeners.push(
                std::os::unix::net::UnixListener::bind(root.join(format!("{name}.sock"))).unwrap(),
            );
            let requirement = ServiceReq::parse(&name, "required@1").unwrap();
            services.insert(name, requirement);
        }
        let resolved = resolve_service_binds(&services, false).unwrap();
        assert_eq!(resolved.len(), listeners.len());
        for &reserved in RESERVED_CHILD_FDS {
            assert!(resolved.iter().any(|service| service.child_fd > reserved));
            assert!(resolved.iter().all(|service| service.child_fd != reserved));
        }
        // Assert the Linux view slots directly as well, so accidentally omitting
        // one from the reservation table cannot make the test pass.
        #[cfg(target_os = "linux")]
        {
            assert!(
                resolved
                    .iter()
                    .any(|service| { service.child_fd > crate::retained_view::SELECTED_PARENT_FD })
            );
            assert!(
                resolved.iter().all(|service| {
                    service.child_fd != crate::retained_view::SELECTED_PARENT_FD
                })
            );
            for reserved in [
                crate::retained_view::ALIAS_VIEW_FD,
                crate::retained_view::ALIAS_ARGS_FD,
                crate::owned_service::RETAINED_SELECTED_REPLACEMENT_PERMIT_FD,
                crate::owned_service::RETAINED_NATIVE_HISTORY_VERIFIER_FD,
            ] {
                assert!(resolved.iter().any(|service| service.child_fd > reserved));
                assert!(resolved.iter().all(|service| service.child_fd != reserved));
            }
        }
    }

    #[test]
    fn service_requirements_resolve_or_fail_as_declared() {
        // Phase `app_host_capability_is_not_self_grantable_by_third_party_packages`.
        {
            let mut services = BTreeMap::new();
            services.insert(
                APP_HOST_SERVICE.to_string(),
                crate::manifest::service_req("required@1"),
            );
            let error = resolve_service_binds_for_launch(
                &services,
                false,
                ServiceLaunchContext {
                    app_id: "org.example.untrusted",
                    workspace_id: None,
                },
            )
            .expect_err("third-party manifest must not grant package-manager authority");
            assert!(matches!(error, ServiceError::AppHostUnauthorized));
        }
        // Phase `optional_service_whose_socket_refuses_is_skipped_not_fatal`.
        {
            let dir = tempfile::tempdir().unwrap();
            let _env = runtime_dir_env(dir.path());
            let root = ensure_services_root().unwrap();
            dead_socket(&root, "collab");

            let mut svcs = BTreeMap::new();
            svcs.insert(
                "collab".to_string(),
                crate::manifest::service_req("optional@1"),
            );
            let out = resolve_service_binds(&svcs, false)
                .expect("a stale optional socket must degrade, not fail the launch");
            assert!(
                out.is_empty(),
                "nothing can be handed to the app for a service that does not answer"
            );
        }
        // Phase `required_service_whose_socket_refuses_is_still_an_error`.
        {
            let dir = tempfile::tempdir().unwrap();
            let _env = runtime_dir_env(dir.path());
            let root = ensure_services_root().unwrap();
            dead_socket(&root, "collab");

            let mut svcs = BTreeMap::new();
            svcs.insert(
                "collab".to_string(),
                crate::manifest::service_req("required@1"),
            );
            let err = resolve_service_binds(&svcs, false)
                .expect_err("a required service that does not answer must fail the launch");
            assert!(
                matches!(err, ServiceError::OpenFailed { .. }),
                "got {err:?}"
            );
        }
        // Phase `a_live_optional_socket_is_handed_over`.
        {
            let dir = tempfile::tempdir().unwrap();
            let _env = runtime_dir_env(dir.path());
            let root = ensure_services_root().unwrap();
            // Kept alive for the duration of the call, so `connect` succeeds.
            let _listener =
                std::os::unix::net::UnixListener::bind(root.join("test-lifecycle.sock")).unwrap();

            let mut svcs = BTreeMap::new();
            svcs.insert(
                "test-lifecycle".to_string(),
                crate::manifest::service_req("optional@1"),
            );
            let out = resolve_service_binds(&svcs, false).expect("a live socket resolves");
            assert_eq!(out.len(), 1, "the one live service must be handed over");
            assert_eq!(out[0].name, "test-lifecycle");
            assert_eq!(
                out[0].child_fd, 17,
                "the app is told to read fd 17, so that is where it must land"
            );
        }
        // Phase `no_services_flag_skips_required_silently`.
        {
            let mut svcs = BTreeMap::new();
            svcs.insert(
                "collab".to_string(),
                crate::manifest::service_req("required@1"),
            );
            let out = resolve_service_binds(&svcs, true).expect("no-services path is infallible");
            assert!(out.is_empty(), "required service must be skipped");
        }
        // Phase `required_missing_returns_required_missing_error`.
        {
            let mut svcs = BTreeMap::new();
            svcs.insert(
                "collab".to_string(),
                crate::manifest::service_req("required@1"),
            );
            // Point XDG_RUNTIME_DIR at a tempdir with nothing in it.
            let dir = tempfile::tempdir().unwrap();
            let _env = runtime_dir_env(dir.path());
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

    /// What `/proc/self/fd/<n>` says a descriptor points at — `socket:[<inode>]`
    /// for a connected socket, unique to that connection. Identifying
    /// descriptors this way rather than counting the process's is the
    /// difference between a check and a race.
    #[cfg(target_os = "linux")]
    fn fd_target(fd: RawFd) -> Option<String> {
        std::fs::read_link(format!("/proc/self/fd/{fd}"))
            .ok()
            .map(|p| p.display().to_string())
    }

    /// Every socket this process still holds a descriptor for. Failing to
    /// read `/proc/self/fd` fails the test — a check that can't see its
    /// subject must not report the subject is fine.
    #[cfg(target_os = "linux")]
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
    #[cfg(target_os = "linux")]
    fn a_finished_launch_leaves_no_descriptor_behind() {
        const LAUNCHES: usize = 8;

        let dir = tempfile::tempdir().unwrap();
        let _env = runtime_dir_env(dir.path());
        let root = ensure_services_root().unwrap();
        let _listener =
            std::os::unix::net::UnixListener::bind(root.join("test-lifecycle.sock")).unwrap();

        let mut svcs = BTreeMap::new();
        svcs.insert(
            "test-lifecycle".to_string(),
            crate::manifest::service_req("optional@1"),
        );

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

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each phase keeps one former test's body verbatim"
    )]
    fn handed_over_descriptors_have_the_right_owners_and_lifetimes() {
        // Phase `the_launchers_end_is_released_when_the_launch_is_over`.
        // Asserted from the *service's* side of the socket: "we called close"
        // isn't the claim, "the far end can tell" is. Taken as a raw fd,
        // nothing owned or closed this descriptor, so every caller held a live
        // connection nothing would ever release.
        {
            use std::io::Read;

            let dir = tempfile::tempdir().unwrap();
            let _env = runtime_dir_env(dir.path());
            let root = ensure_services_root().unwrap();
            let listener =
                std::os::unix::net::UnixListener::bind(root.join("test-lifecycle.sock")).unwrap();

            let mut svcs = BTreeMap::new();
            svcs.insert(
                "test-lifecycle".to_string(),
                crate::manifest::service_req("optional@1"),
            );
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
        // Phase `the_apps_copy_outlives_the_launchers`.
        // The other order, driven explicitly: a fix that released the
        // descriptor too early would close the app's connection at the moment
        // it was handed over, and a test only watching for EOF would call
        // that a pass.
        {
            use std::io::Read;
            use std::os::fd::FromRawFd;

            let dir = tempfile::tempdir().unwrap();
            let _env = runtime_dir_env(dir.path());
            let root = ensure_services_root().unwrap();
            let listener =
                std::os::unix::net::UnixListener::bind(root.join("test-lifecycle.sock")).unwrap();

            let mut svcs = BTreeMap::new();
            svcs.insert(
                "test-lifecycle".to_string(),
                crate::manifest::service_req("optional@1"),
            );
            let resolved = resolve_service_binds(&svcs, false).expect("a live socket resolves");
            let (mut served, _) = listener.accept().expect("the connection has to arrive");

            // What `place_child_fds` makes on the other side of the fork:
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

            // Darwin refuses timeout changes after peer disconnect. Establish the
            // same EOF deadline while the final peer descriptor is still alive.
            served
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            drop(app_copy);
            assert_eq!(
                served.read(&mut buf).expect("the service must see the end"),
                0,
                "every copy is gone and the service still sees a live connection",
            );
        }
        // Phase `the_launchers_copy_is_not_inherited_by_the_next_child`.
        // The launcher's copy must not be inheritable — with CLOEXEC cleared,
        // every process started after the fork (D-Bus proxy, egress gate)
        // would hold a live connection to a session service opened on an
        // app's behalf.
        {
            let dir = tempfile::tempdir().unwrap();
            let _env = runtime_dir_env(dir.path());
            let root = ensure_services_root().unwrap();
            let _listener =
                std::os::unix::net::UnixListener::bind(root.join("test-lifecycle.sock")).unwrap();

            let mut svcs = BTreeMap::new();
            svcs.insert(
                "test-lifecycle".to_string(),
                crate::manifest::service_req("optional@1"),
            );
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
    }
}

/// Bounded nonsecret model selection. Credentials/endpoints remain gateway-owned.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManagedProviderSelection {
    pub profile_id: String,
    pub model_id: String,
    pub protocol: String,
}
impl ManagedProviderSelection {
    fn validate(&self) -> Result<(), String> {
        if [&self.profile_id, &self.model_id].iter().any(|value| {
            value.is_empty() || value.len() > 256 || value.chars().any(char::is_control)
        }) || !matches!(self.protocol.as_str(), "anthropic" | "open_ai_compatible")
        {
            return Err("invalid managed provider selection".into());
        }
        Ok(())
    }
}
/// Exactly one connected identity, collaboration and authenticated model handoff.
/// No ambient service discovery or identity file access occurs on this path.
#[derive(Debug)]
pub struct EnrolledServiceContext {
    binding: crate::managed::ManagedBinding,
    data_home: PathBuf,
    runtime_epoch: String,
    channel_id: String,
    provider: ManagedProviderSelection,
    connected: Option<BTreeMap<String, UnixStream>>,
}
impl EnrolledServiceContext {
    /// Validate one launch's connected services and hand off the model gateway.
    ///
    /// # Errors
    ///
    /// Returns a description for an invalid binding, provider or context label, an unusable
    /// data home or stream, or a failed model gateway handoff.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "public constructor shared with the cloud host; the canonical root replaces the supplied one"
    )]
    pub fn new(
        binding: crate::managed::ManagedBinding,
        data_home: PathBuf,
        runtime_epoch: String,
        channel_id: String,
        provider: ManagedProviderSelection,
        identity: UnixStream,
        collab: UnixStream,
        mut model: UnixStream,
    ) -> Result<Self, String> {
        binding.validate()?;
        provider.validate()?;
        if !data_home.is_absolute()
            || [&runtime_epoch, &channel_id].iter().any(|value| {
                value.is_empty()
                    || value.len() > 128
                    || !value
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"-_.:".contains(&b))
            })
        {
            return Err("invalid managed descriptor context".into());
        }
        let data_home = data_home.canonicalize().map_err(|e| e.to_string())?;
        for stream in [&identity, &collab, &model] {
            stream.peer_addr().map_err(|e| e.to_string())?;
        }
        model
            .set_write_timeout(Some(std::time::Duration::from_secs(5)))
            .map_err(|e| e.to_string())?;
        write_model_gateway_handoff(&mut model, &binding.app).map_err(|e| e.to_string())?;
        model.set_write_timeout(None).map_err(|e| e.to_string())?;
        Ok(Self {
            binding,
            data_home,
            runtime_epoch,
            channel_id,
            provider,
            connected: Some(
                [
                    (IDENTITY_SERVICE.into(), identity),
                    (COLLAB_SERVICE.into(), collab),
                    (MODEL_GATEWAY_SERVICE.into(), model),
                ]
                .into_iter()
                .collect(),
            ),
        })
    }
    pub fn binding(&self) -> &crate::managed::ManagedBinding {
        &self.binding
    }
    pub fn data_home(&self) -> &Path {
        &self.data_home
    }
    pub fn runtime_epoch(&self) -> &str {
        &self.runtime_epoch
    }
    pub fn channel_id(&self) -> &str {
        &self.channel_id
    }
    /// The agent's launch context as JSON.
    ///
    /// # Errors
    ///
    /// Returns a description when serialization fails.
    pub fn agent_context_json(&self) -> Result<String, String> {
        serde_json::to_string(&serde_json::json!({ "version": 1, "room": self.binding.room, "agent": self.binding.agent, "launch": self.binding.launch, "runtime_epoch": self.runtime_epoch, "channel_id": self.channel_id, "provider": self.provider })).map_err(|e| e.to_string())
    }
    /// Take the connected services exactly once for the matching launch.
    ///
    /// # Errors
    ///
    /// Returns a description for another binding, services already taken, or a requirement the
    /// connected services do not satisfy.
    pub fn take_for_launch(
        &mut self,
        binding: &crate::managed::ManagedBinding,
        required: &BTreeMap<String, ServiceReq>,
    ) -> Result<Vec<ResolvedService>, String> {
        if binding != &self.binding {
            return Err("managed descriptor binding mismatch".into());
        }
        let connected = self
            .connected
            .as_ref()
            .ok_or("managed descriptors already consumed")?;
        for (name, requirement) in required {
            if requirement.is_required()
                && (requirement.version != 1 || !connected.contains_key(name))
            {
                return Err(format!("required managed service unavailable: {name}"));
            }
        }
        let connected = self
            .connected
            .take()
            .ok_or("managed descriptors already consumed")?;
        connected
            .into_iter()
            .enumerate()
            .map(|(index, (name, stream))| {
                let child_fd = i32::try_from(index)
                    .map_err(|_| "service index overflow")?
                    .checked_add(17)
                    .ok_or("service FD overflow")?;
                Ok(ResolvedService {
                    name,
                    host: stream.into(),
                    child_fd,
                })
            })
            .collect()
    }
}
