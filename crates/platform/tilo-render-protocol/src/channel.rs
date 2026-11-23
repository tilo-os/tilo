//! The control channel: one Unix socket per app connection, one message per
//! packet, descriptors as `SCM_RIGHTS`.
//!
//! On Linux the socket is `SOCK_SEQPACKET`. macOS has no sequenced packets on
//! Unix sockets; there it is `SOCK_DGRAM`, which on a connected Unix pair is
//! just as reliable and ordered and keeps message boundaries. Its buffers are
//! sized for the largest message, and a peer's close reads as a reset, which
//! this end then answers as closed from then on.
//!
//! A packet longer than [`MAX_MESSAGE_BYTES`] or carrying more than
//! [`MAX_MESSAGE_FDS`] descriptors is truncated by the kernel; the receiver
//! sees the truncation flag and refuses the packet rather than read half a
//! message. Received descriptors are close-on-exec.

use std::io::{self, IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};

use rustix::net::{
    AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags,
    SendAncillaryBuffer, SendAncillaryMessage, SendFlags, SocketFlags, SocketType,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::control::{MAX_MESSAGE_BYTES, MAX_MESSAGE_FDS};
use crate::{WireError, control};

/// The socket type of a channel: see the module's documentation.
#[cfg(target_os = "linux")]
const SOCKET_TYPE: SocketType = SocketType::SEQPACKET;
#[cfg(not(target_os = "linux"))]
const SOCKET_TYPE: SocketType = SocketType::DGRAM;

/// A datagram socket refuses a message larger than its send buffer, and a send its peer has no
/// room for instead of waiting: room for the largest message out, and for many of them in.
#[cfg(not(target_os = "linux"))]
const SEND_BUFFER: usize = MAX_MESSAGE_BYTES * 2;
#[cfg(not(target_os = "linux"))]
const RECV_BUFFER: usize = MAX_MESSAGE_BYTES * 64;

/// One end of a control channel.
pub struct Channel {
    socket: OwnedFd,
    /// The peer is gone. A datagram socket reports its peer's close once; every later
    /// receive and send answers [`ChannelError::Closed`] from this.
    closed: AtomicBool,
}

/// A message received, with the descriptors that came with it.
pub struct Received<T> {
    pub message: T,
    pub fds: Vec<OwnedFd>,
}

/// Why a channel could not carry a message.
#[derive(Debug, thiserror::Error)]
pub enum ChannelError {
    #[error("the peer closed the channel")]
    Closed,
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Wire(#[from] WireError),
}

impl Channel {
    /// Both ends of a new channel, close-on-exec.
    pub fn pair() -> io::Result<(Self, Self)> {
        #[cfg(target_os = "linux")]
        let flags = SocketFlags::CLOEXEC;
        // Set on each end below: macOS's `socketpair` takes no flags.
        #[cfg(not(target_os = "linux"))]
        let flags = SocketFlags::empty();
        let (a, b) = rustix::net::socketpair(AddressFamily::UNIX, SOCKET_TYPE, flags, None)?;
        #[cfg(not(target_os = "linux"))]
        for end in [&a, &b] {
            rustix::io::fcntl_setfd(end, rustix::io::FdFlags::CLOEXEC)?;
        }
        Ok((Self::new(a)?, Self::new(b)?))
    }

    /// Wrap a connected channel socket handed over by a broker.
    pub fn from_socket(socket: OwnedFd) -> io::Result<Self> {
        if rustix::net::sockopt::socket_type(&socket)? != SOCKET_TYPE {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a render channel must be a packet socket",
            ));
        }
        Self::new(socket)
    }

    #[cfg_attr(
        target_os = "linux",
        allow(
            clippy::unnecessary_wraps,
            reason = "a socket's options are set, and can fail, on macOS"
        )
    )]
    fn new(socket: OwnedFd) -> io::Result<Self> {
        // A datagram end's buffers, and a send to a closed peer that answers an error rather
        // than raising `SIGPIPE`.
        #[cfg(not(target_os = "linux"))]
        {
            rustix::net::sockopt::set_socket_send_buffer_size(&socket, SEND_BUFFER)?;
            rustix::net::sockopt::set_socket_recv_buffer_size(&socket, RECV_BUFFER)?;
            rustix::net::sockopt::set_socket_nosigpipe(&socket, true)?;
        }
        Ok(Self {
            socket,
            closed: AtomicBool::new(false),
        })
    }

    /// The peer went: say so now and from now on.
    fn closed(&self) -> ChannelError {
        self.closed.store(true, Ordering::Relaxed);
        ChannelError::Closed
    }

    /// The socket, to wait on.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.socket.as_fd()
    }

    /// Send `message` with `fds`.
    pub fn send<T: Serialize>(
        &self,
        message: &T,
        fds: &[BorrowedFd<'_>],
    ) -> Result<(), ChannelError> {
        if fds.len() > MAX_MESSAGE_FDS {
            return Err(WireError::OverLimit("descriptors in one message").into());
        }
        if self.closed.load(Ordering::Relaxed) {
            return Err(ChannelError::Closed);
        }
        #[cfg(target_os = "linux")]
        let flags = SendFlags::NOSIGNAL;
        // `SO_NOSIGPIPE`, set when the end was made, keeps the signal away.
        #[cfg(not(target_os = "linux"))]
        let flags = SendFlags::empty();
        let bytes = control::encode(message)?;
        let mut space =
            [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(MAX_MESSAGE_FDS))];
        let mut control = SendAncillaryBuffer::new(&mut space);
        if !fds.is_empty() && !control.push(SendAncillaryMessage::ScmRights(fds)) {
            return Err(WireError::OverLimit("descriptors in one message").into());
        }
        let sent = loop {
            match rustix::net::sendmsg(&self.socket, &[IoSlice::new(&bytes)], &mut control, flags) {
                Err(rustix::io::Errno::INTR) => {}
                Err(
                    rustix::io::Errno::PIPE
                    | rustix::io::Errno::CONNRESET
                    | rustix::io::Errno::NOTCONN,
                ) => {
                    return Err(self.closed());
                }
                other => break other.map_err(io::Error::from)?,
            }
        };
        if sent != bytes.len() {
            return Err(WireError::Malformed("short send").into());
        }
        Ok(())
    }

    /// Receive one message, blocking unless the socket is non-blocking.
    ///
    /// # Errors
    ///
    /// [`ChannelError::Closed`] at end of stream; a truncated or malformed
    /// packet, or a receive failure (`WouldBlock` on a non-blocking socket
    /// with nothing to read).
    pub fn recv<T: DeserializeOwned>(&self) -> Result<Received<T>, ChannelError> {
        if self.closed.load(Ordering::Relaxed) {
            return Err(ChannelError::Closed);
        }
        #[cfg(target_os = "linux")]
        let flags = RecvFlags::CMSG_CLOEXEC;
        // Marked close-on-exec below, once received: macOS has no flag for it.
        #[cfg(not(target_os = "linux"))]
        let flags = RecvFlags::empty();
        let mut buffer = vec![0_u8; MAX_MESSAGE_BYTES];
        let mut space =
            [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(MAX_MESSAGE_FDS))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        let received = loop {
            match rustix::net::recvmsg(
                &self.socket,
                &mut [IoSliceMut::new(&mut buffer)],
                &mut control,
                flags,
            ) {
                Err(rustix::io::Errno::INTR) => {}
                Err(rustix::io::Errno::CONNRESET | rustix::io::Errno::NOTCONN) => {
                    return Err(self.closed());
                }
                other => break other.map_err(io::Error::from)?,
            }
        };
        let mut fds = Vec::new();
        for message in control.drain() {
            if let RecvAncillaryMessage::ScmRights(rights) = message {
                fds.extend(rights);
            }
        }
        #[cfg(not(target_os = "linux"))]
        for fd in &fds {
            rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::CLOEXEC).map_err(io::Error::from)?;
        }
        if received.bytes == 0 && fds.is_empty() {
            return Err(self.closed());
        }
        if received
            .flags
            .intersects(ReturnFlags::TRUNC | ReturnFlags::CTRUNC)
        {
            return Err(WireError::OverLimit("message truncated").into());
        }
        let bytes = buffer
            .get(..received.bytes)
            .ok_or(WireError::Malformed("receive length"))?;
        let message = control::decode(bytes)?;
        Ok(Received { message, fds })
    }

    /// End the channel in both directions: a receive waiting on it, on any
    /// thread, returns [`ChannelError::Closed`], and so does every later one.
    pub fn shut_down(&self) -> io::Result<()> {
        rustix::net::shutdown(&self.socket, rustix::net::Shutdown::Both)?;
        Ok(())
    }

    /// Make receives return `WouldBlock` instead of waiting.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        let flags = rustix::fs::fcntl_getfl(&self.socket)?;
        let flags = if nonblocking {
            flags | rustix::fs::OFlags::NONBLOCK
        } else {
            flags.difference(rustix::fs::OFlags::NONBLOCK)
        };
        rustix::fs::fcntl_setfl(&self.socket, flags)?;
        Ok(())
    }
}

impl AsFd for Channel {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.socket.as_fd()
    }
}
