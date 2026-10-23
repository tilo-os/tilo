//! Connecting to a service socket that only this user can have created.
//!
//! A session service publishes its socket below a directory private to the
//! user (the runtime directory, `0700`). Before trusting the endpoint, a
//! client checks that every directory from that root down to the socket, and
//! the socket itself, belongs to it and admits nobody else, so another user
//! cannot have planted it. The connect has a deadline: a stale or saturated
//! endpoint must not stall the caller.

use std::io;
use std::os::unix::fs::{FileTypeExt, MetadataExt};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

/// Connect to `socket`, which lies below `root`, once `root`, every directory
/// between it and `socket`, and `socket` itself are owned by this process's
/// user and closed to group and others. Gives up after `timeout`.
pub fn connect_owner_only(root: &Path, socket: &Path, timeout: Duration) -> io::Result<UnixStream> {
    let invalid = |why| io::Error::new(io::ErrorKind::InvalidInput, why);
    if !root.is_absolute() {
        return Err(invalid("the service root is not absolute"));
    }
    let below = socket
        .strip_prefix(root)
        .map_err(|_| invalid("the service socket is not below its root"))?;
    let parent = below.parent().unwrap_or_else(|| Path::new(""));
    let owner = rustix::process::geteuid().as_raw();
    let private = |path: &Path, is_kind: fn(&std::fs::FileType) -> bool| {
        let metadata = std::fs::symlink_metadata(path)?;
        let exposed = metadata.mode() & 0o077 != 0;
        if !is_kind(&metadata.file_type()) || metadata.uid() != owner || exposed {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{} is not private to this user", path.display()),
            ));
        }
        Ok(())
    };
    private(root, std::fs::FileType::is_dir)?;
    let mut directory = root.to_path_buf();
    for component in parent.components() {
        directory.push(component);
        private(&directory, std::fs::FileType::is_dir)?;
    }
    private(socket, FileTypeExt::is_socket)?;

    let address = socket2::SockAddr::unix(socket)?;
    let stream = socket2::Socket::new(socket2::Domain::UNIX, socket2::Type::STREAM, None)?;
    stream.connect_timeout(&address, timeout)?;
    Ok(stream.into())
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::UnixListener;

    use super::*;

    fn private_dir(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[test]
    fn only_an_owner_only_chain_is_connected() {
        let root = tempfile::tempdir().unwrap();
        private_dir(root.path());
        let services = root.path().join("tilo/services");
        private_dir(&root.path().join("tilo"));
        private_dir(&services);
        let socket = services.join("svc.sock");
        let _listener = UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let timeout = tilo_test_env::hang_bound();

        connect_owner_only(root.path(), &socket, timeout).expect("a private chain connects");

        std::fs::set_permissions(&services, std::fs::Permissions::from_mode(0o755)).unwrap();
        let open = connect_owner_only(root.path(), &socket, timeout).unwrap_err();
        assert_eq!(open.kind(), io::ErrorKind::PermissionDenied);

        let outside = connect_owner_only(&services, Path::new("/elsewhere.sock"), timeout);
        assert_eq!(outside.unwrap_err().kind(), io::ErrorKind::InvalidInput);
        let relative = connect_owner_only(Path::new("run"), Path::new("run/a.sock"), timeout);
        assert_eq!(relative.unwrap_err().kind(), io::ErrorKind::InvalidInput);
    }
}
