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

/// The process at the other end of `stream`, as the kernel saw it connect.
///
/// # Errors
/// The kernel names no process for it.
pub fn peer_pid(stream: &Stream) -> std::io::Result<u32> {
    #[cfg(unix)]
    {
        stream
            .peer_cred()?
            .pid()
            .and_then(|pid| u32::try_from(pid).ok())
            .ok_or_else(|| std::io::Error::other("the peer's process is unknown"))
    }
    #[cfg(windows)]
    {
        stream.peer_pid()
    }
}

/// Wait for the first `buffer.len()` bytes the peer sends, and copy them into `buffer` without
/// consuming them: the next read still returns them. Fewer only when the peer closed first, `0`
/// when it sent nothing. Cancelling the wait loses nothing.
///
/// # Errors
/// The stream could not be read.
pub async fn peek(stream: &mut Stream, buffer: &mut [u8]) -> std::io::Result<usize> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd as _;
        loop {
            let ready = stream.ready(::tokio::io::Interest::READABLE).await?;
            // A peek that finds only part reports `WouldBlock`, so the readiness clears and the
            // next bytes wake this loop.
            match stream.try_io(::tokio::io::Interest::READABLE, || {
                // SAFETY: `buffer` is writable for its length and `stream` owns the live socket
                // for this whole call; MSG_PEEK leaves the bytes for the next read.
                let read = unsafe {
                    libc::recv(
                        stream.as_raw_fd(),
                        buffer.as_mut_ptr().cast(),
                        buffer.len(),
                        libc::MSG_PEEK | libc::MSG_DONTWAIT,
                    )
                };
                let read = usize::try_from(read).map_err(|_| std::io::Error::last_os_error())?;
                if read == 0 || read >= buffer.len() || ready.is_read_closed() {
                    return Ok(read);
                }
                Err(std::io::ErrorKind::WouldBlock.into())
            }) {
                Ok(read) => return Ok(read),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            }
        }
    }
    #[cfg(windows)]
    {
        stream.peek(buffer).await
    }
}

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
                    return Ok((Stream::new(End::Server(server)), SocketAddr));
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
                Stream::new(End::Server(NamedPipeServer::from_raw_handle(raw)?))
            } else {
                Stream::new(End::Client(NamedPipeClient::from_raw_handle(raw)?))
            })
        }
    }

    /// One end of a connected pipe on the runtime.
    #[derive(Debug)]
    pub struct Stream {
        end: End,
        /// What [`Stream::peek`] read off the pipe ahead of the reader. The runtime reads a
        /// pipe into a buffer of its own as soon as data arrives, so `PeekNamedPipe` cannot be
        /// trusted to see what came.
        peeked: Vec<u8>,
    }

    #[derive(Debug)]
    enum End {
        Server(NamedPipeServer),
        Client(NamedPipeClient),
    }

    impl Stream {
        const fn new(end: End) -> Self {
            Self {
                end,
                peeked: Vec::new(),
            }
        }

        /// As [`super::peer_pid`].
        pub(super) fn peer_pid(&self) -> io::Result<u32> {
            match &self.end {
                End::Server(pipe) => crate::windows::peer_pid(pipe.as_raw_handle(), true),
                End::Client(pipe) => crate::windows::peer_pid(pipe.as_raw_handle(), false),
            }
        }

        /// As [`super::peek`].
        pub(super) async fn peek(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            use tokio::io::AsyncReadExt as _;
            let mut chunk = [0_u8; 256];
            while self.peeked.len() < buffer.len() {
                let wanted = buffer
                    .len()
                    .saturating_sub(self.peeked.len())
                    .min(chunk.len());
                let Some(chunk) = chunk.get_mut(..wanted) else {
                    break;
                };
                let read = match &mut self.end {
                    End::Server(pipe) => pipe.read(chunk).await?,
                    End::Client(pipe) => pipe.read(chunk).await?,
                };
                let Some(read) = chunk.get(..read).filter(|read| !read.is_empty()) else {
                    break;
                };
                self.peeked.extend_from_slice(read);
            }
            let have = self.peeked.len().min(buffer.len());
            if let (Some(into), Some(from)) = (buffer.get_mut(..have), self.peeked.get(..have)) {
                into.copy_from_slice(from);
            }
            Ok(have)
        }
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
        Ok(Stream::new(End::Client(client)))
    }

    impl AsyncRead for Stream {
        fn poll_read(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let this = self.get_mut();
            if !this.peeked.is_empty() {
                let given = this.peeked.len().min(buf.remaining());
                buf.put_slice(this.peeked.get(..given).unwrap_or_default());
                this.peeked.drain(..given);
                return Poll::Ready(Ok(()));
            }
            match &mut this.end {
                End::Server(pipe) => Pin::new(pipe).poll_read(cx, buf),
                End::Client(pipe) => Pin::new(pipe).poll_read(cx, buf),
            }
        }
    }

    impl AsyncWrite for Stream {
        fn poll_write(
            self: Pin<&mut Self>,
            cx: &mut Context<'_>,
            buf: &[u8],
        ) -> Poll<io::Result<usize>> {
            match &mut self.get_mut().end {
                End::Server(pipe) => Pin::new(pipe).poll_write(cx, buf),
                End::Client(pipe) => Pin::new(pipe).poll_write(cx, buf),
            }
        }

        fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match &mut self.get_mut().end {
                End::Server(pipe) => Pin::new(pipe).poll_flush(cx),
                End::Client(pipe) => Pin::new(pipe).poll_flush(cx),
            }
        }

        fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            match &mut self.get_mut().end {
                End::Server(pipe) => Pin::new(pipe).poll_shutdown(cx),
                End::Client(pipe) => Pin::new(pipe).poll_shutdown(cx),
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
