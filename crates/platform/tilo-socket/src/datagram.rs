//! Two connected ends that carry whole datagrams: a Unix datagram socket pair, or on Windows a
//! pipe only this process joined ([`crate::link::pair`]) with each datagram written behind its
//! length, so a datagram is read whole however the pipe splits its bytes. A caller hands one end to
//! a process or thread it starts and keeps the other; [`crate::tokio::Datagram`] drives an end on
//! a runtime.
//!
//! One thread reads an end at a time: on Windows a datagram is a header and its bytes, and two
//! readers of one end would split them.

use std::io;
#[cfg(unix)]
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd, RawFd};
#[cfg(unix)]
use std::os::unix::net::UnixDatagram;
use std::time::Duration;

/// The largest datagram an end carries: the largest UDP payload.
pub const MAX_DATAGRAM: usize = 65_535;

/// One end of a connected datagram pair.
#[derive(Debug)]
pub struct Datagram {
    #[cfg(unix)]
    socket: UnixDatagram,
    #[cfg(windows)]
    stream: crate::Stream,
    #[cfg(not(any(unix, windows)))]
    never: std::convert::Infallible,
}

impl Datagram {
    /// Two connected ends, both in this process.
    ///
    /// # Errors
    /// The system's error making the pair.
    pub fn pair() -> io::Result<(Self, Self)> {
        #[cfg(unix)]
        {
            let (ours, theirs) = UnixDatagram::pair()?;
            Ok((Self { socket: ours }, Self { socket: theirs }))
        }
        #[cfg(windows)]
        {
            let (ours, theirs) = crate::link::pair()?;
            Ok((Self { stream: ours }, Self { stream: theirs }))
        }
        #[cfg(not(any(unix, windows)))]
        {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "a page has no local socket",
            ))
        }
    }

    /// Send `payload` as one datagram.
    ///
    /// # Errors
    /// The other end is gone, or `payload` is longer than [`MAX_DATAGRAM`].
    pub fn send(&self, payload: &[u8]) -> io::Result<usize> {
        if payload.len() > MAX_DATAGRAM {
            return Err(too_long(payload.len()));
        }
        #[cfg(unix)]
        {
            self.socket.send(payload)
        }
        #[cfg(windows)]
        {
            use std::io::Write as _;
            (&self.stream).write_all(&framed(payload))?;
            Ok(payload.len())
        }
        #[cfg(not(any(unix, windows)))]
        {
            match self.never {}
        }
    }

    /// The next datagram, into `buffer`: its length, cut to the buffer's when it is longer, as a
    /// datagram socket reads one. `0` when the other end is gone and says so (a Windows pipe
    /// does; a Unix datagram socket may wait out its timeout instead).
    ///
    /// # Errors
    /// The read timeout passed before a datagram began ([`io::ErrorKind::WouldBlock`] or
    /// [`io::ErrorKind::TimedOut`]), or the system's error reading.
    pub fn recv(&self, buffer: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)]
        {
            self.socket.recv(buffer)
        }
        #[cfg(windows)]
        {
            let mut header = [0_u8; HEADER];
            if !self.fill(&mut header, false)? {
                return Ok(0);
            }
            let length = usize::try_from(u32::from_le_bytes(header)).unwrap_or(usize::MAX);
            if length > MAX_DATAGRAM {
                return Err(too_long(length));
            }
            let kept = length.min(buffer.len());
            let mut rest = vec![0_u8; length.saturating_sub(kept)];
            let whole = self.fill(buffer.get_mut(..kept).unwrap_or_default(), true)?
                && self.fill(&mut rest, true)?;
            Ok(if whole { kept } else { 0 })
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = buffer;
            match self.never {}
        }
    }

    /// Fill `into` from the pipe: `false` when the other end closed first. A timeout ends the
    /// wait only before a datagram's first byte; once it `begun`, the rest of it is read whatever
    /// the timeout, so the next read starts at a header.
    #[cfg(windows)]
    fn fill(&self, into: &mut [u8], begun: bool) -> io::Result<bool> {
        use std::io::Read as _;
        let mut at = 0;
        while let Some(left) = into.get_mut(at..).filter(|left| !left.is_empty()) {
            match (&self.stream).read(left) {
                Ok(0) => return Ok(false),
                Ok(read) => at = at.saturating_add(read),
                Err(error)
                    if (begun || at > 0)
                        && matches!(
                            error.kind(),
                            io::ErrorKind::WouldBlock
                                | io::ErrorKind::TimedOut
                                | io::ErrorKind::Interrupted
                        ) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(true)
    }

    /// How long [`Self::recv`] waits for a datagram to begin; `None` waits for ever.
    ///
    /// # Errors
    /// The system's error setting it.
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        #[cfg(unix)]
        {
            self.socket.set_read_timeout(timeout)
        }
        #[cfg(windows)]
        {
            self.stream.set_read_timeout(timeout)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = timeout;
            match self.never {}
        }
    }

    /// A second handle on this end, for a thread that sends while another reads.
    ///
    /// # Errors
    /// The system's error duplicating it.
    pub fn try_clone(&self) -> io::Result<Self> {
        #[cfg(unix)]
        {
            Ok(Self {
                socket: self.socket.try_clone()?,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {
                stream: self.stream.try_clone()?,
            })
        }
        #[cfg(not(any(unix, windows)))]
        {
            match self.never {}
        }
    }

    #[cfg(all(unix, feature = "tokio"))]
    pub(crate) fn into_socket(self) -> UnixDatagram {
        self.socket
    }

    #[cfg(all(windows, feature = "tokio"))]
    pub(crate) fn into_stream(self) -> crate::Stream {
        self.stream
    }
}

/// A datagram's header on Windows: its length, little-endian.
#[cfg(windows)]
pub(crate) const HEADER: usize = 4;

/// `payload` behind its length, written in one call so the pipe takes it as one message.
#[cfg(windows)]
pub(crate) fn framed(payload: &[u8]) -> Vec<u8> {
    let length = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    let mut framed = Vec::with_capacity(HEADER.saturating_add(payload.len()));
    framed.extend_from_slice(&length.to_le_bytes());
    framed.extend_from_slice(payload);
    framed
}

pub(crate) fn too_long(length: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("a datagram of {length} bytes is longer than the {MAX_DATAGRAM} one carries"),
    )
}

/// The end a process inherited by its descriptor.
#[cfg(unix)]
impl From<OwnedFd> for Datagram {
    fn from(fd: OwnedFd) -> Self {
        Self {
            socket: UnixDatagram::from(fd),
        }
    }
}

#[cfg(unix)]
impl AsFd for Datagram {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.socket.as_fd()
    }
}

#[cfg(unix)]
impl AsRawFd for Datagram {
    fn as_raw_fd(&self) -> RawFd {
        self.socket.as_raw_fd()
    }
}

#[cfg(test)]
#[cfg(any(unix, windows))]
mod tests {
    use super::Datagram;
    use std::time::Duration;

    #[test]
    fn a_datagram_arrives_whole_and_cut_to_the_buffer() {
        let (ours, theirs) = Datagram::pair().unwrap();
        ours.send(b"first").unwrap();
        ours.send(&[7; 3000]).unwrap();
        ours.send(b"third").unwrap();
        let mut buffer = [0_u8; 16];
        assert_eq!(theirs.recv(&mut buffer).unwrap(), 5);
        assert_eq!(&buffer[..5], b"first");
        // A datagram longer than the buffer is cut, and the next starts where it should.
        assert_eq!(theirs.recv(&mut buffer).unwrap(), 16);
        assert_eq!(buffer, [7; 16]);
        assert_eq!(theirs.recv(&mut buffer).unwrap(), 5);
        assert_eq!(&buffer[..5], b"third");
    }

    #[test]
    fn a_read_waits_no_longer_than_its_timeout() {
        let (_ours, theirs) = Datagram::pair().unwrap();
        theirs
            .set_read_timeout(Some(Duration::from_millis(50)))
            .unwrap();
        let waited = theirs.recv(&mut [0_u8; 8]).unwrap_err();
        assert!(
            matches!(
                waited.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
            "{waited}"
        );
    }
}
