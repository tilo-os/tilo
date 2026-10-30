//! Local (Unix-domain) sockets named by a path of any length.
//!
//! The kernel takes a socket's path in a fixed buffer (`sun_path`: 108
//! bytes on Linux, 104 on macOS), so a socket in a deep enough directory
//! cannot be bound or reached by its path at all. Every Tilo socket that
//! lives in a directory it does not choose (a profile, a state root, a
//! worktree) binds and connects here instead: a path that does not fit is
//! reached through its directory, opened first and named by its descriptor
//! (`/proc/self/fd/<fd>/<name>`), so only the socket's own name has to fit.
//!
//! A path that fits is used as it is. Elsewhere than Linux and Android, a
//! path that does not fit is refused with [`io::ErrorKind::InvalidInput`]
//! naming the limit, never silently.

use std::io;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

/// The longest socket path the kernel takes, without its terminating NUL.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const MAX_PATH: usize = 107;
/// The longest socket path the kernel takes, without its terminating NUL.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
pub const MAX_PATH: usize = 103;

/// Listen on a new socket at `path`, whatever its length.
///
/// # Errors
/// What `bind(2)` says (`AddrInUse` where something is already there), or
/// what opening the directory says for a path that does not fit.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    reach(path, |short| UnixListener::bind(short))
}

/// Connect to the socket at `path`, whatever its length.
///
/// # Errors
/// What `connect(2)` says: `NotFound` where nothing is bound there,
/// `ConnectionRefused` where nobody listens any more.
pub fn connect(path: &Path) -> io::Result<UnixStream> {
    reach(path, |short| UnixStream::connect(short))
}

/// Call `act` with a name for `path` that fits the kernel's buffer, valid for
/// as long as `act` runs.
fn reach<T>(path: &Path, act: impl FnOnce(&Path) -> io::Result<T>) -> io::Result<T> {
    if path.as_os_str().len() <= MAX_PATH {
        return act(path);
    }
    through_directory(path, act)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn through_directory<T>(path: &Path, act: impl FnOnce(&Path) -> io::Result<T>) -> io::Result<T> {
    use rustix::fs::{CWD, Mode, OFlags};
    use std::os::fd::AsRawFd as _;

    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(too_long(path));
    };
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    let directory = rustix::fs::openat(
        CWD,
        parent,
        OFlags::PATH | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    )
    .map_err(|error| {
        io::Error::new(
            io::Error::from(error).kind(),
            format!("{}: {error}", parent.display()),
        )
    })?;
    let short = Path::new("/proc/self/fd")
        .join(directory.as_raw_fd().to_string())
        .join(name);
    if short.as_os_str().len() > MAX_PATH {
        return Err(too_long(path));
    }
    act(&short)
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
fn through_directory<T>(path: &Path, _act: impl FnOnce(&Path) -> io::Result<T>) -> io::Result<T> {
    Err(too_long(path))
}

fn too_long(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "{}: a socket's name must be at most {MAX_PATH} bytes",
            path.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    /// A directory whose sockets' paths are longer than the kernel takes.
    fn deep() -> (tempfile::TempDir, std::path::PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("a".repeat(60)).join("b".repeat(60));
        std::fs::create_dir_all(&dir).unwrap();
        (root, dir)
    }

    #[test]
    fn a_socket_in_a_deep_directory_binds_and_answers() {
        let (_root, dir) = deep();
        let path = dir.join("door.sock");
        assert!(path.as_os_str().len() > MAX_PATH);
        let listener = bind(&path).unwrap();
        assert!(path.exists(), "the socket is named where it was asked for");
        let mut client = connect(&path).unwrap();
        client.write_all(b"hi").unwrap();
        let (mut served, _) = listener.accept().unwrap();
        let mut got = [0; 2];
        served.read_exact(&mut got).unwrap();
        assert_eq!(&got, b"hi");
    }

    #[test]
    fn nothing_bound_in_a_deep_directory_is_not_found() {
        let (_root, dir) = deep();
        let error = connect(&dir.join("door.sock")).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn a_short_path_is_used_as_it_is() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("s");
        let listener = bind(&path).unwrap();
        assert_eq!(
            listener.local_addr().unwrap().as_pathname(),
            Some(path.as_path())
        );
        connect(&path).unwrap();
    }
}
