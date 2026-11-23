//! The actions connection a session hands a surface app, one framed message at
//! a time.
//!
//! A session serves its surfaces over a Unix-domain socket and puts the
//! connected descriptor on the app's environment. A platform without those
//! sockets hands no surface its actions, so nothing here can be taken.
use wire_frame::Framing;

/// Both directions' framing: one length-prefixed message at a time, as the
/// collaboration wire frames the same protocol's frames.
pub const FRAMING: Framing = Framing::little_endian(64 << 20);

#[cfg(unix)]
mod connected {
    use std::io;
    use std::os::fd::FromRawFd;
    use std::os::unix::net::UnixStream;

    use super::FRAMING;

    /// Reading the session's messages.
    pub struct Reader(UnixStream);
    /// Writing the app's answers.
    pub struct Writer(UnixStream);

    /// The two halves of the socket `number` names, if it is one.
    ///
    /// # Safety
    ///
    /// The caller owns `number`: it was handed over on this process's
    /// environment and is taken exactly once.
    #[must_use]
    pub unsafe fn take(number: i32) -> Option<(Reader, Writer)> {
        // SAFETY: the caller owns the descriptor, as `from_raw_fd` requires.
        let stream = unsafe { UnixStream::from_raw_fd(number) };
        split(stream)
    }

    /// The two halves of `stream`, one end of a socket to the session.
    #[must_use]
    pub fn split(stream: UnixStream) -> Option<(Reader, Writer)> {
        let writing = stream.try_clone().ok()?;
        Some((Reader(stream), Writer(writing)))
    }

    impl Reader {
        /// The next message, `None` once the connection ends.
        ///
        /// # Errors
        ///
        /// The frame could not be read or was larger than the framing allows.
        pub fn read(&mut self) -> io::Result<Option<Vec<u8>>> {
            FRAMING.read(&mut self.0).map_err(Into::into)
        }
    }

    impl Writer {
        /// Write one message.
        ///
        /// # Errors
        ///
        /// The frame could not be written.
        pub fn write(&mut self, body: &[u8]) -> io::Result<()> {
            FRAMING.write(&mut self.0, body).map_err(Into::into)
        }
    }
}

#[cfg(not(unix))]
mod connected {
    use std::io;

    /// Never constructed: a session hands a surface its actions on a Unix
    /// socket, and this platform has none.
    pub enum Reader {}
    /// Never constructed, as [`Reader`].
    pub enum Writer {}

    /// A platform without Unix-domain sockets never hands one over.
    ///
    /// # Safety
    ///
    /// Always returns `None`; the number is not taken.
    #[must_use]
    pub unsafe fn take(_number: i32) -> Option<(Reader, Writer)> {
        None
    }

    impl Reader {
        /// Never reached: no reader exists, since [`take`] hands none out here.
        ///
        /// # Errors
        ///
        /// [`io::ErrorKind::Unsupported`], were anything able to call it.
        pub fn read(&mut self) -> io::Result<Option<Vec<u8>>> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "only a Unix socket carries an app's actions",
            ))
        }
    }

    impl Writer {
        /// Never reached: no writer exists, since [`take`] hands none out here.
        ///
        /// # Errors
        ///
        /// [`io::ErrorKind::Unsupported`], were anything able to call it.
        pub fn write(&mut self, _body: &[u8]) -> io::Result<()> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "only a Unix socket carries an app's actions",
            ))
        }
    }
}

#[cfg(unix)]
pub use connected::split;
pub use connected::{Reader, Writer, take};
