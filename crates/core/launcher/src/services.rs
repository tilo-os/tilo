//! Service-socket discovery + `SCM_RIGHTS` handoff.
//!
//! Each declared service maps to a Unix socket at
//! `$XDG_RUNTIME_DIR/tilo/services/<name>.sock`.

/// Native startup protocol selector; it conveys no authority.
pub use runtime_portal::process_binding::ENV as COLLAB_EXECUTING_PEER_ENV;
#[cfg(target_os = "linux")]
pub use runtime_portal::process_binding::ExecutingPeer;

use std::collections::BTreeMap;
use std::io::Write;
#[cfg(unix)]
use std::os::fd::{AsRawFd as _, RawFd};

use runtime_portal::packet::{Descriptor, descriptor_of};
use std::path::{Path, PathBuf};
use tilo_socket::Stream;

use wire_frame::Framing;

use crate::manifest::ServiceReq;

/// Exit code for "required service missing" — mirrors `EX_SOFTWARE` (70) from sysexits.h.
pub const EXIT_REQUIRED_SERVICE_MISSING: i32 = 70;

/// Manifest name of the CRDT fan-out service (`[sandbox.services] collab`).
pub const COLLAB_SERVICE: &str = "collab";
/// The daemon's native endpoint: action frames, one JSON line each. The launcher connects it from
/// its own process before the sandbox starts, so the daemon reads the launch it recorded from the
/// connection's peer and admits it as the app it started, in its workspace, and as nothing else.
pub const NATIVE_SERVICE: &str = "native";
pub const IDENTITY_SERVICE: &str = "identity";
pub const SHELL_UI_SERVICE: &str = "shell-ui";
/// Version of the launcher-written shell-UI authentication envelope.
pub const SHELL_UI_LAUNCH_PROTOCOL: u16 = 1;
pub use runtime_portal::session_services::{
    CHOOSER as CHOOSER_SERVICE, CLIPBOARD as CLIPBOARD_SERVICE, MESSAGING as MESSAGING_SERVICE,
    MODEL_GATEWAY as MODEL_GATEWAY_SERVICE, chooser_handshake,
};
pub const COMPAT_SERVICE: &str = "compat";
/// The system interface: the image-owned grant through which the device's control panel reaches the
/// system services it drives ([`crate::system_authority`]).
pub const SYSTEM_SERVICE: &str = "system";
/// The connections an account's sign-in makes to the account's own servers, made by the session
/// for Settings, whose sandbox has no network (`tilo_account_probe::connect`).
pub const ACCOUNTS_SERVICE: &str = "accounts";
/// The shell runtime's supervisor.
pub const USH_SERVICE: &str = "ush";
/// Prefix of the `ush` handoff; the supervisor reads it before any request.
pub const USH_LAUNCH_MAGIC: [u8; 4] = *b"QUS1";
/// After [`USH_LAUNCH_MAGIC`]: a little-endian `u32` length, then
/// `{"protocol":1,"app_id":"…","workspace":"…"}`.
const USH_HANDOFF_FRAMING: Framing = Framing::little_endian(1024).reject_empty();

/// Document media.
pub const BLOB_SERVICE: &str = "blobs";
/// The filesystem daemon's document-media socket (`runtime_fs::blobs`).
pub const DEFAULT_BLOB_SOCKET: &str = "/run/tilo/fs-blobs.sock";
/// Overrides [`DEFAULT_BLOB_SOCKET`] for sessions whose daemon runs elsewhere.
pub const BLOB_SOCKET_ENV: &str = "TILO_FS_BLOB_SOCKET";
/// Prefix of the handoff that fixes the connection's scope before the app holds the descriptor; the
/// daemon reads it before any filesystem request.
pub const BLOB_HANDOFF_MAGIC: [u8; 4] = *b"QBL1";
const BLOB_HANDOFF_PROTOCOL: u16 = 1;
const BLOB_HANDOFF_FRAMING: Framing = Framing::little_endian(4096).reject_empty();
/// In-sandbox descriptor slot of the automatic document-media service.
const BLOB_CHILD_FD: i32 = 30;
/// Slot of the automatic identity service.
const IDENTITY_CHILD_FD: i32 = 31;
/// Slot of the automatic chooser service.
const CHOOSER_CHILD_FD: i32 = 32;
/// Slot of the automatic clipboard service.
const CLIPBOARD_CHILD_FD: i32 = 38;
/// Where the shell-UI connection every window gets lands in the sandbox.
const SHELL_UI_CHILD_FD: i32 = 35;
/// Slot of the automatic render broker connection (`tilo_render_protocol::broker`).
const RENDER_CHILD_FD: i32 = 34;

/// The services an app of this login reaches by name: every name the launcher hands it a socket
/// for, and the two the protocol crates name beside it. A client that dials a service dials one of
/// these, so the session that serves them has a row for each — a name here that nothing serves is a
/// launch that fails.
pub const HANDED_SERVICES: &[&str] = &[
    COLLAB_SERVICE,
    NATIVE_SERVICE,
    IDENTITY_SERVICE,
    SHELL_UI_SERVICE,
    CHOOSER_SERVICE,
    CLIPBOARD_SERVICE,
    COMPAT_SERVICE,
    SYSTEM_SERVICE,
    MODEL_GATEWAY_SERVICE,
    MESSAGING_SERVICE,
    USH_SERVICE,
    BLOB_SERVICE,
    // The render broker is the Linux desktop's: its socket, transport and handshake live in
    // `tilo_render_protocol::broker`, which only compiles there. A host without it (the
    // per-user macOS runtime) hands no render service, and naming one nothing serves would
    // fail every launch that dials it.
    #[cfg(target_os = "linux")]
    tilo_render_protocol::broker::SERVICE,
    tilo_system_stats::SERVICE,
];
/// Slots bwrap reads its two seccomp programs from (`--add-seccomp-fd`).
pub(crate) const CLONE3_COMPAT_SECCOMP_CHILD_FD: i32 = 40;
pub(crate) const BASELINE_SECCOMP_CHILD_FD: i32 = 41;
/// Synthetic sandbox account file, consumed by bwrap before app execution.
pub(crate) const ACCOUNT_CHILD_FD: i32 = 42;
/// Bwrap private-service namespace information, closed before app execution.
pub(crate) const SERVICE_NAMESPACE_CHILD_FD: i32 = 43;
/// Every fixed child slot; declared services are numbered around them.
pub(crate) const RESERVED_CHILD_FDS: &[i32] = &[
    BLOB_CHILD_FD,
    IDENTITY_CHILD_FD,
    CHOOSER_CHILD_FD,
    CLIPBOARD_CHILD_FD,
    SHELL_UI_CHILD_FD,
    RENDER_CHILD_FD,
    CLONE3_COMPAT_SECCOMP_CHILD_FD,
    BASELINE_SECCOMP_CHILD_FD,
    ACCOUNT_CHILD_FD,
    SERVICE_NAMESPACE_CHILD_FD,
    #[cfg(target_os = "linux")]
    crate::system_stats::CHILD_FD,
    #[cfg(target_os = "linux")]
    runtime_portal::process_link::CHILD_FD,
];

fn child_fd_reserved(fd: i32) -> bool {
    RESERVED_CHILD_FDS.contains(&fd)
}
/// Only Settings adds accounts, so only Settings is handed the connections to their servers.
use tilo_app_id::apps::SETTINGS as SETTINGS_APP_ID;
/// The version of the handoff the launcher writes the compatibility service, apart from the
/// version of the requests the app then sends.
pub const COMPAT_HANDOFF_PROTOCOL: u16 = 1;
/// The compatibility service's handoff: a little-endian `u32` length, then JSON.
pub const COMPAT_HANDOFF_FRAMING: Framing = Framing::little_endian(4 * 1024).reject_empty();
/// The collaboration handoff: what an app says first on the connection to its session's services,
/// before the wire starts.
pub use runtime_portal::launch_handoff::{
    FRAMING as COLLAB_LAUNCH_FRAMING, MAGIC as COLLAB_LAUNCH_MAGIC,
    PROTOCOL_VERSION as COLLAB_LAUNCH_PROTOCOL_VERSION,
};
// The model gateway's handoff: a little-endian `u32` length, then JSON.
#[cfg(test)]
use runtime_portal::session_services::APP_ID_FRAMING as MODEL_GATEWAY_HANDOFF_FRAMING;
use runtime_portal::session_services::write_app_id as write_model_gateway_handoff;

/// One resolved service handoff: the launcher's own end of the connection, plus the in-sandbox fd
/// number the app should find it at.
#[derive(Debug)]
pub struct ResolvedService {
    pub name: String,
    /// The launcher's end, **owned**, closed when the launch is over.
    pub host: Descriptor,
    /// Where the app is told to look for it.
    pub child_fd: i32,
}

impl ResolvedService {
    /// The number to `dup2` onto [`Self::child_fd`] in the child.
    #[cfg(unix)]
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
    /// Only Settings adds accounts, so only Settings is handed the connections to their servers.
    #[error("service `accounts` is restricted to Settings, where accounts are added")]
    AccountsServiceUnauthorized,
}

#[derive(Clone, Copy, Debug)]
pub struct ServiceLaunchContext<'a> {
    pub app_id: &'a str,
    pub workspace_id: Option<&'a str>,
}

/// Discover `$XDG_RUNTIME_DIR/tilo/services/`.
pub fn services_root() -> PathBuf {
    tilo_paths::xdg::services_dir()
}

pub fn socket_for(name: &str) -> PathBuf {
    tilo_paths::xdg::service_socket(name)
}

/// Where the session's CRDT fan-out broker listens.
pub fn collab_socket() -> PathBuf {
    socket_for(COLLAB_SERVICE)
}

pub fn identity_socket() -> PathBuf {
    socket_for(IDENTITY_SERVICE)
}

/// The first line on a shell-UI connection: the app it speaks for, and whether its launch may know
/// the network's state.
pub fn shell_ui_handshake(app_id: &str, network_status_allowed: bool) -> Vec<u8> {
    let mut frame = serde_json::json!({
        "protocol": SHELL_UI_LAUNCH_PROTOCOL,
        "app_id": app_id,
        "network_status_allowed": network_status_allowed,
    })
    .to_string()
    .into_bytes();
    frame.push(b'\n');
    frame
}

/// Connect a window to the session shell, naming its app before the app can write: every window
/// describes its controls, bar and cards there, so people and agents reach them in any app.
pub fn resolve_shell_ui_bind(
    app_id: &str,
    egress: &crate::egress_proxy::EgressDecision,
    no_services: bool,
) -> Result<Option<ResolvedService>, ServiceError> {
    let path = socket_for(SHELL_UI_SERVICE);
    if no_services || !path.exists() {
        return Ok(None);
    }
    let mut stream = match tilo_files::native::socket::connect(&path) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "shell-ui service refused connection");
            return Ok(None);
        }
    };
    let allowed = !matches!(egress, crate::egress_proxy::EgressDecision::Blocked);
    stream
        .write_all(&shell_ui_handshake(app_id, allowed))
        .and_then(|()| stream.flush())
        .map_err(|source| ServiceError::OpenFailed { path, source })?;
    Ok(Some(ResolvedService {
        name: SHELL_UI_SERVICE.to_string(),
        host: descriptor_of(stream),
        child_fd: SHELL_UI_CHILD_FD,
    }))
}

/// Connect the workspace identity service and authenticate the launch context before the sandboxed
/// app can write to the stream.
pub fn resolve_identity_bind(
    app_id: &str,
    workspace_id: Option<&str>,
    no_services: bool,
) -> Result<Option<ResolvedService>, ServiceError> {
    resolve_identity_bind_for_contract(app_id, workspace_id, no_services, None)
}

/// Validate a signed service requirement and use the canonical identity handshake.
pub fn resolve_identity_bind_for_contract(
    app_id: &str,
    workspace_id: Option<&str>,
    no_services: bool,
    requirement: Option<&ServiceReq>,
) -> Result<Option<ResolvedService>, ServiceError> {
    let presentation = requirement.is_some_and(|requirement| {
        requirement.version >= tilo_identity_client::wire::PRESENTATION_SERVICE_VERSION
    });
    let Some(workspace_id) = workspace_id else {
        return Ok(None);
    };
    if no_services {
        return Ok(None);
    }
    let path = identity_socket();
    if !path.exists() {
        tracing::warn!("identity service is unavailable; the app runs without an identity");
        return Ok(None);
    }
    let mut stream = match tilo_files::native::socket::connect(&path) {
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
        host: descriptor_of(stream),
        // Normal manifest services start at 17.
        child_fd: IDENTITY_CHILD_FD,
    }))
}

/// Name the app and workspace an identity connection signs for, as the first thing on it.
pub fn write_identity_handshake(
    stream: &mut Stream,
    app_id: &str,
    workspace_id: &str,
) -> std::io::Result<()> {
    write_identity_handshake_negotiated(stream, app_id, workspace_id, false)
}

fn write_identity_handshake_negotiated(
    stream: &mut Stream,
    app_id: &str,
    workspace_id: &str,
    presentation: bool,
) -> std::io::Result<()> {
    let handshake = tilo_identity_client::wire::PresentationHandshake {
        handshake: tilo_identity_client::wire::Handshake {
            workspace_id: workspace_id.to_owned(),
            app_id: app_id.to_owned(),
            surface: None,
        },
        presentation_version: if presentation {
            tilo_identity_client::wire::PRESENTATION_VERSION
        } else {
            0
        },
    };
    let bytes = serde_json::to_vec(&handshake)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    tilo_identity_client::wire::HANDSHAKE_FRAMING
        .write(stream, &bytes)
        .map_err(std::io::Error::from)?;
    stream.flush()
}

/// Connect the session shell's chooser service on the app's behalf and name the app and its workspace
/// before the sandboxed app can write to it.
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
    let mut stream = match tilo_files::native::socket::connect(path) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "chooser service refused connection");
            return Ok(None);
        }
    };
    runtime_portal::session_services::write_chooser_handshake(&mut stream, app_id, workspace_id)
        .map_err(|source| ServiceError::OpenFailed {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(Some(ResolvedService {
        name: CHOOSER_SERVICE.to_string(),
        host: descriptor_of(stream),
        child_fd: CHOOSER_CHILD_FD,
    }))
}

/// Connect the clipboard on the app's behalf: the one service a Rust app and a tilolang instance
/// both reach their copy and paste through.
///
/// Inside a Tilo session the session's clipboard answers, so a copy is the person's copy on their
/// other devices too. Where no session serves one — the person's own macOS, Windows or Linux
/// desktop, or a dev run outside a session — this process serves the same wire from the operating
/// system's clipboard ([`tilo_clipboard::os`]) on a thread that lives as long as the app. None
/// when the launch runs without services, and the app's copies and pastes are then refused.
pub fn resolve_clipboard_bind(no_services: bool) -> Option<ResolvedService> {
    if no_services {
        return None;
    }
    tilo_clipboard::service::for_app(&socket_for(CLIPBOARD_SERVICE)).map(clipboard_bind)
}

/// The session's clipboard at `path`, never this process's own.
#[cfg(test)]
fn resolve_clipboard_bind_at(path: &Path, no_services: bool) -> Option<ResolvedService> {
    if no_services {
        return None;
    }
    tilo_clipboard::service::connect_session(path).map(clipboard_bind)
}

fn clipboard_bind(stream: Stream) -> ResolvedService {
    ResolvedService {
        name: CLIPBOARD_SERVICE.to_string(),
        host: descriptor_of(stream),
        child_fd: CLIPBOARD_CHILD_FD,
    }
}

/// Connect the session's render broker on the app's behalf and name the app and its workspace before
/// the sandboxed app can write to it: the broker picks the render service that draws the app from this,
/// never from the app (`tilo_render_protocol::broker`).
#[cfg(target_os = "linux")]
pub fn resolve_render_bind(
    app_id: &str,
    workspace_id: Option<&str>,
    no_services: bool,
) -> Result<Option<ResolvedService>, ServiceError> {
    resolve_render_bind_at(
        &socket_for(tilo_render_protocol::broker::SERVICE),
        app_id,
        workspace_id,
        no_services,
    )
}

#[cfg(target_os = "linux")]
fn resolve_render_bind_at(
    path: &Path,
    app_id: &str,
    workspace_id: Option<&str>,
    no_services: bool,
) -> Result<Option<ResolvedService>, ServiceError> {
    if no_services || !path.exists() {
        return Ok(None);
    }
    let mut stream = match tilo_files::native::socket::connect(path) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "render broker refused connection");
            return Ok(None);
        }
    };
    tilo_render_protocol::broker::write_handshake(&mut stream, app_id, workspace_id).map_err(
        |source| ServiceError::OpenFailed {
            path: path.to_path_buf(),
            source,
        },
    )?;
    Ok(Some(ResolvedService {
        name: tilo_render_protocol::broker::SERVICE.to_string(),
        host: descriptor_of(stream),
        child_fd: RENDER_CHILD_FD,
    }))
}

/// Where the filesystem daemon serves document media.
pub fn blob_socket() -> PathBuf {
    std::env::var_os(BLOB_SOCKET_ENV)
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from(DEFAULT_BLOB_SOCKET), PathBuf::from)
}

/// Connect the filesystem daemon's document-media service on the app's behalf and fix the connection's
/// scope — this app in this workspace — before the sandboxed app can write to it.
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
    // The same partition the app's private directory uses, so a launch with no workspace lands in the
    // personal workspace.
    let workspace = crate::appdir::workspace_slug(workspace_id);
    if !blob_scope_component(&workspace) || !blob_scope_component(app_id) {
        tracing::warn!(app_id, workspace, "no document media scope for this launch");
        return Ok(None);
    }
    // Say which: a missing socket is the media service not running, a refused lookup is this person
    // not being allowed to reach it.
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
    let mut stream = match tilo_files::native::socket::connect(path) {
        Ok(stream) => stream,
        Err(error) => {
            tracing::warn!(%error, path = %path.display(), "document media service refused connection");
            return Ok(None);
        }
    };
    write_blob_handoff(&mut stream, app_id, &workspace).map_err(|source| {
        ServiceError::OpenFailed {
            path: path.to_path_buf(),
            source,
        }
    })?;
    Ok(Some(ResolvedService {
        name: BLOB_SERVICE.to_string(),
        host: descriptor_of(stream),
        child_fd: BLOB_CHILD_FD,
    }))
}

fn write_blob_handoff(stream: &mut Stream, app_id: &str, workspace: &str) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&serde_json::json!({
        "protocol": BLOB_HANDOFF_PROTOCOL,
        "actor": "app",
        "workspace": workspace,
        "app_id": app_id,
    }))
    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    stream.write_all(&BLOB_HANDOFF_MAGIC)?;
    BLOB_HANDOFF_FRAMING.write(stream, &bytes)?;
    stream.flush()
}

/// Create `$XDG_RUNTIME_DIR/tilo/services/` if it does not exist: it and every directory made
/// for it, `tilo` included, this account's alone from the start, as each service of the account
/// that binds there makes it.
pub fn ensure_services_root() -> std::io::Result<PathBuf> {
    let root = services_root();
    tilo_files::native::private::directory(&root)?;
    Ok(root)
}

/// Walk the manifest's service requirements + open sockets.
pub fn resolve_service_binds(
    services: &BTreeMap<String, ServiceReq>,
    no_services: bool,
) -> Result<Vec<ResolvedService>, ServiceError> {
    resolve_service_binds_inner(services, no_services, None)
}

/// Resolve service descriptors and write the compositor-authenticated compatibility preamble before the
/// descriptor enters the app sandbox.
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
    stream: &mut Stream,
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

    if name == MODEL_GATEWAY_SERVICE || name == MESSAGING_SERVICE {
        let Some(launch) = context else {
            let error = ServiceError::OpenFailed {
                path: path.to_path_buf(),
                source: std::io::Error::other(format!("{name} requires launcher context")),
            };
            return handoff_failure(required, error, "optional app-named service skipped");
        };
        return match write_model_gateway_handoff(stream, launch.app_id) {
            Ok(()) => Ok(HandoffOutcome::Ready),
            Err(source) => handoff_failure(
                required,
                ServiceError::OpenFailed {
                    path: path.to_path_buf(),
                    source,
                },
                "optional app-named service rejected launch handoff",
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
        return match write_ush_handoff(stream, launch.app_id, launch.workspace_id) {
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
    Ok(HandoffOutcome::Ready)
}

fn write_ush_handoff(
    stream: &mut Stream,
    app_id: &str,
    workspace_id: Option<&str>,
) -> std::io::Result<()> {
    let mut handoff = serde_json::Map::new();
    handoff.insert("protocol".into(), 1.into());
    handoff.insert("app_id".into(), app_id.into());
    if let Some(workspace) = workspace_id.filter(|workspace| !workspace.is_empty()) {
        handoff.insert("workspace".into(), workspace.into());
    }
    let bytes = serde_json::to_vec(&handoff)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let frame = USH_HANDOFF_FRAMING.encode(&bytes)?;
    stream.write_all(&USH_LAUNCH_MAGIC)?;
    stream.write_all(&frame)?;
    stream.flush()
}

fn resolve_service_binds_inner(
    services: &BTreeMap<String, ServiceReq>,
    no_services: bool,
    context: Option<ServiceLaunchContext<'_>>,
) -> Result<Vec<ResolvedService>, ServiceError> {
    // Start child fds well above stdio (3) — 17, 18, 19… keeps them out of the way of bwrap's own fds
    // (allocated from ~10 upwards).
    let mut next_child_fd: i32 = 17;
    let mut out = Vec::with_capacity(services.len());

    for (name, req) in services {
        if name == SYSTEM_SERVICE || name == IDENTITY_SERVICE || name == SHELL_UI_SERVICE {
            continue;
        }
        let path = socket_for(name);

        if name == ACCOUNTS_SERVICE && context.is_none_or(|launch| launch.app_id != SETTINGS_APP_ID)
        {
            return Err(ServiceError::AccountsServiceUnauthorized);
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

        // A socket file that exists is not the same as a service that answers.
        let mut stream = match tilo_files::native::socket::connect(&path) {
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
        // Owned from here, and deliberately left `FD_CLOEXEC`.
        let host = descriptor_of(stream);

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

/// Name the app and workspace a collaboration connection is for, as the first thing on it: what the
/// broker attests against the session's launch record.
pub use runtime_portal::launch_handoff::{
    write as write_collab_handoff, write_executing as write_collab_executing_handoff,
    write_window as write_collab_window_handoff,
};

fn write_compat_handoff(
    stream: &mut Stream,
    app_id: &str,
    workspace_id: &str,
) -> std::io::Result<()> {
    let value = serde_json::json!({
        "protocol": COMPAT_HANDOFF_PROTOCOL,
        "app_id": app_id,
        "workspace_id": workspace_id,
    });
    let bytes = serde_json::to_vec(&value)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    COMPAT_HANDOFF_FRAMING.write(stream, &bytes)?;
    stream.flush()
}

/// Convert resolved services into `ServiceBind`s consumable by the sandbox argv builder.
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

    /// The image's preinstalled copy of an app is built with the image, so it must speak exactly the
    /// interface versions the image serves: every service every app in the repository names, at the
    /// image's version, and the declaration serves every service a manifest can be handed.
    #[test]
    fn every_shipped_app_fits_the_interfaces_the_image_declares() {
        let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(3)
            .expect("workspace root above crates/core/launcher");
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
            USH_SERVICE,
            "tools",
            "models",
            "tilo-fs",
        ] {
            assert!(
                provided.version(service).is_some(),
                "the image does not serve the {service} interface"
            );
        }

        // The examples are copied into new apps (`tilolang new --from-example`), so they must fit the
        // image they are scaffolded on as well.
        let mut checked = 0;
        for root in ["apps", "examples/apps"] {
            let mut in_root = 0;
            for entry in std::fs::read_dir(repo.join(root))
                .unwrap_or_else(|e| panic!("{root}/ is readable: {e}"))
            {
                let path = entry.expect("dir entry").path().join("tilo-app.toml");
                let Ok(raw) = std::fs::read_to_string(&path) else {
                    continue;
                };
                let manifest = tilo_app_package::Manifest::from_toml_str(&raw)
                    .unwrap_or_else(|e| panic!("{} does not parse: {e}", path.display()));
                let unmet = provided.unmet(&manifest.interface_versions());
                assert!(
                    unmet.is_empty(),
                    "{} requires {}",
                    path.display(),
                    tilo_app_package::interfaces::describe_unmet(&unmet)
                );
                in_root += 1;
            }
            assert!(in_root > 0, "no app manifests found under {root}/");
            checked += in_root;
        }
        assert!(checked > 50, "only {checked} app manifests found");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use tilo_test_env::SpawnUnderEnvLock as _;

    /// A socket file with nothing listening on it — what a session that exited without unlinking leaves
    /// behind.
    fn dead_socket(root: &std::path::Path, name: &str) -> PathBuf {
        let path = root.join(format!("{name}.sock"));
        drop(std::os::unix::net::UnixListener::bind(&path).expect("bind the fixture socket"));
        assert!(path.exists(), "a stale socket must still be on disk");
        assert!(
            Stream::connect(&path).is_err(),
            "the fixture must actually refuse connections, or it proves nothing"
        );
        path
    }

    tilo_test_env::scenario! {
        fn the_identity_handshake_is_canonical_and_versioned;

        phase identity_handshake_uses_only_current_canonical_bound_subjects;
        {
            let (mut writer, mut reader) = Stream::pair().unwrap();
            write_identity_handshake(&mut writer, "so.tilo.notes", "workspace-7").unwrap();
            drop(writer);
            let bytes = tilo_identity_client::wire::HANDSHAKE_FRAMING
                .read(&mut reader)
                .unwrap()
                .unwrap();
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(
                value,
                serde_json::json!({
                    "workspace_id": "workspace-7", "app_id": "so.tilo.notes"
                })
            );
            let actual: tilo_identity_client::wire::Handshake = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(actual.workspace_id, "workspace-7");
            assert_eq!(actual.app_id, "so.tilo.notes");
        }

        phase identity_presentation_negotiation_preserves_legacy_default_bytes;
        {
            for enabled in [false, true] {
                let (mut writer, mut reader) = Stream::pair().unwrap();
                write_identity_handshake_negotiated(&mut writer, "so.tilo.notes", "workspace-7", enabled)
                    .unwrap();
                drop(writer);
                let bytes = tilo_identity_client::wire::HANDSHAKE_FRAMING
                    .read(&mut reader)
                    .unwrap()
                    .unwrap();
                let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                if enabled {
                    assert_eq!(
                        value["presentation_version"],
                        tilo_identity_client::wire::PRESENTATION_VERSION
                    );
                    assert_eq!(value.as_object().unwrap().len(), 3);
                } else {
                    assert_eq!(
                        bytes,
                        serde_json::to_vec(&tilo_identity_client::wire::Handshake {
                            workspace_id: "workspace-7".into(),
                            app_id: "so.tilo.notes".into(),
                            surface: None,
                        })
                        .unwrap()
                    );
                    assert_eq!(value.as_object().unwrap().len(), 2);
                }
            }
        }

        phase malformed_identity_contract_is_rejected_before_service_resolution;
        {
            assert!(ServiceReq::parse(IDENTITY_SERVICE, "required@not-a-version").is_err());
        }
    }

    /// Services are found under `$XDG_RUNTIME_DIR`; point it at `dir` for as long as the returned scope
    /// lives.
    fn runtime_dir_env(dir: &std::path::Path) -> tilo_test_env::ScopedEnv {
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set("XDG_RUNTIME_DIR", dir);
        env
    }

    fn written(write: impl FnOnce(&mut Stream) -> std::io::Result<()>) -> Vec<u8> {
        use std::io::Read;
        let (mut launcher, mut service) = Stream::pair().expect("pair");
        write(&mut launcher).expect("handoff");
        drop(launcher);
        let mut bytes = Vec::new();
        service.read_to_end(&mut bytes).expect("read");
        bytes
    }

    /// A handshake as a service reads it: `magic`, then a length in `framing`, then that many bytes
    /// of JSON.
    fn handshake(magic: &[u8], framing: Framing, bytes: &[u8]) -> serde_json::Value {
        assert_eq!(&bytes[..magic.len()], magic, "the frame's tag");
        let (prefix, json) = bytes[magic.len()..].split_at(4);
        let prefix: [u8; 4] = prefix.try_into().expect("a length");
        assert_eq!(
            framing.decode_prefix(prefix).expect("the frame's length"),
            json.len(),
            "the length names the payload that follows it"
        );
        serde_json::from_slice(json).expect("the handshake is JSON")
    }

    /// What `/proc/self/fd/<n>` says a descriptor points at — `socket:[<inode>]` for a connected
    /// socket, unique to that connection.
    #[cfg(target_os = "linux")]
    fn fd_target(fd: RawFd) -> Option<String> {
        std::fs::read_link(format!("/proc/self/fd/{fd}"))
            .ok()
            .map(|p| p.display().to_string())
    }

    /// Every socket this process still holds a descriptor for.
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
            // What the spawn does with them: read the numbers for the `pre_exec` dup2 list, fork, and
            // have no further use for this side of the pair.
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

    tilo_test_env::scenario! {
        fn declared_services_are_handed_over_bound_to_the_launch;

        phase service_handoffs_bind_the_launch_context;
        {
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
                    bytes
                });
                let bind = resolve_identity_bind("so.tilo.notes", Some("workspace-7"), false)
                    .expect("resolve")
                    .expect("identity bind");
                assert_eq!(bind.name, IDENTITY_SERVICE);
                assert_eq!(bind.child_fd, 31);
                drop(bind);
                let handshake = handshake(
                    b"",
                    tilo_identity_client::wire::HANDSHAKE_FRAMING,
                    &reader.join().expect("reader"),
                );
                assert_eq!(handshake["app_id"], "so.tilo.notes");
                assert_eq!(handshake["workspace_id"], "workspace-7");
            }
            {
                let fields = serde_json::json!({
                    "protocol": 1,
                    "app_id": "so.tilo.notes",
                    "workspace_id": "workspace-7",
                });
                let bytes = written(|stream| write_collab_handoff(stream, "so.tilo.notes", "workspace-7"));
                assert_eq!(handshake(b"QCL1", COLLAB_LAUNCH_FRAMING, &bytes), fields);

                let bytes = written(|stream| write_compat_handoff(stream, "so.tilo.notes", "workspace-7"));
                assert_eq!(handshake(b"", COMPAT_HANDOFF_FRAMING, &bytes), fields);

                let app_only = serde_json::json!({"protocol": 1, "app_id": "so.tilo.notes"});
                let bytes = written(|stream| write_model_gateway_handoff(stream, "so.tilo.notes"));
                assert_eq!(handshake(b"", MODEL_GATEWAY_HANDOFF_FRAMING, &bytes), app_only);

                let bytes = written(|stream| write_ush_handoff(stream, "so.tilo.notes", None));
                assert_eq!(handshake(b"QUS1", USH_HANDOFF_FRAMING, &bytes), app_only);
                // An empty workspace is no workspace: the old bytes, exactly.
                let empty = written(|stream| write_ush_handoff(stream, "so.tilo.notes", Some("")));
                assert_eq!(empty, bytes);

                let bytes =
                    written(|stream| write_ush_handoff(stream, "so.tilo.notes", Some("workspace-7")));
                assert_eq!(
                    handshake(b"QUS1", USH_HANDOFF_FRAMING, &bytes),
                    serde_json::json!({
                        "protocol": 1,
                        "app_id": "so.tilo.notes",
                        "workspace": "workspace-7",
                    })
                );
            }
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
                let workspace = resolve_blob_bind_at(&path, "so.tilo.notes", Some("workspace-7"), false)
                    .expect("resolve")
                    .expect("blob bind");
                assert_eq!(workspace.name, BLOB_SERVICE);
                assert_eq!(workspace.child_fd, BLOB_CHILD_FD);
                let personal = resolve_blob_bind_at(&path, "so.tilo.notes", None, false)
                    .expect("resolve")
                    .expect("personal blob bind");
                let frames = reader.join().expect("reader");
                drop((workspace, personal));
                assert_eq!(frames[0].0, BLOB_HANDOFF_MAGIC);
                assert_eq!(
                    frames[0].1,
                    serde_json::json!({
                        "protocol": 1,
                        "actor": "app",
                        "workspace": "workspace-7",
                        "app_id": "so.tilo.notes",
                    })
                );
                assert_eq!(frames[1].1["workspace"], crate::appdir::PERSONAL_WORKSPACE);

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
                let bind = resolve_chooser_bind_at(&path, "so.tilo.sheets", Some("workspace-7"), false)
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
                        "workspace_id": "workspace-7",
                    })
                );

                assert!(
                    resolve_chooser_bind_at(&path, "so.tilo.sheets", None, false)
                        .expect("no workspace")
                        .is_none(),
                    "the shell cannot scope a launch outside a workspace"
                );
                assert!(
                    resolve_chooser_bind_at(&path, "so.tilo.sheets", Some("workspace-7"), true)
                        .expect("no services")
                        .is_none()
                );
                assert!(
                    resolve_chooser_bind_at(
                        &dir.path().join("absent.sock"),
                        "so.tilo.sheets",
                        Some("workspace-7"),
                        false
                    )
                    .expect("an absent chooser service gives no connection")
                    .is_none()
                );
            }
            {
                let dir = tempfile::tempdir().expect("socket dir");
                let path = dir.path().join("clipboard.sock");
                assert!(
                    resolve_clipboard_bind_at(&path, false).is_none(),
                    "an absent clipboard service degrades"
                );
                let listener =
                    std::os::unix::net::UnixListener::bind(&path).expect("listen");
                let bind = resolve_clipboard_bind_at(&path, false).expect("clipboard bind");
                assert_eq!(bind.name, CLIPBOARD_SERVICE);
                assert_eq!(bind.child_fd, CLIPBOARD_CHILD_FD);
                assert_eq!(
                    crate::sandbox::svc_env_var(&bind.name),
                    "TILO_SVC_CLIPBOARD_FD"
                );
                assert!(
                    resolve_clipboard_bind_at(&path, true).is_none(),
                    "--no-services refuses the clipboard"
                );
                drop(listener);
            }
            {
                // No Tilo session serves the clipboard here: the launcher serves the same wire
                // from the OS clipboard instead of leaving the app without one.
                let dir = tempfile::tempdir().expect("runtime");
                let _env = runtime_dir_env(dir.path());
                assert!(
                    resolve_clipboard_bind(false).is_some(),
                    "without a session the launcher serves the OS clipboard"
                );
                assert!(
                    resolve_clipboard_bind(true).is_none(),
                    "--no-services still refuses the clipboard"
                );
            }
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
                        workspace_id: Some("workspace-7"),
                    },
                )
                .expect("resolve");
                drop(resolved);
                let (magic, handoff) = reader.join().expect("reader");
                assert_eq!(magic, COLLAB_LAUNCH_MAGIC);
                assert_eq!(handoff["protocol"], COLLAB_LAUNCH_PROTOCOL_VERSION);
                assert_eq!(handoff["app_id"], "so.tilo.notes");
                assert_eq!(handoff["workspace_id"], "workspace-7");
            }
        }

        phase declared_services_cannot_replace_a_reserved_descriptor;
        {
            let dir = tempfile::tempdir().unwrap();
            let _env = runtime_dir_env(dir.path());
            let root = ensure_services_root().unwrap();
            let mut services = BTreeMap::new();
            let mut listeners = Vec::new();
            for index in 0..48 {
                let name = format!("reserved-view-{index:02}");
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
        }

        phase service_requirements_resolve_or_fail_as_declared;
        {
            {
                let mut services = BTreeMap::new();
                services.insert(
                    ACCOUNTS_SERVICE.to_string(),
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
                .expect_err("an app that declares the accounts service is not handed its network");
                assert!(matches!(error, ServiceError::AccountsServiceUnauthorized));
            }
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
            {
                let mut svcs = BTreeMap::new();
                svcs.insert(
                    "collab".to_string(),
                    crate::manifest::service_req("required@1"),
                );
                let out = resolve_service_binds(&svcs, true).expect("no-services path is infallible");
                assert!(out.is_empty(), "required service must be skipped");
            }
            {
                let mut svcs = BTreeMap::new();
                svcs.insert(
                    "collab".to_string(),
                    crate::manifest::service_req("required@1"),
                );
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

        phase handed_over_descriptors_have_the_right_owners_and_lifetimes;
        {
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
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .expect("a bounded wait, so a leak fails the test instead of hanging it");

                // The launch is over: either bwrap was spawned and holds the `dup2` copy, or the spawn
                // failed and there is no child at all.
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
            {
                use std::io::Read;
                use std::os::fd::{FromRawFd, OwnedFd};

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

                // What `place_child_fds` makes on the other side of the fork: another descriptor on the
                // same socket.
                let app_copy = unsafe {
                    let raw = libc::dup(resolved[0].host_fd());
                    assert!(raw >= 0, "dup of the handed-over descriptor failed");
                    OwnedFd::from_raw_fd(raw)
                };
                drop(resolved);

                // A closed connection reads its end at once; an open one with nothing to read would block.
                served.set_nonblocking(true).unwrap();
                let mut buf = [0u8; 1];
                match served.read(&mut buf) {
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                    Ok(0) => panic!(
                        "the app's connection was closed the moment the launcher let go \
                     of its own copy, so the app is handed a descriptor to a socket \
                     nobody is serving"
                    ),
                    other => panic!("unexpected read from a live connection: {other:?}"),
                }

                // Darwin refuses timeout changes after peer disconnect.
                served.set_nonblocking(false).unwrap();
                served
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
                    .unwrap();
                drop(app_copy);
                assert_eq!(
                    served.read(&mut buf).expect("the service must see the end"),
                    0,
                    "every copy is gone and the service still sees a live connection",
                );
            }
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
                    .output_locked()
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
}
