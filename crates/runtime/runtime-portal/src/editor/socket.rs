//! Connected descriptor custody only; callers must still admit its producer.
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixDatagram;
use std::time::Duration;

/// A bounded packet transport. Possession is not app or resource authority.
pub struct Socket(UnixDatagram);

impl std::fmt::Debug for Socket {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("Socket(connected native editor)")
    }
}

impl AsFd for Socket {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.0.as_fd()
    }
}
impl Socket {
    /// Create private, close-on-exec endpoints without an ambient listener.
    pub fn pair() -> io::Result<(Self, Self)> {
        let mut fds = [-1; 2];
        // SAFETY: initialized outputs; a successful pair is owned exactly once.
        if unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                fds.as_mut_ptr(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: socketpair transferred the two freshly owned descriptors.
        let (first, second) =
            unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
        Ok((Self::adopt(first)?, Self::adopt(second)?))
    }

    /// Adopt a connected Unix seqpacket descriptor; close it on every error.
    /// # Errors
    /// Rejects wrong socket type/family, disconnected descriptors or bad flags.
    pub fn adopt(fd: OwnedFd) -> io::Result<Self> {
        let mut kind = 0;
        let mut size =
            libc::socklen_t::try_from(std::mem::size_of_val(&kind)).map_err(io::Error::other)?;
        // SAFETY: an owned live fd and properly sized initialized output.
        if unsafe {
            libc::getsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_TYPE,
                (&raw mut kind).cast(),
                &raw mut size,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        if kind != libc::SOCK_SEQPACKET || size as usize != std::mem::size_of_val(&kind) {
            return Err(io::Error::other(
                "editor needs a connected Unix packet socket",
            ));
        }
        rustix::io::fcntl_setfd(&fd, rustix::io::FdFlags::CLOEXEC)?;
        let socket = UnixDatagram::from(fd);
        socket.peer_addr()?;
        Ok(Self(socket))
    }

    /// Bound reads on a startup or transport worker, never on the UI thread.
    pub fn read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.0.set_read_timeout(timeout)
    }

    /// Send exactly one bounded packet without waiting for receiver capacity.
    /// # Errors
    /// Rejects empty/oversized packets and partial/failed delivery. A caller
    /// must retire uncertain mutations; it must not blindly retry them.
    pub fn send(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() || bytes.len() > super::FRAME_BYTES {
            return Err(io::Error::other("editor packet exceeds its bound"));
        }
        // SAFETY: initialized bytes remain live and the socket is owned.
        let sent = unsafe {
            libc::send(
                self.0.as_raw_fd(),
                bytes.as_ptr().cast(),
                bytes.len(),
                libc::MSG_DONTWAIT | libc::MSG_NOSIGNAL,
            )
        };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }
        if usize::try_from(sent).ok() != Some(bytes.len()) {
            return Err(io::Error::other("editor packet delivery is uncertain"));
        }
        Ok(())
    }

    /// Receive one exact bounded packet, refusing and closing ancillary rights.
    /// This checks framing custody only, not kernel sender identity.
    pub fn receive(&self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.len() > super::FRAME_BYTES {
            return Err(io::Error::other("editor packet exceeds its bound"));
        }
        let received = crate::fd_transport::recv_data(&self.0, bytes)?;
        if received == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "editor retired",
            ));
        }
        Ok(received)
    }

    /// Retire all copies of this endpoint when its session or worker ends.
    pub fn retire(&self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}
