//! Local (Unix-domain) sockets named by a path of any length. The kernel takes a
//! socket's path in a fixed buffer (`sun_path`: 108 bytes on Linux, 104 on macOS), so a
//! socket in a deep enough directory cannot be bound or reached by its path at all; a
//! path that does not fit is reached through its directory, opened first and named by
//! its descriptor (`/proc/self/fd/<fd>/<name>`), so only the socket's own name has to
//! fit. A path that fits is used as it is. Elsewhere than Linux and Android a path that
//! does not fit is refused with [`io::ErrorKind::InvalidInput`] naming the limit, never
//! silently.
//!
//! Windows names a local service with a named pipe instead (`windows`): the same
//! [`bind`], [`connect`], [`Listener`] and [`Stream`], kept to this account by the
//! pipe's DACL and a check of the peer's token. [`tokio`] is the same for an async
//! caller.
//!
//! The crate is std-only on Unix: `tilo_files::native::socket` re-exports it, and a
//! tool that must stay small to start quickly (the `cargo desktop` launcher) can bind
//! and connect through the same code without taking on a dependency graph.

use std::io;
#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;

#[cfg(windows)]
pub mod account;
#[cfg(feature = "tokio")]
pub mod tokio;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::{Listener, MAX_PATH, SocketAddr, Stream, link};

/// The longest socket path the kernel takes, without its terminating NUL.
#[cfg(any(target_os = "linux", target_os = "android"))]
pub const MAX_PATH: usize = 107;
#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
pub const MAX_PATH: usize = 103;
/// A page has no local socket: no length of a path fits.
#[cfg(not(any(unix, windows)))]
pub const MAX_PATH: usize = 0;

#[cfg(unix)]
pub type Listener = UnixListener;
#[cfg(unix)]
pub type Stream = UnixStream;
#[cfg(unix)]
pub use std::os::unix::net::SocketAddr;
/// A page has no local socket: nothing produces one.
#[cfg(not(any(unix, windows)))]
pub struct Listener(std::convert::Infallible);
#[cfg(not(any(unix, windows)))]
pub struct Stream(std::convert::Infallible);

/// Listens on a new socket at `path`, whatever its length.
pub fn bind(path: &Path) -> io::Result<Listener> {
    #[cfg(unix)]
    {
        reach(path, |short| UnixListener::bind(short))
    }
    #[cfg(windows)]
    {
        windows::bind(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(no_socket(path))
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
        windows::connect(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(no_socket(path))
    }
}

/// Two connected ends, both in this process: one is handed to another process or thread, as a
/// connection made without a listener. A Unix stream pair; on Windows a pipe only this process
/// joined ([`link::pair`]), which reads as a byte stream as a socket does.
///
/// # Errors
/// The system's error making the pair.
pub fn pair() -> io::Result<(Stream, Stream)> {
    #[cfg(unix)]
    {
        UnixStream::pair()
    }
    #[cfg(windows)]
    {
        link::pair()
    }
    #[cfg(not(any(unix, windows)))]
    {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "a page has no local socket",
        ))
    }
}

/// Whether something is bound at `path` now, without connecting to it: a
/// socket file on Unix, a pipe with an instance on Windows. One a run left
/// behind on Unix still reads as bound; [`connect`] says whether it answers.
#[must_use]
pub fn is_bound(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt as _;
        std::fs::symlink_metadata(path).is_ok_and(|found| found.file_type().is_socket())
    }
    #[cfg(windows)]
    {
        windows::is_bound(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        false
    }
}

/// The refusal a page reaches: it has no local socket to reach.
#[cfg(not(any(unix, windows)))]
fn no_socket(_path: &Path) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, "a page has no local socket")
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

#[cfg(test)]
#[expect(
    clippy::mixed_attributes_style,
    reason = "the module is a test module, whose panics the lint rules exempt, that binds a Unix socket"
)]
mod tests {
    #![cfg(unix)]

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
