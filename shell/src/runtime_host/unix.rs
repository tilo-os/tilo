//! Unix transport/launch edge, shared by macOS and Linux.
use std::{
    io::{self, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, MetadataExt, PermissionsExt},
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

pub(super) fn launch(
    mut command: Command,
    app_id: &str,
    room: &str,
    collab_path: &Path,
    identity_path: &Path,
) -> io::Result<Child> {
    if app_id.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "app identity is empty",
        ));
    }
    let collab = UnixStream::connect(collab_path)?;
    let mut identity = UnixStream::connect(identity_path)?;
    let handshake = serde_json::to_vec(&runtime_identity::wire::Handshake {
        workspace_id: room.into(),
        app_id: app_id.into(),
    })?;
    let length = u32::try_from(handshake.len()).map_err(io::Error::other)?;
    identity.write_all(&length.to_le_bytes())?;
    identity.write_all(&handshake)?;
    let fds = [collab.as_raw_fd(), identity.as_raw_fd()];
    command
        .env("QOS_PACKAGE_ID", app_id)
        .env("QOS_WORKSPACE_ID", room)
        .env("QOS_SVC_COLLAB_FD", collab.as_raw_fd().to_string())
        .env("QOS_SVC_IDENTITY_FD", identity.as_raw_fd().to_string());
    // SAFETY: only async-signal-safe fcntl calls run between fork and exec.
    // Both sockets remain owned here until spawn completes. Other descriptors
    // retain CLOEXEC, including endpoints belonging to previously launched apps.
    unsafe {
        command.pre_exec(move || {
            for fd in fds {
                let flags = libc::fcntl(fd, libc::F_GETFD);
                if flags < 0 || libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
    command.spawn()
}
