//! The control socket's transport: who may connect, and frames with the descriptor `Attach`
//! carries.

use std::collections::VecDeque;
use std::io::{self, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;

use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, SendFlags};
use smithay::reexports::calloop::RegistrationToken;

use crate::wire;

/// What a session was sent and has not read yet, beyond which it is closed: a session that stops
/// reading never holds the keeper, and never grows it without end.
const PENDING_LIMIT: usize = 4 * 1024 * 1024;

/// Descriptors kept from a session until a message takes them; more are closed on arrival, so a
/// peer cannot use up the keeper's descriptors.
const FD_LIMIT: usize = 4;

/// Only the keeper's own user may drive it.
#[must_use]
pub fn peer_allowed(peer_uid: u32, own_uid: u32) -> bool {
    peer_uid == own_uid
}

/// The uid of the process at the other end of `stream`, from the kernel.
pub(crate) fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    peer_facts::facts(stream).map(|facts| facts.uid)
}

/// The process at the other end of `stream`, when the kernel names one this namespace can look
/// up.
pub(crate) fn peer_pid(stream: &UnixStream) -> io::Result<Option<u32>> {
    peer_facts::facts(stream).map(|facts| facts.pid)
}

/// The process's own uid.
pub(crate) fn own_uid() -> u32 {
    rustix::process::getuid().as_raw()
}

/// One session connection.
pub(crate) struct Conn {
    pub stream: UnixStream,
    inbuf: Vec<u8>,
    fds: VecDeque<OwnedFd>,
    /// Written, not yet taken by the socket.
    pending: Vec<u8>,
    /// Watches the socket for room while something is pending.
    pub writing: Option<RegistrationToken>,
    /// Said hello at this keeper's version.
    pub greeted: bool,
    /// Said hello at another version and was told [`wire::HOLDING`]: while it stays open the app
    /// is not ended for want of a desktop.
    pub holds: bool,
}

impl Conn {
    pub(crate) const fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            inbuf: Vec::new(),
            fds: VecDeque::new(),
            pending: Vec::new(),
            writing: None,
            greeted: false,
            holds: false,
        }
    }

    /// Read what arrived, without waiting: the complete frames, and whether the session is still
    /// there. A frame larger than the framing allows is an error, found as soon as its prefix
    /// arrives, so what a session sends is never held beyond one frame.
    pub(crate) fn receive(&mut self) -> io::Result<(Vec<Vec<u8>>, bool)> {
        let mut open = true;
        let mut frames = Vec::new();
        loop {
            let mut bytes = [0_u8; 4096];
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(4))];
            let mut control = RecvAncillaryBuffer::new(&mut space);
            let received = rustix::net::recvmsg(
                &self.stream,
                &mut [IoSliceMut::new(&mut bytes)],
                &mut control,
                RecvFlags::DONTWAIT | RecvFlags::CMSG_CLOEXEC,
            );
            let count = match received {
                Ok(message) => message.bytes,
                Err(rustix::io::Errno::AGAIN) => break,
                Err(rustix::io::Errno::INTR) => continue,
                Err(error) => return Err(error.into()),
            };
            for message in control.drain() {
                if let RecvAncillaryMessage::ScmRights(fds) = message {
                    for fd in fds {
                        if self.fds.len() < FD_LIMIT {
                            self.fds.push_back(fd);
                        }
                    }
                }
            }
            if count == 0 {
                open = false;
                break;
            }
            self.inbuf.extend(bytes.get(..count).unwrap_or_default());
            frames.extend(wire::take_frames(&mut self.inbuf)?);
        }
        Ok((frames, open))
    }

    /// The next descriptor that arrived, in order.
    pub(crate) fn take_fd(&mut self) -> Option<OwnedFd> {
        self.fds.pop_front()
    }

    /// Send `message` without waiting: whether all of it reached the socket (otherwise the rest
    /// is pending, for [`Self::flush`] once the socket has room).
    ///
    /// # Errors
    /// The session is gone, or has left more unread than [`PENDING_LIMIT`].
    pub(crate) fn send<T: serde::Serialize>(&mut self, message: &T) -> io::Result<bool> {
        let frame = wire::encode(message)?;
        if self.pending.len().saturating_add(frame.len()) > PENDING_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::OutOfMemory,
                "the session left too much unread",
            ));
        }
        self.pending.extend(frame);
        self.flush()
    }

    /// Write what is pending, without waiting, and never raising `SIGPIPE`: whether nothing is
    /// left.
    pub(crate) fn flush(&mut self) -> io::Result<bool> {
        while !self.pending.is_empty() {
            match rustix::net::send(
                &self.stream,
                &self.pending,
                SendFlags::DONTWAIT | SendFlags::NOSIGNAL,
            ) {
                Ok(0) => return Err(io::ErrorKind::WriteZero.into()),
                Ok(sent) => {
                    self.pending.drain(..sent.min(self.pending.len()));
                }
                Err(rustix::io::Errno::AGAIN) => return Ok(false),
                Err(rustix::io::Errno::INTR) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_keepers_own_user_may_drive_it() {
        assert!(peer_allowed(1000, 1000));
        assert!(!peer_allowed(0, 1000));
        assert!(!peer_allowed(1001, 1000));
    }
}
