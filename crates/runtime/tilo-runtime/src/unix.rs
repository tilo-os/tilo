//! Unix transport/launch edge, shared by macOS and Linux.
use std::{
    fs::{File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, RawFd},
        unix::{
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            net::UnixStream,
            process::CommandExt,
        },
    },
    path::Path,
    process::{Child, Command},
};

/// Create `path` as a directory only this user can enter.
/// # Errors
/// Rejects a symlink or a directory owned by someone else.
pub(crate) fn private_directory(path: &Path) -> io::Result<()> {
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
/// # Errors
/// Rejects a profile readable by others, or any [`private_directory`] error.
pub(crate) fn check_private_profile(path: &Path) -> io::Result<()> {
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
pub struct ServiceSockets<'a> {
    pub collab: &'a Path,
    pub identity: &'a Path,
    pub blobs: &'a Path,
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

/// One app's connections to the host's services, handshaken with its
/// identity before the app holds any of them.
pub(crate) struct ServiceConnections {
    collab: UnixStream,
    identity: UnixStream,
    blobs: Option<UnixStream>,
}

impl ServiceConnections {
    /// # Errors
    /// Rejects an empty identity and returns connect or handshake errors.
    pub(crate) fn connect(
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
        Ok(Self {
            collab,
            identity,
            blobs,
        })
    }

    /// The descriptors the app inherits, at these same numbers.
    #[must_use]
    pub(crate) fn fds(&self) -> Vec<RawFd> {
        [
            Some(&self.collab),
            Some(&self.identity),
            self.blobs.as_ref(),
        ]
        .into_iter()
        .flatten()
        .map(AsRawFd::as_raw_fd)
        .collect()
    }

    /// Where the app finds each descriptor, and who it is.
    #[must_use]
    pub(crate) fn env(&self, app_id: &str, room: &str) -> Vec<(&'static str, String)> {
        let mut env = vec![
            ("TILO_PACKAGE_ID", app_id.to_owned()),
            ("TILO_WORKSPACE_ID", room.to_owned()),
            ("TILO_SVC_COLLAB_FD", self.collab.as_raw_fd().to_string()),
            (
                "TILO_SVC_IDENTITY_FD",
                self.identity.as_raw_fd().to_string(),
            ),
        ];
        if let Some(blobs) = &self.blobs {
            env.push(("TILO_SVC_BLOBS_FD", blobs.as_raw_fd().to_string()));
        }
        env
    }
}

/// Own the connected sockets until spawn/exec finishes. Preparation is shared
/// by the manifest launcher and independently attached native apps.
pub(crate) struct Connections {
    _services: ServiceConnections,
}
impl Connections {
    /// # Errors
    /// Rejects an empty identity and returns connect or handshake errors.
    pub(crate) fn prepare(
        command: &mut Command,
        app_id: &str,
        room: &str,
        sockets: &ServiceSockets<'_>,
    ) -> io::Result<Self> {
        let services = ServiceConnections::connect(app_id, room, sockets)?;
        let fds = services.fds();
        command
            .env_remove("TILO_SVC_BLOBS_FD")
            // This prepares a fresh transfer, including managed spawn where
            // the future child PID is not yet known. Never carry an ancestor's
            // automatic-exec ownership or command-resume marker into it.
            .env_remove("TILO_RUNTIME_CONNECTION_OWNER_PID")
            .env_remove("TILO_RUNTIME_RESUME_LIVE")
            .envs(services.env(app_id, room));
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
            _services: services,
        })
    }
}

/// Spawn `command` as `app_id` with its service connections inherited.
/// # Errors
/// Returns [`Connections::prepare`] or spawn errors.
pub fn launch(
    mut command: Command,
    app_id: &str,
    room: &str,
    sockets: &ServiceSockets<'_>,
) -> io::Result<Child> {
    let _connections = Connections::prepare(&mut command, app_id, room, sockets)?;
    command.spawn()
}

/// Open or create a private file, refusing symlinks and shared files.
/// # Errors
/// Rejects a file that is linked, shared or owned by someone else.
pub(crate) fn private_file(path: &Path) -> io::Result<File> {
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

#[cfg(test)]
mod tests {
    #[test]
    fn managed_connection_preparation_clears_ancestor_automatic_metadata() {
        let root = tempfile::tempdir().unwrap();
        let collab_path = root.path().join("collab.sock");
        let identity_path = root.path().join("identity.sock");
        let _collab = std::os::unix::net::UnixListener::bind(&collab_path).unwrap();
        let _identity = std::os::unix::net::UnixListener::bind(&identity_path).unwrap();
        let mut command = std::process::Command::new("/usr/bin/true");
        command
            .env("TILO_RUNTIME_CONNECTION_OWNER_PID", "1")
            .env("TILO_RUNTIME_RESUME_LIVE", "dev.tilo.parent");
        let _connections = super::Connections::prepare(
            &mut command,
            "dev.tilo.child",
            "workspace",
            &super::ServiceSockets {
                collab: &collab_path,
                identity: &identity_path,
                blobs: &root.path().join("blobs.sock"),
            },
        )
        .unwrap();
        for key in [
            "TILO_RUNTIME_CONNECTION_OWNER_PID",
            "TILO_RUNTIME_RESUME_LIVE",
        ] {
            assert_eq!(
                command.get_envs().find(|(name, _)| *name == key).unwrap().1,
                None
            );
        }
    }
}
