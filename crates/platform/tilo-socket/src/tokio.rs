//! The same local sockets for tokio: a Unix socket, or on Windows the same
//! named pipe with the same account checks, behind one listener and stream.

#[cfg(unix)]
use std::{io, path::Path};

#[cfg(unix)]
pub use tokio::net::{UnixListener as Listener, UnixStream as Stream};

/// Serve `listener`, bound with [`crate::bind`] (or a caller's own private
/// bind), on the current tokio runtime.
///
/// # Errors
/// The listener could not be registered with the runtime.
#[cfg(unix)]
pub fn listener(listener: crate::Listener) -> io::Result<Listener> {
    listener.set_nonblocking(true)?;
    Listener::from_std(listener)
}

/// Connect to the socket at `path`, whatever its length.
///
/// # Errors
/// As [`crate::connect`], or the stream could not be registered with the runtime.
#[cfg(unix)]
#[expect(
    clippy::unused_async,
    reason = "one signature on every host: Windows' connect waits out a busy pipe"
)]
pub async fn connect(path: &Path) -> io::Result<Stream> {
    let stream = crate::connect(path)?;
    stream.set_nonblocking(true)?;
    Stream::from_std(stream)
}

/// Drive `stream`, connected by [`crate::connect`] or handed over, on the current tokio runtime.
///
/// # Errors
/// The stream could not be registered with the runtime.
#[cfg(unix)]
pub fn from_std(stream: crate::Stream) -> io::Result<Stream> {
    stream.set_nonblocking(true)?;
    Stream::from_std(stream)
}

#[cfg(windows)]
pub use self::pipe::{Listener, Stream, connect, from_std, listener};

/// Whether `stream`'s peer is a program of this login: this account, and not
/// one a sandbox confined. On Linux the peer's uid must be this process's and it
/// must live in this process's user and mount namespaces, as a session's other
/// sockets require; a program a sandbox put in namespaces of its own is the same
/// account and is refused all the same. On Windows the pipe's DACL grants this
/// account and SYSTEM alone, which an `AppContainer`'s token is not given, and
/// [`Listener::accept`] took only this account's peers.
#[must_use]
pub fn peer_is_this_login(stream: &Stream) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let owner = std::fs::metadata("/proc/self")
            .ok()
            .map(|metadata| metadata.uid());
        stream
            .peer_cred()
            .ok()
            .filter(|credentials| Some(credentials.uid()) == owner)
            .and_then(|credentials| credentials.pid())
            .and_then(|pid| u32::try_from(pid).ok())
            .is_some_and(|pid| {
                let same = |kind: &str| {
                    let ours = std::fs::metadata(format!("/proc/self/ns/{kind}"));
                    let theirs = std::fs::metadata(format!("/proc/{pid}/ns/{kind}"));
                    // A process that cannot be read is not one this login knows.
                    matches!((ours, theirs), (Ok(ours), Ok(theirs))
                        if ours.dev() == theirs.dev() && ours.ino() == theirs.ino())
                };
                same("user") && same("mnt")
            })
    }
    #[cfg(windows)]
    {
        let _ = stream;
        true
    }
}

#[cfg(windows)]
mod pipe {
    use crate::account::{Account, LocalAllocation};
    use crate::windows::{SocketAddr, open, peer_is};
    use std::io;
    use std::os::windows::io::{AsRawHandle, IntoRawHandle, OwnedHandle};
    use std::path::Path;
    use std::pin::Pin;
    use std::sync::Mutex;
    use std::task::{Context, Poll};
    use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
    use tokio::net::windows::named_pipe::{NamedPipeClient, NamedPipeServer};

    /// A listening pipe on the runtime: the instance the next client connects to.
    pub struct Listener {
        name: Vec<u16>,
        account: Account,
        descriptor: LocalAllocation,
        next: Mutex<Option<NamedPipeServer>>,
    }

    /// An instance the synchronous listener made, served on the runtime.
    fn served(pipe: OwnedHandle) -> io::Result<NamedPipeServer> {
        // SAFETY: an overlapped pipe instance owned here, handed over once.
        unsafe { NamedPipeServer::from_raw_handle(pipe.into_raw_handle()) }
    }

    fn instance(name: &[u16], descriptor: &LocalAllocation) -> io::Result<NamedPipeServer> {
        served(crate::windows::instance(name, descriptor, false)?)
    }

    /// Serve `listener`, made by [`crate::bind`], on the current runtime: the
    /// runtime takes over its instance, which holds the pipe's name.
    ///
    /// # Errors
    /// The runtime could not take the instance.
    pub fn listener(listener: crate::Listener) -> io::Result<Listener> {
        let (name, account, descriptor, first) = listener.into_parts();
        let next = first.map(served).transpose()?;
        Ok(Listener {
            name,
            account,
            descriptor,
            next: Mutex::new(next),
        })
    }

    impl Listener {
        /// Accept the next connection from this account; a client of another
        /// account is let go and the wait goes on.
        ///
        /// # Errors
        /// Another task is accepting on this listener, or the pipe could not
        /// take a connection or make its next instance.
        pub async fn accept(&self) -> io::Result<(Stream, SocketAddr)> {
            loop {
                let pending = self
                    .next
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take();
                let server = match pending {
                    Some(server) => server,
                    None => instance(&self.name, &self.descriptor)?,
                };
                server.connect().await?;
                let next = instance(&self.name, &self.descriptor)?;
                *self
                    .next
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(next);
                if peer_is(server.as_raw_handle(), true, &self.account)? {
                    return Ok((Stream::Server(server), SocketAddr));
                }
                server.disconnect()?;
            }
        }
    }

    /// Drive `stream`, connected by [`crate::connect`] or handed over, on the current runtime:
    /// its end of the pipe, server or client, is taken over by the runtime.
    ///
    /// # Errors
    /// The runtime could not take the pipe.
    pub fn from_std(stream: crate::Stream) -> io::Result<Stream> {
        let (pipe, server_end) = stream.into_end();
        let raw = pipe.into_raw_handle();
        // SAFETY: an overlapped pipe handle `stream` owned, handed over once.
        unsafe {
            Ok(if server_end {
                Stream::Server(NamedPipeServer::from_raw_handle(raw)?)
            } else {
                Stream::Client(NamedPipeClient::from_raw_handle(raw)?)
            })
        }
    }

    /// One end of a connected pipe on the runtime.
    #[derive(Debug)]
    pub enum Stream {
        Server(NamedPipeServer),
        Client(NamedPipeClient),
    }

    /// Connect to the pipe `path` names, served by this account alone.
    ///
    /// # Errors
    /// Nothing serves it, it is served by another account, or the runtime
    /// could not take the connection.
    pub async fn connect(path: &Path) -> io::Result<Stream> {
        let account = Account::current()?;
        let name = crate::windows::pipe_name(&account, path)?;
        let pipe = tokio::task::spawn_blocking(move || open(&crate::windows::wide(&name)))
            .await
            .map_err(io::Error::other)??;
        if !peer_is(pipe.as_raw_handle(), false, &account)? {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("{}: the pipe is served by another account", path.display()),
            ));
        }
        // SAFETY: an overlapped pipe handle this function owns, handed over once.
        let client = unsafe { NamedPipeClient::from_raw_handle(pipe.into_raw_handle())? };
        Ok(Stream::Client(client))
    }

    impl AsyncRead for Stream {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            match self.get_mut() {
                Self::Server(pipe) => Pin::new(pipe).poll_read(cx, buf),
                Self::Client(pipe) => Pin::new(pipe).poll_read(cx, buf),
            }
        }
    }

    impl AsyncWrite for Stream {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            match self.get_mut() {
                Self::Server(pipe) => Pin::new(pipe).poll_write(cx, buf),
                Self::Client(pipe) => Pin::new(pipe).poll_write(cx, buf),
            }
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match self.get_mut() {
                Self::Server(pipe) => Pin::new(pipe).poll_flush(cx),
                Self::Client(pipe) => Pin::new(pipe).poll_flush(cx),
            }
        }

        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match self.get_mut() {
                Self::Server(pipe) => Pin::new(pipe).poll_shutdown(cx),
                Self::Client(pipe) => Pin::new(pipe).poll_shutdown(cx),
            }
        }
    }
}

#[cfg(all(test, windows))]
mod tests {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[test]
    fn a_pipe_on_the_runtime_carries_a_request_and_its_answer() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("api.sock");
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let listener = super::listener(crate::bind(&path).unwrap()).unwrap();
            let serving = async {
                let (mut stream, _) = listener.accept().await.unwrap();
                assert!(super::peer_is_this_login(&stream));
                let mut asked = [0; 4];
                stream.read_exact(&mut asked).await.unwrap();
                stream.write_all(b"pong").await.unwrap();
                asked
            };
            let asking = async {
                let mut stream = super::connect(&path).await.unwrap();
                stream.write_all(b"ping").await.unwrap();
                let mut answer = [0; 4];
                stream.read_exact(&mut answer).await.unwrap();
                answer
            };
            let (asked, answer) = tokio::join!(serving, asking);
            assert_eq!((&asked, &answer), (b"ping", b"pong"));
        });
    }
}
