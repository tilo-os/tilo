//! The control socket's transport: who may connect, and frames with the descriptor `Attach`
//! carries.

use std::collections::VecDeque;
use std::io::{self, IoSliceMut, Write as _};
use std::mem::MaybeUninit;
use std::os::fd::{AsRawFd as _, OwnedFd};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags};

use crate::wire::{self, FromKeeper};

/// A session writes small messages; one that does not read its own for this long is closed.
const WRITE_TIMEOUT: Duration = Duration::from_secs(1);

/// Only the keeper's own user may drive it.
#[must_use]
pub fn peer_allowed(peer_uid: u32, own_uid: u32) -> bool {
    peer_uid == own_uid
}

/// The uid of the process at the other end of `stream`, from the kernel (`SO_PEERCRED`).
pub(crate) fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len =
        libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()).map_err(io::Error::other)?;
    // SAFETY: getsockopt writes at most `len` bytes into `credentials`, a ucred it owns.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut len,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(credentials.uid)
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
    /// Said hello at this keeper's version.
    pub greeted: bool,
}

impl Conn {
    pub(crate) fn new(stream: UnixStream) -> io::Result<Self> {
        stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
        Ok(Self {
            stream,
            inbuf: Vec::new(),
            fds: VecDeque::new(),
            greeted: false,
        })
    }

    /// Read what arrived, without waiting: the complete frames, and whether the session is still
    /// there.
    pub(crate) fn receive(&mut self) -> io::Result<(Vec<Vec<u8>>, bool)> {
        let mut open = true;
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
                    self.fds.extend(fds);
                }
            }
            if count == 0 {
                open = false;
                break;
            }
            self.inbuf.extend(bytes.get(..count).unwrap_or_default());
        }
        Ok((wire::take_frames(&mut self.inbuf)?, open))
    }

    /// The next descriptor that arrived, in order.
    pub(crate) fn take_fd(&mut self) -> Option<OwnedFd> {
        self.fds.pop_front()
    }

    pub(crate) fn send(&mut self, message: &FromKeeper) -> io::Result<()> {
        let frame = wire::encode(message)?;
        self.stream.write_all(&frame)
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
