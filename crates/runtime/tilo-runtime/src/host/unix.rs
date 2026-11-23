//! Unix transport/launch edge, shared by macOS and Linux.
use std::{
    fs::File,
    io,
    os::{
        fd::{AsRawFd, RawFd},
        unix::{fs::PermissionsExt, net::UnixStream, process::CommandExt},
    },
    path::Path,
    process::{Child, Command},
};

/// Native automatic startup rejects a pre-existing public profile before any
/// legacy profile initialization can change its permissions.
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
    tilo_files::native::private::directory(path)
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
fn connect_blobs(path: &Path, app_id: &str, workspace: &str) -> Option<UnixStream> {
    use runtime_fs::blobs::{BlobActor, BlobScope, write_handoff};

    let scope = BlobScope::new(workspace, app_id).ok()?;
    let mut stream = tilo_files::native::socket::connect(path).ok()?;
    stream
        .set_write_timeout(Some(std::time::Duration::from_secs(5)))
        .ok()?;
    write_handoff(&mut stream, &BlobActor::App(scope)).ok()?;
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

/// The surface file this process was started with ([`ui::app_surface::SURFACE_FILE_ENV`]),
/// when it hashes to [`ui::app_surface::SURFACE_HASH_ENV`]: what `tilo-app dev`
/// names for an app under development.
fn started_surface() -> Option<String> {
    let path = std::env::var_os(ui::app_surface::SURFACE_FILE_ENV)?;
    let hash = std::env::var(ui::app_surface::SURFACE_HASH_ENV).ok()?;
    let expected = ui::app_surface::parse_hash(&hash)?;
    let surface = std::fs::read_to_string(path).ok()?;
    (*blake3::hash(surface.as_bytes()).as_bytes() == expected).then_some(surface)
}

impl ServiceConnections {
    pub(crate) fn connect(
        app_id: &str,
        workspace: &str,
        sockets: &ServiceSockets<'_>,
    ) -> io::Result<Self> {
        if app_id.trim().is_empty() || app_id.len() > 4096 || workspace.len() > 4096 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "app identity is empty",
            ));
        }
        let collab = tilo_files::native::socket::connect(sockets.collab)?;
        let mut identity = tilo_files::native::socket::connect(sockets.identity)?;
        identity.set_write_timeout(Some(std::time::Duration::from_secs(5)))?;
        let handshake = serde_json::to_vec(&tilo_identity_client::wire::Handshake {
            workspace_id: workspace.into(),
            app_id: app_id.into(),
            surface: started_surface(),
        })?;
        tilo_identity_client::wire::HANDSHAKE_FRAMING.write(&mut identity, &handshake)?;
        identity.set_write_timeout(None)?;
        let blobs = connect_blobs(sockets.blobs, app_id, workspace);
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
    pub(crate) fn env(&self, app_id: &str, workspace: &str) -> Vec<(&'static str, String)> {
        let mut env = vec![
            ("TILO_PACKAGE_ID", app_id.to_owned()),
            ("TILO_WORKSPACE_ID", workspace.to_owned()),
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
    pub(crate) fn prepare(
        command: &mut Command,
        app_id: &str,
        workspace: &str,
        sockets: &ServiceSockets<'_>,
    ) -> io::Result<Self> {
        let services = ServiceConnections::connect(app_id, workspace, sockets)?;
        let fds = services.fds();
        command
            .env_remove("TILO_SVC_BLOBS_FD")
            // Independently attached connections use their existing RPC route,
            // not the manifest launcher's executing-task challenge protocol.
            .env_remove(launcher::services::COLLAB_EXECUTING_PEER_ENV)
            // This prepares a fresh transfer, including managed spawn where
            // the future child PID is not yet known. Never carry an ancestor's
            // automatic-exec ownership or command-resume marker into it.
            .env_remove("TILO_RUNTIME_CONNECTION_OWNER_PID")
            .env_remove("TILO_RUNTIME_RESUME_LIVE")
            .envs(services.env(app_id, workspace));
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
/// Returns `Connections::prepare` or spawn errors.
pub fn launch(
    mut command: Command,
    app_id: &str,
    workspace: &str,
    sockets: &ServiceSockets<'_>,
) -> io::Result<Child> {
    let _connections = Connections::prepare(&mut command, app_id, workspace, sockets)?;
    command.spawn()
}

/// Open or create a private file, refusing symlinks, other links and another user's files.
pub(crate) fn private_file(path: &Path) -> io::Result<File> {
    // One this user owns that others may read (an earlier Tilo made its files so) is made
    // private first: nobody but its owner and the superuser could have written it.
    if let Ok(metadata) = std::fs::symlink_metadata(path)
        && metadata.is_file()
        && tilo_files::native::private::owned_by_me(&metadata)
        && metadata.permissions().mode() & 0o077 != 0
    {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    tilo_files::native::private::lock_file(path)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_runtime_file_an_earlier_tilo_left_readable_is_made_private() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let lock = dir.path().join("holder.lock");
        std::fs::write(&lock, b"").unwrap();
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o644)).unwrap();
        super::private_file(&lock).expect("this user's own file is taken");
        let mode = std::fs::metadata(&lock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let other = dir.path().join("other.lock");
        std::fs::hard_link(&lock, &other).unwrap();
        let refused = super::private_file(&lock).expect_err("a file with another link");
        assert!(refused.to_string().contains("singly linked"), "{refused}");
    }

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
            .env("TILO_RUNTIME_RESUME_LIVE", "dev.tilo.parent")
            .env(launcher::services::COLLAB_EXECUTING_PEER_ENV, "1");
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
            launcher::services::COLLAB_EXECUTING_PEER_ENV,
        ] {
            assert_eq!(
                command.get_envs().find(|(name, _)| *name == key).unwrap().1,
                None
            );
        }
    }
}
