//! The control channel: one `SOCK_SEQPACKET` socket per app connection,
//! one message per packet, descriptors as `SCM_RIGHTS`.
//!
//! A packet longer than [`MAX_MESSAGE_BYTES`] or carrying more than
//! [`MAX_MESSAGE_FDS`] descriptors is truncated by the kernel; the receiver
//! sees the truncation flag and refuses the packet rather than read half a
//! message. Received descriptors are close-on-exec.

use std::io::{self, IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use rustix::net::{
    AddressFamily, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags,
    SendAncillaryBuffer, SendAncillaryMessage, SendFlags, SocketFlags, SocketType,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::control::{MAX_MESSAGE_BYTES, MAX_MESSAGE_FDS};
use crate::{WireError, control};

/// One end of a control channel.
pub struct Channel {
    socket: OwnedFd,
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
        let (a, b) = rustix::net::socketpair(
            AddressFamily::UNIX,
            SocketType::SEQPACKET,
            SocketFlags::CLOEXEC,
            None,
        )?;
        Ok((Self { socket: a }, Self { socket: b }))
    }

    /// Wrap a connected `SOCK_SEQPACKET` socket handed over by a broker.
    ///
    /// # Errors
    ///
    /// When `socket` is not a sequenced-packet socket: a stream socket would
    /// lose message boundaries.
    pub fn from_socket(socket: OwnedFd) -> io::Result<Self> {
        if rustix::net::sockopt::socket_type(&socket)? != SocketType::SEQPACKET {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "a render channel must be a SOCK_SEQPACKET socket",
            ));
        }
        Ok(Self { socket })
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
        let bytes = control::encode(message)?;
        let mut space =
            [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(MAX_MESSAGE_FDS))];
        let mut control = SendAncillaryBuffer::new(&mut space);
        if !fds.is_empty() && !control.push(SendAncillaryMessage::ScmRights(fds)) {
            return Err(WireError::OverLimit("descriptors in one message").into());
        }
        let sent = loop {
            match rustix::net::sendmsg(
                &self.socket,
                &[IoSlice::new(&bytes)],
                &mut control,
                SendFlags::NOSIGNAL,
            ) {
                Err(rustix::io::Errno::INTR) => {}
                Err(rustix::io::Errno::PIPE | rustix::io::Errno::CONNRESET) => {
                    return Err(ChannelError::Closed);
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
        let mut buffer = vec![0_u8; MAX_MESSAGE_BYTES];
        let mut space =
            [MaybeUninit::<u8>::uninit(); rustix::cmsg_space!(ScmRights(MAX_MESSAGE_FDS))];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        let received = loop {
            match rustix::net::recvmsg(
                &self.socket,
                &mut [IoSliceMut::new(&mut buffer)],
                &mut control,
                RecvFlags::CMSG_CLOEXEC,
            ) {
                Err(rustix::io::Errno::INTR) => {}
                Err(rustix::io::Errno::CONNRESET) => return Err(ChannelError::Closed),
                other => break other.map_err(io::Error::from)?,
            }
        };
        let mut fds = Vec::new();
        for message in control.drain() {
            if let RecvAncillaryMessage::ScmRights(rights) = message {
                fds.extend(rights);
            }
        }
        if received.bytes == 0 && fds.is_empty() {
            return Err(ChannelError::Closed);
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
    ///
    /// # Errors
    ///
    /// When the socket refuses the shutdown.
    pub fn shut_down(&self) -> io::Result<()> {
        rustix::net::shutdown(&self.socket, rustix::net::Shutdown::Both)?;
        Ok(())
    }

    /// Make receives return `WouldBlock` instead of waiting.
    ///
    /// # Errors
    ///
    /// When the descriptor's flags cannot be changed.
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
