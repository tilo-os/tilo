//! One owner process per resource; every other process connects to it.
//!
//! Some resources admit only one process at a time — a browser profile, a
//! database file — yet several processes want them. A [`Rendezvous`] beside
//! the resource elects the owner with an advisory lock (`<name>.lock`) and
//! lets every other process reach it over a local socket (`<name>.sock`).
//!
//! The owner keeps its [`Ownership`] for as long as it uses the resource; the
//! lock is released when that is dropped, so the next process can take over
//! even when the owner died without cleaning up. A socket left behind by a
//! dead owner is removed by its successor: nobody can be serving it.
//!
//! Local sockets exist on Linux and macOS. In the browser [`Rendezvous::beside`]
//! reports [`std::io::ErrorKind::Unsupported`].

use std::io::{self, Read, Write};
use std::path::Path;
use std::time::{Duration, Instant};

/// The meeting point beside one resource.
pub struct Rendezvous(backend::Rendezvous);

/// The outcome of [`Rendezvous::join`].
pub enum Joined {
    /// No process owns the resource; this one does now.
    Owner(Ownership),
    /// Another process owns the resource; this is the connection to it.
    Peer(Connection),
}

/// The right to use the resource, held until dropped.
pub struct Ownership(backend::Ownership);

/// The owner's end of the socket. [`Listener::close`] (or drop) removes the
/// socket so no process connects to an owner that stopped serving.
pub struct Listener(backend::Listener);

/// A byte stream between the owner and one other process.
pub struct Connection(backend::Connection);

impl Rendezvous {
    /// The rendezvous for `resource`: `<resource>.sock` and `<resource>.lock`
    /// beside it, named after the resource's canonical path.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::InvalidInput`] when the socket's file name alone is
    /// too long for a local socket (a deeper path is reached relative to its
    /// directory), [`io::ErrorKind::Unsupported`] where local sockets do not
    /// exist, and the error resolving `resource`, which must exist.
    pub fn beside(resource: &Path) -> io::Result<Self> {
        backend::Rendezvous::beside(resource).map(Self)
    }

    /// Connects to the owner, or becomes the owner when there is none.
    ///
    /// `greet` checks a new connection before it counts as a peer; an owner
    /// that is shutting down refuses there, and joining tries again. While
    /// another process holds the lock without serving yet (it is starting,
    /// or still releasing the resource), this waits up to `timeout`.
    ///
    /// # Errors
    ///
    /// [`io::ErrorKind::TimedOut`] when the owner neither served nor let go
    /// in time, and the error opening or locking the lock file.
    pub fn join(
        &self,
        timeout: Duration,
        mut greet: impl FnMut(&Connection) -> io::Result<()>,
    ) -> io::Result<Joined> {
        let started = Instant::now();
        loop {
            if let Ok(connection) = self.0.connect().map(Connection)
                && greet(&connection).is_ok()
            {
                return Ok(Joined::Peer(connection));
            }
            if let Some(ownership) = self.0.try_own()? {
                return Ok(Joined::Owner(Ownership(ownership)));
            }
            if started.elapsed() >= timeout {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "the process that owns this resource did not answer",
                ));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Ownership {
    /// Starts serving other processes.
    pub fn listen(&self) -> io::Result<Listener> {
        self.0.listen().map(Listener)
    }
}

impl Listener {
    /// The next process that connected, waiting at most `wait` for one.
    pub fn accept_within(&self, wait: Duration) -> io::Result<Option<Connection>> {
        Ok(self.0.accept_within(wait)?.map(Connection))
    }

    /// Removes the socket, so no other process connects to this owner any
    /// more. Connections already accepted are unaffected.
    pub fn close(&self) {
        self.0.close();
    }
}

impl Connection {
    /// A second handle to the same connection, for a reader and a writer.
    pub fn try_clone(&self) -> io::Result<Self> {
        self.0.try_clone().map(Self)
    }

    /// Ends the connection in both directions, for every handle to it.
    pub fn close(&self) {
        self.0.close();
    }

    /// Bounds how long a read waits; `None` waits indefinitely.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_read_timeout(timeout)
    }
}

impl Read for &Connection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for &Connection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl Read for Connection {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let mut shared: &Self = self;
        shared.read(buf)
    }
}

impl Write for Connection {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut shared: &Self = self;
        shared.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        let mut shared: &Self = self;
        shared.flush()
    }
}

#[cfg(unix)]
mod backend {
    use std::io::{self, Read, Write};
    use std::net::Shutdown;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
    use std::os::unix::net::{SocketAddr, UnixListener, UnixStream};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::Duration;

    pub struct Rendezvous {
        socket: PathBuf,
        address: Arc<Address>,
        lock: PathBuf,
    }

    pub struct Ownership {
        socket: PathBuf,
        address: Arc<Address>,
        _lock: advisory_lock::FileLock,
    }

    /// How the socket is bound and reached. A local socket address holds a
    /// path of about a hundred bytes (104 on macOS, 108 on Linux), and a
    /// resource may lie deeper than that, as a private browser profile in
    /// macOS's per-user temporary directory does. Such a socket is reached
    /// by its name relative to its directory instead of by its full path.
    enum Address {
        Path(PathBuf),
        Beside {
            directory: relative::Directory,
            name: PathBuf,
        },
    }

    impl Address {
        fn new(socket: &Path) -> io::Result<Self> {
            if SocketAddr::from_pathname(socket).is_ok() {
                return Ok(Self::Path(socket.to_owned()));
            }
            let too_long = || {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!(
                        "the socket path {} is too long for a local socket",
                        socket.display()
                    ),
                )
            };
            let (Some(parent), Some(name)) = (socket.parent(), socket.file_name()) else {
                return Err(too_long());
            };
            let name = PathBuf::from(name);
            if SocketAddr::from_pathname(&name).is_err() {
                return Err(too_long());
            }
            Ok(Self::Beside {
                directory: relative::Directory::open(parent)?,
                name,
            })
        }

        fn bind(&self) -> io::Result<UnixListener> {
            match self {
                Self::Path(path) => UnixListener::bind(path),
                Self::Beside { directory, name } => relative::bind(directory, name),
            }
        }

        fn connect(&self) -> io::Result<UnixStream> {
            match self {
                Self::Path(path) => UnixStream::connect(path),
                Self::Beside { directory, name } => relative::connect(directory, name),
            }
        }
    }

    /// Linux names an open directory in a short path, `/proc/self/fd/<n>`,
    /// that the kernel resolves to the directory itself.
    #[cfg(not(target_vendor = "apple"))]
    mod relative {
        use std::io;
        use std::os::fd::{AsRawFd, OwnedFd};
        use std::os::unix::fs::OpenOptionsExt;
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::path::{Path, PathBuf};

        /// The socket's directory, held open.
        pub struct Directory(OwnedFd);

        impl Directory {
            pub fn open(path: &Path) -> io::Result<Self> {
                let directory = std::fs::OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
                    .open(path)?;
                Ok(Self(directory.into()))
            }

            fn through(&self, name: &Path) -> PathBuf {
                PathBuf::from(format!("/proc/self/fd/{}", self.0.as_raw_fd())).join(name)
            }
        }

        pub fn bind(directory: &Directory, name: &Path) -> io::Result<UnixListener> {
            UnixListener::bind(directory.through(name))
        }

        pub fn connect(directory: &Directory, name: &Path) -> io::Result<UnixStream> {
            UnixStream::connect(directory.through(name))
        }
    }

    /// macOS has no such path, and no `bindat` it exports. The socket's
    /// directory is reached through a symbolic link in a fresh private
    /// temporary directory (`mkdtemp`, mode 0700) in `/tmp`, made for the one
    /// call and removed after it: the socket itself is created in, and found
    /// in, its own directory.
    #[cfg(target_vendor = "apple")]
    mod relative {
        use std::ffi::{CString, OsString};
        use std::io;
        use std::os::unix::ffi::OsStringExt;
        use std::os::unix::net::{UnixListener, UnixStream};
        use std::path::{Path, PathBuf};

        /// The socket's directory, by its canonical path.
        pub struct Directory(PathBuf);

        impl Directory {
            pub fn open(path: &Path) -> io::Result<Self> {
                path.canonicalize().map(Self)
            }

            fn through<T>(
                &self,
                name: &Path,
                call: impl FnOnce(PathBuf) -> io::Result<T>,
            ) -> io::Result<T> {
                let template = CString::new("/tmp/tilo-socket-XXXXXX").map_err(io::Error::other)?;
                let mut template = template.into_bytes_with_nul();
                // SAFETY: a writable, NUL-terminated template mkdtemp fills in.
                let made = unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) };
                if made.is_null() {
                    return Err(io::Error::last_os_error());
                }
                template.pop();
                let short = PathBuf::from(OsString::from_vec(template));
                let link = short.join("d");
                let result =
                    std::os::unix::fs::symlink(&self.0, &link).and_then(|()| call(link.join(name)));
                let _ = std::fs::remove_file(&link);
                let _ = std::fs::remove_dir(&short);
                result
            }
        }

        pub fn bind(directory: &Directory, name: &Path) -> io::Result<UnixListener> {
            directory.through(name, UnixListener::bind)
        }

        pub fn connect(directory: &Directory, name: &Path) -> io::Result<UnixStream> {
            directory.through(name, UnixStream::connect)
        }
    }

    pub struct Listener {
        socket: PathBuf,
        identity: (u64, u64),
        bound: UnixListener,
    }

    pub struct Connection(UnixStream);

    fn identity(metadata: &std::fs::Metadata) -> (u64, u64) {
        (metadata.dev(), metadata.ino())
    }

    impl Rendezvous {
        pub fn beside(resource: &Path) -> io::Result<Self> {
            let resource = resource.canonicalize()?;
            let (Some(parent), Some(name)) = (resource.parent(), resource.file_name()) else {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "a resource at the filesystem root has no place beside it",
                ));
            };
            let sibling = |suffix: &str| {
                let mut file = name.to_os_string();
                file.push(suffix);
                parent.join(file)
            };
            let socket = sibling(".sock");
            let address = Arc::new(Address::new(&socket)?);
            Ok(Self {
                socket,
                address,
                lock: sibling(".lock"),
            })
        }

        pub fn connect(&self) -> io::Result<Connection> {
            self.address.connect().map(Connection)
        }

        pub fn try_own(&self) -> io::Result<Option<Ownership>> {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(&self.lock)?;
            let lock = match advisory_lock::FileLock::try_exclusive(file) {
                Ok(lock) => lock,
                Err(std::fs::TryLockError::WouldBlock) => return Ok(None),
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            };
            // The previous owner may have removed the lock file (with the
            // directory it lived in) after this process opened it: a lock on
            // a file that is gone owns nothing.
            let held = lock.metadata()?;
            if !std::fs::metadata(&self.lock).is_ok_and(|path| identity(&path) == identity(&held)) {
                return Ok(None);
            }
            match std::fs::remove_file(&self.socket) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            Ok(Some(Ownership {
                socket: self.socket.clone(),
                address: self.address.clone(),
                _lock: lock,
            }))
        }
    }

    impl Ownership {
        pub fn listen(&self) -> io::Result<Listener> {
            let listener = self.address.bind()?;
            let secured = listener
                .set_nonblocking(true)
                .and_then(|()| {
                    std::fs::set_permissions(&self.socket, std::fs::Permissions::from_mode(0o600))
                })
                .and_then(|()| std::fs::symlink_metadata(&self.socket));
            match secured {
                Ok(metadata) => Ok(Listener {
                    socket: self.socket.clone(),
                    identity: identity(&metadata),
                    bound: listener,
                }),
                Err(error) => {
                    let _ = std::fs::remove_file(&self.socket);
                    Err(error)
                }
            }
        }
    }

    impl Listener {
        pub fn accept_within(&self, wait: Duration) -> io::Result<Option<Connection>> {
            let mut poll = libc::pollfd {
                fd: self.bound.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            let millis = libc::c_int::try_from(wait.as_millis()).unwrap_or(libc::c_int::MAX);
            // SAFETY: one valid pollfd that outlives the call.
            let ready = unsafe { libc::poll(&raw mut poll, 1, millis) };
            if ready < 0 {
                let error = io::Error::last_os_error();
                return if error.kind() == io::ErrorKind::Interrupted {
                    Ok(None)
                } else {
                    Err(error)
                };
            }
            if ready == 0 {
                return Ok(None);
            }
            match self.bound.accept() {
                Ok((stream, _)) => {
                    // macOS hands accepted sockets the listener's flags.
                    stream.set_nonblocking(false)?;
                    Ok(Some(Connection(stream)))
                }
                // The peer went away between poll and accept.
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(None),
                Err(error) => Err(error),
            }
        }

        pub fn close(&self) {
            // Only this owner's socket: a successor may already serve its own.
            if std::fs::symlink_metadata(&self.socket)
                .is_ok_and(|metadata| identity(&metadata) == self.identity)
            {
                let _ = std::fs::remove_file(&self.socket);
            }
        }
    }

    impl Drop for Listener {
        fn drop(&mut self) {
            self.close();
        }
    }

    impl Connection {
        pub fn try_clone(&self) -> io::Result<Self> {
            self.0.try_clone().map(Self)
        }

        pub fn close(&self) {
            let _ = self.0.shutdown(Shutdown::Both);
        }

        pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
            self.0.set_read_timeout(timeout)
        }

        pub fn read(&self, buf: &mut [u8]) -> io::Result<usize> {
            (&self.0).read(buf)
        }

        pub fn write(&self, buf: &[u8]) -> io::Result<usize> {
            (&self.0).write(buf)
        }

        pub fn flush(&self) -> io::Result<()> {
            (&self.0).flush()
        }
    }
}

#[cfg(not(unix))]
mod backend {
    use std::io;
    use std::path::Path;
    use std::time::Duration;

    /// Nothing of these exists where local sockets do not.
    pub enum Never {}

    pub struct Rendezvous(Never);
    pub struct Ownership(Never);
    pub struct Listener(Never);
    pub struct Connection(Never);

    impl Rendezvous {
        pub fn beside(_: &Path) -> io::Result<Self> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "local sockets are not available on this platform",
            ))
        }

        pub fn connect(&self) -> io::Result<Connection> {
            match self.0 {}
        }

        pub fn try_own(&self) -> io::Result<Option<Ownership>> {
            match self.0 {}
        }
    }

    impl Ownership {
        pub fn listen(&self) -> io::Result<Listener> {
            match self.0 {}
        }
    }

    impl Listener {
        pub fn accept_within(&self, _: Duration) -> io::Result<Option<Connection>> {
            match self.0 {}
        }

        pub fn close(&self) {
            match self.0 {}
        }
    }

    impl Connection {
        pub fn try_clone(&self) -> io::Result<Self> {
            match self.0 {}
        }

        pub fn close(&self) {
            match self.0 {}
        }

        pub fn set_read_timeout(&self, _: Option<Duration>) -> io::Result<()> {
            match self.0 {}
        }

        pub fn read(&self, _: &mut [u8]) -> io::Result<usize> {
            match self.0 {}
        }

        pub fn write(&self, _: &[u8]) -> io::Result<usize> {
            match self.0 {}
        }

        pub fn flush(&self) -> io::Result<()> {
            match self.0 {}
        }
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;

    #[allow(
        clippy::unnecessary_wraps,
        reason = "the signature `Rendezvous::join` requires of a greeting"
    )]
    fn greet(_: &Connection) -> io::Result<()> {
        Ok(())
    }

    #[test]
    fn the_first_process_owns_and_the_next_connects() {
        let dir = tempfile::tempdir().unwrap();
        let resource = dir.path().join("profile");
        std::fs::create_dir(&resource).unwrap();
        let rendezvous = Rendezvous::beside(&resource).unwrap();
        let Joined::Owner(owner) = rendezvous.join(tilo_test_env::hang_bound(), greet).unwrap()
        else {
            panic!("nobody owned the resource");
        };
        let listener = owner.listen().unwrap();
        let socket = dir.path().canonicalize().unwrap().join("profile.sock");
        assert!(socket.exists());
        let Joined::Peer(peer) = rendezvous.join(tilo_test_env::hang_bound(), greet).unwrap()
        else {
            panic!("the owner was not reached");
        };
        let accepted = listener
            .accept_within(tilo_test_env::hang_bound())
            .unwrap()
            .unwrap();
        (&peer).write_all(b"hello").unwrap();
        let mut read = [0; 5];
        (&accepted).read_exact(&mut read).unwrap();
        assert_eq!(&read, b"hello");
        assert!(
            listener
                .accept_within(Duration::from_millis(10))
                .unwrap()
                .is_none()
        );
        listener.close();
        assert!(!socket.exists());
        drop(owner);
    }

    #[test]
    fn a_dead_owners_socket_is_replaced_and_a_busy_lock_times_out() {
        let dir = tempfile::tempdir().unwrap();
        let resource = dir.path().join("profile");
        std::fs::create_dir(&resource).unwrap();
        let socket = dir.path().canonicalize().unwrap().join("profile.sock");
        drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
        let rendezvous = Rendezvous::beside(&resource).unwrap();
        let Joined::Owner(owner) = rendezvous.join(tilo_test_env::hang_bound(), greet).unwrap()
        else {
            panic!("a dead owner's socket answered");
        };
        assert!(!socket.exists(), "the stale socket is removed");
        let error = rendezvous
            .join(Duration::from_millis(100), greet)
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        drop(owner);
        assert!(matches!(
            rendezvous.join(tilo_test_env::hang_bound(), greet).unwrap(),
            Joined::Owner(_)
        ));
    }

    #[test]
    fn a_resource_deeper_than_a_socket_address_still_meets() {
        let dir = tempfile::tempdir().unwrap();
        let deep = dir.path().join("d".repeat(60)).join("e".repeat(60));
        std::fs::create_dir_all(&deep).unwrap();
        let resource = deep.join("profile");
        std::fs::create_dir(&resource).unwrap();
        let socket = deep.canonicalize().unwrap().join("profile.sock");
        assert!(std::os::unix::net::SocketAddr::from_pathname(&socket).is_err());
        let rendezvous = Rendezvous::beside(&resource).unwrap();
        let Joined::Owner(owner) = rendezvous.join(tilo_test_env::hang_bound(), greet).unwrap()
        else {
            panic!("nobody owned the resource");
        };
        let listener = owner.listen().unwrap();
        assert!(socket.exists(), "the socket is bound beside the resource");
        let Joined::Peer(peer) = rendezvous.join(tilo_test_env::hang_bound(), greet).unwrap()
        else {
            panic!("the owner was not reached");
        };
        let accepted = listener
            .accept_within(tilo_test_env::hang_bound())
            .unwrap()
            .unwrap();
        (&peer).write_all(b"hello").unwrap();
        let mut read = [0; 5];
        (&accepted).read_exact(&mut read).unwrap();
        assert_eq!(&read, b"hello");
        listener.close();
        assert!(!socket.exists());
        drop(owner);
    }

    #[test]
    fn a_socket_path_too_long_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let resource = dir.path().join("x".repeat(120));
        std::fs::create_dir(&resource).unwrap();
        let error = Rendezvous::beside(&resource).err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
