//! Local (Unix-domain) sockets named by a path of any length. The kernel takes a
//! socket's path in a fixed buffer (`sun_path`: 108 bytes on Linux, 104 on macOS), so a
//! socket in a deep enough directory cannot be bound or reached by its path at all; a
//! path that does not fit is reached through its directory, opened first and named by
//! its descriptor (`/proc/self/fd/<fd>/<name>`), so only the socket's own name has to
//! fit. A path that fits is used as it is. Elsewhere than Linux and Android a path that
//! does not fit is refused with [`io::ErrorKind::InvalidInput`] naming the limit, never
//! silently.
//!
//! Windows names a local service with a named pipe instead, which is not here yet:
//! [`bind`] and [`connect`] answer [`io::ErrorKind::Unsupported`], and their connection
//! types are never produced. A caller that reaches a service only behind `cfg(unix)`
//! needs no change.
//!
//! The crate is std-only: `tilo_files::native::socket` re-exports it, and a tool that
//! must stay small to start quickly (the `cargo desktop` launcher) can bind and connect
//! through the same code without taking on a dependency graph.

use std::io;
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

/// The longest socket path the kernel takes, without its terminating NUL.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const MAX_PATH: usize = 107;
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
pub const MAX_PATH: usize = 103;
/// A named pipe's name is not a socket path: no length of one fits here yet.
#[cfg(windows)]
pub const MAX_PATH: usize = 0;

/// A listening local socket; on Windows nothing produces one yet.
#[cfg(unix)]
pub type Listener = UnixListener;
/// A connection to a local service; on Windows nothing produces one yet.
#[cfg(windows)]
pub struct Listener(std::convert::Infallible);
#[cfg(unix)]
pub type Stream = UnixStream;
#[cfg(windows)]
pub struct Stream(std::convert::Infallible);

/// The refusal a Windows caller reaches: a local service is a named pipe, and
/// none is wired up yet.
#[cfg(windows)]
fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "local sockets are not supported on Windows yet",
    )
}

/// Listens on a new socket at `path`, whatever its length.
pub fn bind(path: &Path) -> io::Result<Listener> {
    #[cfg(unix)]
    {
        reach(path, |short| UnixListener::bind(short))
    }
    #[cfg(windows)]
    {
        let _ = path;
        Err(unsupported())
    }
}

/// Connects to the socket at `path`, whatever its length.
pub fn connect(path: &Path) -> io::Result<Stream> {
    #[cfg(unix)]
    {
        reach(path, |short| UnixStream::connect(short))
    }
    #[cfg(windows)]
    {
        let _ = path;
        Err(unsupported())
    }
}

/// Calls `act` with a name for `path` that fits the kernel's buffer, valid for as long as `act` runs.
#[cfg(unix)]
fn reach<T>(path: &Path, act: impl FnOnce(&Path) -> io::Result<T>) -> io::Result<T> {
    if path.as_os_str().len() <= MAX_PATH {
        return act(path);
    }
    through_directory(path, act)
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn through_directory<T>(path: &Path, act: impl FnOnce(&Path) -> io::Result<T>) -> io::Result<T> {
    use std::os::fd::AsRawFd as _;

    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(too_long(path));
    };
    let parent = if parent.as_os_str().is_empty() {
        Path::new(".")
    } else {
        parent
    };
    // The directory is held open read-only while the socket call runs, so the
    // kernel resolves `/proc/self/fd/<fd>` to it and only the socket's own name
    // has to fit.
    let directory = std::fs::File::open(parent).map_err(|error| {
        let kind = error.kind();
        io::Error::new(kind, format!("{}: {error}", parent.display()))
    })?;
    let short = Path::new("/proc/self/fd")
        .join(directory.as_raw_fd().to_string())
        .join(name);
    if short.as_os_str().len() > MAX_PATH {
        return Err(too_long(path));
    }
    act(&short)
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn through_directory<T>(path: &Path, _act: impl FnOnce(&Path) -> io::Result<T>) -> io::Result<T> {
    Err(too_long(path))
}

#[cfg(unix)]
fn too_long(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!(
            "{}: a socket's name must be at most {MAX_PATH} bytes",
            path.display()
        ),
    )
}

#[cfg(all(test, unix))]
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

    tilo_test_env::scenario! {
        fn a_socket_binds_and_connects_however_deep_its_directory;

        phase a_socket_in_a_deep_directory_binds_and_answers;
        {
            let (_root, dir) = deep();
            let path = dir.join("files.sock");
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

        phase nothing_bound_in_a_deep_directory_is_not_found;
        {
            let (_root, dir) = deep();
            let error = connect(&dir.join("files.sock")).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::NotFound);
        }

        phase a_short_path_is_used_as_it_is;
        {
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
}
