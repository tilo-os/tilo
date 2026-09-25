//! Unix transport/launch edge, shared by macOS and Linux.
use std::{
    fs::{File, OpenOptions},
    io,
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::UnixStream,
            process::CommandExt,
        },
    },
    path::Path,
    process::{Child, Command},
};

pub(super) fn private_directory(path: &Path) -> io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700).create(path)?;
    let metadata = std::fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no arguments or memory preconditions.
    if !metadata.file_type().is_dir() || metadata.uid() != unsafe { libc::geteuid() } {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "runtime directory must be owned by this user and not a symlink",
        ));
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

/// Native automatic startup rejects a pre-existing public profile before any
/// legacy profile initialization can change its permissions.
pub(super) fn check_private_profile(path: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.permissions().mode() & 0o077 != 0 => {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "native runtime profile must be private (mode 0700)",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    private_directory(path)
}

/// The host's service sockets, as one app connects to them.
pub(super) struct ServiceSockets<'a> {
    pub(super) collab: &'a Path,
    pub(super) identity: &'a Path,
    pub(super) blobs: &'a Path,
}

/// Connect the document-media socket with this app's scope fixed before the
/// app holds the descriptor. A host without the service leaves the app on
/// its process-local store.
fn connect_blobs(path: &Path, app_id: &str, room: &str) -> Option<UnixStream> {
    use runtime_fs::blobs::{BlobPrincipal, BlobScope, write_handoff};

    let scope = BlobScope::new(room, app_id).ok()?;
    let mut stream = UnixStream::connect(path).ok()?;
    stream
        .set_write_timeout(Some(std::time::Duration::from_secs(5)))
        .ok()?;
    write_handoff(&mut stream, &BlobPrincipal::App(scope)).ok()?;
    stream.set_write_timeout(None).ok()?;
    Some(stream)
}

/// Own the connected sockets until spawn/exec finishes. Preparation is shared
/// by the manifest launcher and independently attached native apps.
pub(super) struct Connections {
    _collab: UnixStream,
    _identity: UnixStream,
    _blobs: Option<UnixStream>,
}
impl Connections {
    pub(super) fn prepare(
        command: &mut Command,
        app_id: &str,
        room: &str,
        sockets: &ServiceSockets<'_>,
    ) -> io::Result<Self> {
        if app_id.trim().is_empty() || app_id.len() > 4096 || room.len() > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "app identity is empty",
            ));
        }
        let collab = UnixStream::connect(sockets.collab)?;
        let mut identity = UnixStream::connect(sockets.identity)?;
        identity.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
        let handshake = serde_json::to_vec(&runtime_identity::wire::Handshake {
            workspace_id: room.into(),
            app_id: app_id.into(),
        })?;
        runtime_identity::wire::HANDSHAKE_FRAMING.write(&mut identity, &handshake)?;
        identity.set_write_timeout(None)?;
        let blobs = connect_blobs(sockets.blobs, app_id, room);
        let mut fds = vec![collab.as_raw_fd(), identity.as_raw_fd()];
        command.env_remove("TILO_SVC_BLOBS_FD");
        if let Some(blobs) = &blobs {
            fds.push(blobs.as_raw_fd());
            command.env("TILO_SVC_BLOBS_FD", blobs.as_raw_fd().to_string());
        }
        command
            // This prepares a fresh transfer, including managed spawn where
            // the future child PID is not yet known. Never carry an ancestor's
            // automatic-exec ownership or command-resume marker into it.
            .env_remove("QOS_RUNTIME_CONNECTION_OWNER_PID")
            .env_remove("QOS_RUNTIME_RESUME_LIVE")
            .env("TILO_PACKAGE_ID", app_id)
            .env("TILO_WORKSPACE_ID", room)
            .env("TILO_SVC_COLLAB_FD", collab.as_raw_fd().to_string())
            .env("TILO_SVC_IDENTITY_FD", identity.as_raw_fd().to_string());
        // SAFETY: only async-signal-safe fcntl calls run between fork and exec.
        // Both sockets remain owned here until spawn completes. Other descriptors
        // retain CLOEXEC, including endpoints belonging to previously launched apps.
        unsafe {
            command.pre_exec(move || {
                for &fd in &fds {
                    let flags = libc::fcntl(fd, libc::F_GETFD);
                    if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
        Ok(Self {
            _collab: collab,
            _identity: identity,
            _blobs: blobs,
        })
    }
}

pub(super) fn launch(
    mut command: Command,
    app_id: &str,
    room: &str,
    sockets: &ServiceSockets<'_>,
) -> io::Result<Child> {
    let _connections = Connections::prepare(&mut command, app_id, room, sockets)?;
    command.spawn()
}

pub(super) fn private_file(path: &Path) -> io::Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let metadata = file.metadata()?;
    // SAFETY: geteuid has no memory preconditions.
    if !metadata.is_file()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.nlink() != 1
        || metadata.permissions().mode() & 0o077 != 0
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "runtime file must be private, singly linked and owned by this user",
        ));
    }
    Ok(file)
}
