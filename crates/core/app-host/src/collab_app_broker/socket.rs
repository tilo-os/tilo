#[allow(
    clippy::wildcard_imports,
    reason = "one part of the broker module: the shared state, its imports and the other parts reach this through the parent glob"
)]
use super::*;

/// How long a peer that began its protocol header has to finish it.
pub(super) const LAUNCH_MARKER_COMPLETION: Duration = Duration::from_millis(300);

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SocketIdentity {
    pub(super) device: u64,
    pub(super) inode: u64,
}

#[cfg(unix)]
pub(super) fn socket_identity(path: &Path) -> std::io::Result<SocketIdentity> {
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "broker socket path is not a Unix socket",
        ));
    }
    Ok(SocketIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

#[cfg(unix)]
pub(super) fn socket_lock_path(path: &Path) -> PathBuf {
    let mut lock = path.as_os_str().to_os_string();
    lock.push(".lock");
    PathBuf::from(lock)
}

#[cfg(unix)]
pub(super) fn acquire_socket_path_lease(path: &Path) -> std::io::Result<advisory_lock::FileLock> {
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(socket_lock_path(path))?;
    if !lock.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "broker socket lease is not a regular file",
        ));
    }
    advisory_lock::FileLock::try_exclusive(lock).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "broker socket path is owned by another broker",
        )
    })
}

#[cfg(unix)]
pub(super) fn remove_socket_if_owned(
    path: &Path,
    identity: SocketIdentity,
) -> std::io::Result<bool> {
    let current = match socket_identity(path) {
        Ok(current) => current,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if current != identity {
        return Ok(false);
    }
    std::fs::remove_file(path)?;
    Ok(true)
}

#[cfg(unix)]
pub(super) fn private_socket_parent(path: &Path) -> std::io::Result<&Path> {
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "broker socket requires a parent directory",
        )
    })?;
    let metadata = std::fs::symlink_metadata(parent)?;
    let effective_uid = rustix::process::geteuid().as_raw();
    if !metadata.file_type().is_dir()
        || metadata.uid() != effective_uid
        || metadata.mode() & 0o077 != 0
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "broker socket parent must be an owner-only directory",
        ));
    }
    Ok(parent)
}

#[cfg(unix)]
pub(super) fn socket_stage_path(path: &Path) -> std::io::Result<PathBuf> {
    let parent = private_socket_parent(path)?;
    let leaf_len = path
        .file_name()
        .map(|leaf| leaf.as_bytes().len())
        .filter(|len| *len > 0)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "broker socket requires a filename",
            )
        })?;
    let token = uuid::Uuid::new_v4().simple().to_string();
    let mut leaf = String::with_capacity(leaf_len);
    if leaf_len > 1 {
        leaf.push('.');
    }
    while leaf.len() < leaf_len {
        let remaining = leaf_len.saturating_sub(leaf.len());
        // The token is ASCII hex, so each char is one byte.
        leaf.extend(token.chars().take(remaining));
    }
    Ok(parent.join(leaf))
}

#[cfg(all(test, unix))]
std::thread_local! {
    pub(super) static FAIL_STAGED_SOCKET_IDENTITY: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(unix)]
pub(super) fn staged_socket_identity(path: &Path) -> std::io::Result<SocketIdentity> {
    #[cfg(test)]
    if FAIL_STAGED_SOCKET_IDENTITY.with(|fail| fail.replace(false)) {
        return Err(std::io::Error::other(
            "injected staged socket identity failure",
        ));
    }
    socket_identity(path)
}

#[cfg(unix)]
pub(super) fn bind_socket_stage(
    path: &Path,
) -> std::io::Result<(std::os::unix::net::UnixListener, SocketStage)> {
    for _ in 0..8 {
        let socket = socket_stage_path(path)?;
        if socket == path {
            continue;
        }
        let mut stage = SocketStage {
            socket,
            identity: None,
            bound: false,
            published: false,
        };
        match tilo_files::native::socket::bind(&stage.socket) {
            Ok(listener) => {
                stage.bound = true;
                stage.identity = Some(staged_socket_identity(&stage.socket)?);
                return Ok((listener, stage));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::AddrInUse | std::io::ErrorKind::AlreadyExists
                ) => {}
            Err(error) => return Err(error),
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "broker socket staging name collision",
    ))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn rename_socket_noreplace(source: &Path, destination: &Path) -> std::io::Result<()> {
    match tilo_files::native::rename_file_noreplace(source, destination) {
        // Once the fallback hard link succeeds, the destination socket is
        // published even if unlinking the staging name then fails.
        Ok(
            tilo_files::native::FileRename::Moved
            | tilo_files::native::FileRename::SourceUnlinkFailed(_),
        ) => Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::EEXIST) => Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "broker socket destination appeared during publication",
        )),
        Err(error) => Err(error),
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub(super) fn rename_socket_noreplace(_: &Path, _: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "atomic no-replace broker socket publication is unsupported",
    ))
}

#[cfg(unix)]
pub(super) struct SocketStage {
    pub(super) socket: PathBuf,
    pub(super) identity: Option<SocketIdentity>,
    pub(super) bound: bool,
    pub(super) published: bool,
}

#[cfg(unix)]
impl Drop for SocketStage {
    fn drop(&mut self) {
        if !self.published
            && let Some(identity) = self.identity
        {
            let _ = tilo_failure::ignored!(
                "the next bind proves a stale socket and unlinks it",
                remove_socket_if_owned(&self.socket, identity)
            );
        } else if !self.published
            && self.bound
            && std::fs::symlink_metadata(&self.socket)
                .is_ok_and(|metadata| metadata.file_type().is_socket())
        {
            let _ = std::fs::remove_file(&self.socket);
        }
    }
}

/// Remove only a proven stale socket, then bind and retain inode ownership.
/// A live socket, symlink, non-socket, or replacement inode is never unlinked.
#[cfg(unix)]
pub(super) fn bind_owned_socket(
    path: &Path,
) -> std::io::Result<(tilo_socket::Listener, SocketOwnership)> {
    private_socket_parent(path)?;
    let lease = acquire_socket_path_lease(path)?;
    match socket_identity(path) {
        Ok(stale_candidate) => match tilo_files::native::socket::connect(path) {
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "broker socket is already accepting connections",
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
                if !remove_socket_if_owned(path, stale_candidate)? {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AddrInUse,
                        "broker socket changed during stale-socket recovery",
                    ));
                }
            }
            Err(error) => return Err(error),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let (listener, mut stage) = bind_socket_stage(path)?;
    let identity = stage.identity.ok_or_else(|| {
        std::io::Error::other("bound broker socket stage has no recorded identity")
    })?;
    rename_socket_noreplace(&stage.socket, path)?;
    stage.published = true;
    if socket_identity(path)? != identity {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            "broker socket pathname changed during publication",
        ));
    }
    drop(stage);
    Ok((
        listener,
        SocketOwnership {
            path: path.to_path_buf(),
            identity,
            _lease: lease,
        },
    ))
}

#[cfg(unix)]
pub(super) struct SocketOwnership {
    pub(super) path: PathBuf,
    pub(super) identity: SocketIdentity,
    pub(super) _lease: advisory_lock::FileLock,
}

#[cfg(unix)]
impl Drop for SocketOwnership {
    fn drop(&mut self) {
        let _ = tilo_failure::ignored!(
            "the next bind proves a stale socket and unlinks it",
            remove_socket_if_owned(&self.path, self.identity)
        );
    }
}

/// On Windows the socket is a named pipe: its first instance refuses a pipe another broker
/// holds, its DACL keeps it to this account, and it leaves nothing behind to reclaim.
#[cfg(windows)]
pub(super) fn bind_owned_socket(
    path: &Path,
) -> std::io::Result<(tilo_socket::Listener, SocketOwnership)> {
    Ok((
        tilo_files::native::private::bind_socket(path)?,
        SocketOwnership,
    ))
}

/// The pipe is the broker's while its listener lives; there is nothing else to hold.
#[cfg(windows)]
pub(super) struct SocketOwnership;

/// Peek at an incoming descriptor without consuming the wire's framing
/// prefix. Raw endpoint diagnostics intentionally omit the launcher marker;
/// only a marker-bearing connection is subject to shell-process attestation.
pub(super) async fn has_collab_launch_marker(
    stream: &mut tilo_socket::tokio::Stream,
) -> Result<bool, BrokerError> {
    Ok(peeked_marker(stream).await? == runtime_portal::launch_handoff::MAGIC)
}

/// The first bytes the peer on `stream` sent, as long as a marker, without consuming them: waited
/// for as long as the peer has not begun, and for `LAUNCH_MARKER_COMPLETION` once it has.
pub(super) async fn peeked_marker(
    stream: &mut tilo_socket::tokio::Stream,
) -> Result<[u8; runtime_portal::launch_handoff::MAGIC.len()], BrokerError> {
    let unreadable = |error: std::io::Error| {
        BrokerError::failed(format!("collaboration socket unreadable: {error}"))
    };
    let mut marker = [0_u8; runtime_portal::launch_handoff::MAGIC.len()];
    let begun = tilo_socket::tokio::peek(stream, marker.get_mut(..1).unwrap_or_default())
        .await
        .map_err(unreadable)?;
    if begun == 0 {
        return Err(BrokerError::disconnected(
            "collaboration peer closed before its protocol header".into(),
        ));
    }
    let read = tokio::time::timeout(
        LAUNCH_MARKER_COMPLETION,
        tilo_socket::tokio::peek(stream, &mut marker),
    )
    .await
    .map_err(|_| {
        BrokerError::failed("collaboration peer sent an incomplete protocol header".into())
    })?
    .map_err(unreadable)?;
    if read < marker.len() {
        return Err(BrokerError::disconnected(
            "collaboration peer closed mid protocol header".into(),
        ));
    }
    Ok(marker)
}

/// Why a connection was refused before its first wire call.
pub(super) struct LaunchRefusal {
    pub(super) error: BrokerError,
    /// What the connection said it was, as far as it got: the process and
    /// the app and workspace its launcher named.
    pub(super) peer: String,
    /// The handoff was read whole, so the app can still be told the reason
    /// on the wire instead of seeing its connection reset.
    pub(super) answerable: bool,
}

/// What authenticating a launcher's handoff established about a connection.
#[derive(Default)]
pub(super) struct AuthenticatedHandoff {
    /// The exact process, where the kernel pins it.
    pub(super) local_media: Option<Arc<app_process::LocalAppConnection>>,
    /// The launch that started it, where its host keeps a ticket for it.
    pub(super) launch: Option<workspace_open::ConnectedLaunch>,
}

pub(super) async fn authenticate_launcher_handoff(
    stream: &mut tilo_socket::tokio::Stream,
    services: &ProcessServices,
) -> Result<AuthenticatedHandoff, LaunchRefusal> {
    let LauncherHandoff {
        handoff,
        pid,
        media_process,
    } = match read_launcher_handoff(stream).await {
        Ok(Some(read)) => read,
        Ok(None) => return Ok(AuthenticatedHandoff::default()),
        Err(error) => {
            return Err(LaunchRefusal {
                error,
                peer: credentialed_peer(stream),
                answerable: false,
            });
        }
    };
    let peer = format!(
        "pid {pid} launching {} in workspace {}",
        handoff.app_id, handoff.workspace_id
    );
    if handoff.protocol != runtime_portal::launch_handoff::PROTOCOL_VERSION
        || handoff.app_id.trim().is_empty()
        || handoff.workspace_id.trim().is_empty()
    {
        return Err(LaunchRefusal {
            error: BrokerError::failed(
                "collaboration launch handoff has an unsupported or empty identity".into(),
            ),
            peer,
            answerable: true,
        });
    }

    // Every host records a launch before it releases the child's
    // launch hold (`runtime_portal::launch_hold`), so the record
    // exists before the child can connect, and one look decides. A direct
    // tilo-launcher command never obtains a record and is refused at once
    // instead of becoming a way around admin/admission checks.
    let launch = services
        .launcher
        .as_ref()
        .map(|service| handoff.authenticated_launch(service.as_ref(), pid))
        .transpose()
        .map_err(|error| LaunchRefusal {
            error: BrokerError::failed(error),
            peer: peer.clone(),
            answerable: !handoff.executing_peer,
        })?
        .flatten();
    if let Some(launch) = launch {
        #[cfg(target_os = "linux")]
        let media_process = if handoff.executing_peer {
            Some(
                process_binding::bind(stream, media_process, services, pid, &handoff)
                    .await
                    .map_err(|error| LaunchRefusal {
                        error,
                        peer: peer.clone(),
                        answerable: false,
                    })?,
            )
        } else {
            media_process
        };
        #[cfg(not(target_os = "linux"))]
        if handoff.executing_peer {
            return Err(LaunchRefusal {
                error: BrokerError::failed("executing ownership requires Linux".into()),
                peer,
                answerable: false,
            });
        }
        let opened = launch
            .launch_ticket
            .map(|ticket| workspace_open::ConnectedLaunch {
                ticket,
                app_id: handoff.app_id.clone(),
                workspace: handoff.workspace_id.clone(),
            });
        return Ok(AuthenticatedHandoff {
            local_media: app_process::LocalAppConnection::from_launch(
                handoff,
                media_process,
                launch,
            ),
            launch: opened,
        });
    }
    let error = if services.launcher.is_none() {
        "collaboration launch cannot be attested: this broker has no launcher service"
    } else {
        "collaboration launch is not one this computer made: this computer recorded no launch \
         of this live process, app or workspace; start it with `tilo-runtime --run-app <app>`, \
         so this computer launches it"
    };
    Err(LaunchRefusal {
        error: BrokerError::failed(error.into()),
        peer,
        answerable: !handoff.executing_peer,
    })
}

/// Tell a refused app why over the wire it speaks: every call it makes is
/// answered with `error`. Dropping the socket instead leaves its first call
/// unanswered, and the app only sees its connection reset. Bounded, so a
/// peer that never hangs up holds nothing for long.
pub(super) async fn answer_refused_launch(stream: tilo_socket::tokio::Stream, error: BrokerError) {
    use tokio::io::AsyncWriteExt as _;
    let reason = error.to_string();
    let (mut reader, mut writer) = tokio::io::split(stream);
    let answer = async {
        while let Ok(Some(call)) = runtime_collab::wire::FRAMING
            .read_value_async::<_, runtime_collab::wire::Call>(&mut reader)
            .await
        {
            let frame = runtime_collab::wire::HostFrame::Reply {
                id: call.id,
                result: Err(reason.clone()),
            };
            if runtime_collab::wire::FRAMING
                .write_value_async(&mut writer, &frame)
                .await
                .is_err()
                || writer.flush().await.is_err()
            {
                return;
            }
        }
    };
    let _ = tokio::time::timeout(REFUSED_LAUNCH_ANSWER_WINDOW, answer).await;
}

/// How long a refused app's connection is kept open to hear why.
pub(super) const REFUSED_LAUNCH_ANSWER_WINDOW: Duration = Duration::from_secs(5);
