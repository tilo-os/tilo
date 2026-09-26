//! Where the test-control socket lives, and binding it so only this user can
//! connect.
//!
//! The socket drives the shell as if it were the person at the keyboard, so
//! its default is in the user's private runtime directory and the compositor
//! never publishes it in a state another account could connect to. There is
//! no shared-directory fallback: a well-known name under `/tmp` could be
//! bound first by anyone, capturing every command a client sends.

use std::ffi::OsString;
use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};

/// Environment variable naming the socket; the compositor listens only
/// when it is set (and development overrides are compiled in).
pub const TEST_CONTROL_ENV: &str = "TILO_TEST_CONTROL";

/// Directory under `$XDG_RUNTIME_DIR` shared with the other per-session
/// endpoints the compositor publishes.
pub const TEST_CONTROL_RUNTIME_SUBDIR: &str = "tilo-session";

/// File name of the default socket inside [`TEST_CONTROL_RUNTIME_SUBDIR`].
pub const TEST_CONTROL_SOCKET_FILE: &str = "test-control.sock";

/// `$XDG_RUNTIME_DIR/tilo-session/test-control.sock`, or `None` when the
/// runtime directory is unset or not absolute.
#[must_use]
pub fn default_socket_path(runtime_dir: Option<OsString>) -> Option<PathBuf> {
    let dir = PathBuf::from(runtime_dir?);
    dir.is_absolute().then(|| {
        dir.join(TEST_CONTROL_RUNTIME_SUBDIR)
            .join(TEST_CONTROL_SOCKET_FILE)
    })
}

/// Bind a listener at `path` that only this user can connect to.
///
/// Missing parent directories are created 0700. The socket is bound inside a
/// fresh 0700 staging directory next to `path`, restricted to 0600 there, and
/// only then renamed into place, so it is never reachable with wider
/// permissions, even briefly and even when `path` is in a shared directory.
/// A stale socket at `path` is replaced; anything else there is refused.
///
/// # Errors
///
/// Returns the filesystem error, or `AlreadyExists` when `path` names
/// something other than a socket.
pub fn bind_private(path: &Path) -> io::Result<UnixListener> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if path.file_name().is_none() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "socket path has no file name",
        ));
    }
    if !parent.exists() {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)?;
    }
    match std::fs::symlink_metadata(path) {
        Ok(existing) if !existing.file_type().is_socket() => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            ));
        }
        Ok(_) | Err(_) => {}
    }

    // Short on purpose: a Unix socket path is capped near 108 bytes, and the
    // staged path must fit wherever the final one does.
    let staging = parent.join(format!(".t{:08x}", uuid::Uuid::new_v4().as_fields().0));
    std::fs::DirBuilder::new().mode(0o700).create(&staging)?;
    let staged = staging.join("s");
    let bound = UnixListener::bind(&staged).and_then(|listener| {
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&staged, path)?;
        Ok(listener)
    });
    let _ = std::fs::remove_file(&staged);
    let _ = std::fs::remove_dir(&staging);
    bound
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;
    use std::os::unix::net::UnixStream;

    #[test]
    fn control_socket_is_private_replaces_only_stale_sockets() {
        // Phase `default_lives_under_an_absolute_runtime_dir_only`.
        {
            assert_eq!(
                default_socket_path(Some("/run/user/1000".into())),
                Some(PathBuf::from(
                    "/run/user/1000/tilo-session/test-control.sock"
                ))
            );
            assert_eq!(default_socket_path(Some("run/user".into())), None);
            assert_eq!(default_socket_path(None), None);
        }

        // Phase `bound_socket_is_private_and_reachable`.
        {
            let runtime = tempfile::tempdir().unwrap();
            let path = default_socket_path(Some(runtime.path().into())).unwrap();

            let _listener = bind_private(&path).unwrap();

            let dir = std::fs::symlink_metadata(path.parent().unwrap()).unwrap();
            assert_eq!(dir.permissions().mode() & 0o777, 0o700);
            let socket = std::fs::symlink_metadata(&path).unwrap();
            assert!(socket.file_type().is_socket());
            assert_eq!(socket.permissions().mode() & 0o777, 0o600);
            assert_eq!(socket.uid(), unsafe { libc::geteuid() });
            UnixStream::connect(&path).unwrap();
            // The staging directory is gone.
            assert_eq!(
                std::fs::read_dir(path.parent().unwrap()).unwrap().count(),
                1
            );
        }

        // Phase `a_stale_socket_is_replaced`.
        {
            let runtime = tempfile::tempdir().unwrap();
            let path = runtime.path().join("control.sock");
            drop(bind_private(&path).unwrap());
            assert!(path.exists());

            let _listener = bind_private(&path).unwrap();
            UnixStream::connect(&path).unwrap();
        }

        // Phase `a_file_or_link_at_the_path_is_left_alone`.
        {
            let runtime = tempfile::tempdir().unwrap();
            let file = runtime.path().join("notes.txt");
            std::fs::write(&file, "keep").unwrap();
            let link = runtime.path().join("link.sock");
            std::os::unix::fs::symlink(&file, &link).unwrap();

            for path in [&file, &link] {
                let error = bind_private(path).unwrap_err();
                assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
            }
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "keep");
            assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        }
    }
}
