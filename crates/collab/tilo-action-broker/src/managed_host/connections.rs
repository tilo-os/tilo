//! Actual installed session services; no ambient endpoints are handed to the app.
use super::{ManagedBinding, ManagedProviderSelection, ManagedServiceConnections};
use std::{
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        net::UnixStream,
    },
    path::Path,
    time::Duration,
};

/// The host-side connector for the session's normal identity and model gateway services.
/// Room/app handoffs use the existing launcher framing, not a fixture identity protocol.
pub struct InstalledSessionConnections;
impl ManagedServiceConnections for InstalledSessionConnections {
    fn connect(
        &self,
        binding: &ManagedBinding,
        _provider: &ManagedProviderSelection,
    ) -> Result<(UnixStream, UnixStream), String> {
        binding.validate()?;
        let identity_path = launcher::services::identity_socket();
        let model_path = launcher::services::socket_for(launcher::services::MODEL_GATEWAY_SERVICE);
        let mut identity = session_socket(&identity_path)?;
        let model = session_socket(&model_path)?;
        identity
            .set_write_timeout(Some(Duration::from_secs(5)))
            .map_err(|error| error.to_string())?;
        launcher::services::write_identity_handshake(&mut identity, &binding.app, &binding.room)
            .map_err(|error| error.to_string())?;
        identity
            .set_write_timeout(None)
            .map_err(|error| error.to_string())?;
        // EnrolledServiceContext writes the existing model launch handoff exactly once.
        Ok((identity, model))
    }
}
fn session_socket(path: &Path) -> Result<UnixStream, String> {
    let parent = path.parent().ok_or("session service parent absent")?;
    let session_root = parent.parent().ok_or("session directory absent")?;
    let runtime_root = session_root
        .parent()
        .ok_or("session runtime directory absent")?;
    let runtime = std::fs::symlink_metadata(runtime_root).map_err(|error| error.to_string())?;
    let session = std::fs::symlink_metadata(session_root).map_err(|error| error.to_string())?;
    let directory = std::fs::symlink_metadata(parent).map_err(|error| error.to_string())?;
    let socket = std::fs::symlink_metadata(path).map_err(|error| error.to_string())?;
    let uid = unsafe { libc::geteuid() };
    if !runtime.is_dir()
        || runtime.mode() & 0o077 != 0
        || runtime.uid() != uid
        || !session.is_dir()
        || session.mode() & 0o022 != 0
        || session.uid() != uid
        || !directory.is_dir()
        || directory.mode() & 0o022 != 0
        || directory.uid() != uid
        || !socket.file_type().is_socket()
        || socket.uid() != uid
    {
        return Err("managed service is not in this host's private session directory".into());
    }
    UnixStream::connect(path).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::session_socket;
    use std::os::unix::{fs::PermissionsExt, net::UnixListener};

    #[test]
    fn installed_socket_connects_only_through_host_owned_session_directories() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let session = root.path().join("tilo");
        let services = session.join("services");
        std::fs::create_dir_all(&services).unwrap();
        for directory in [&session, &services] {
            std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = services.join("identity.sock");
        let listener = UnixListener::bind(&path).unwrap();
        let connection = session_socket(&path).unwrap();
        let (accepted, _) = listener.accept().unwrap();
        drop((connection, accepted));
        std::fs::set_permissions(&session, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(session_socket(&path).is_err());
        std::fs::set_permissions(&session, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(session_socket(&path).is_err());
    }

    #[test]
    fn installed_socket_refuses_a_symlinked_session_directory() {
        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let actual = root.path().join("actual");
        std::fs::create_dir_all(actual.join("services")).unwrap();
        let listener = UnixListener::bind(actual.join("services/identity.sock")).unwrap();
        std::os::unix::fs::symlink(&actual, root.path().join("tilo")).unwrap();
        assert!(session_socket(&root.path().join("tilo/services/identity.sock")).is_err());
        drop(listener);
    }
}
