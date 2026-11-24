//! A descriptor a local connection carries: an open file, or one end of a connection.
//!
//! A Unix connection hands the file descriptor itself; Windows hands a handle. Both are one type
//! here, so a protocol above this crate names it once and means the same thing on every target.
//! Sending one beside a line is the caller's transport: `SCM_RIGHTS` on a Unix socket, and a
//! handle duplicated into the process at the other end on Windows.

use std::fs::File;
use std::io;

/// An open file or connection, carried beside a message on a local connection.
#[derive(Debug)]
pub struct Descriptor(Held);

#[cfg(unix)]
type Held = std::os::fd::OwnedFd;
#[cfg(windows)]
type Held = windows::Held;
#[cfg(not(any(unix, windows)))]
type Held = std::convert::Infallible;

#[cfg(windows)]
mod windows {
    use std::fmt;
    use std::io;
    use std::os::windows::io::{
        AsHandle, AsRawHandle, BorrowedHandle, FromRawHandle as _, OwnedHandle, RawHandle,
    };

    use crate::Stream;

    /// What a descriptor's handle is: a connection, or any other handle (a section).
    pub(super) enum Held {
        Pipe(Stream),
        Other(OwnedHandle),
    }

    impl Held {
        /// `handle` as what it is: a pipe, or any other handle.
        pub(super) fn of(handle: OwnedHandle) -> io::Result<Self> {
            // SAFETY: a live handle, only asked its type.
            let kind = unsafe {
                windows_sys::Win32::Storage::FileSystem::GetFileType(handle.as_raw_handle())
            };
            if kind == windows_sys::Win32::Storage::FileSystem::FILE_TYPE_PIPE {
                Stream::from_handle(handle).map(Self::Pipe)
            } else {
                Ok(Self::Other(handle))
            }
        }

        pub(super) fn handle(&self) -> RawHandle {
            match self {
                Self::Pipe(stream) => stream.as_raw_handle(),
                Self::Other(handle) => handle.as_raw_handle(),
            }
        }
    }

    impl fmt::Debug for Held {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("Held")
                .field("handle", &self.handle())
                .finish()
        }
    }

    impl AsRawHandle for Held {
        fn as_raw_handle(&self) -> RawHandle {
            self.handle()
        }
    }

    impl AsHandle for Held {
        fn as_handle(&self) -> BorrowedHandle<'_> {
            // SAFETY: the handle this descriptor owns, borrowed for as long as it lives.
            unsafe { BorrowedHandle::borrow_raw(self.handle()) }
        }
    }

    impl Held {
        /// The connection, when this descriptor is one.
        pub(super) fn stream(&self) -> io::Result<&Stream> {
            match self {
                Self::Pipe(stream) => Ok(stream),
                Self::Other(_) => Err(no_pipe()),
            }
        }

        /// The connection, owned.
        pub(super) fn into_stream(self) -> io::Result<Stream> {
            match self {
                Self::Pipe(stream) => Ok(stream),
                Self::Other(_) => Err(no_pipe()),
            }
        }

        /// The handle, owned.
        pub(super) fn into_handle(self) -> OwnedHandle {
            match self {
                Self::Pipe(stream) => stream.into_handle(),
                Self::Other(handle) => handle,
            }
        }
    }

    /// What a caller that wanted a connection was handed instead.
    pub(super) fn no_pipe() -> io::Error {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "a handle handed on as a connection is no pipe",
        )
    }
}

impl Descriptor {
    /// A descriptor for `held`.
    #[must_use]
    fn of(held: Held) -> Self {
        Self(held)
    }

    /// The file this descriptor holds open.
    ///
    /// A connection handed as a file still opens as it, which is how a chosen folder or a section
    /// is read: the descriptor names the thing, not how it is carried.
    #[cfg(unix)]
    #[must_use]
    pub fn into_file(self) -> File {
        File::from(self.0)
    }

    /// The file this descriptor holds open.
    #[cfg(windows)]
    #[must_use]
    pub fn into_file(self) -> File {
        File::from(match self.0 {
            windows::Held::Pipe(stream) => stream.into_handle(),
            windows::Held::Other(handle) => handle,
        })
    }

    /// The file this descriptor holds open: a page carries none.
    #[cfg(not(any(unix, windows)))]
    #[must_use]
    pub fn into_file(self) -> File {
        match self.0 {}
    }

    /// The connection, owned.
    ///
    /// # Errors
    /// `InvalidInput` for a descriptor that is no connection.
    #[cfg(unix)]
    pub fn into_stream(self) -> io::Result<crate::Stream> {
        Ok(crate::Stream::from(self.0))
    }

    /// The connection, owned.
    ///
    /// # Errors
    /// `InvalidInput` for a descriptor that is no connection.
    #[cfg(windows)]
    pub fn into_stream(self) -> io::Result<crate::Stream> {
        self.0.into_stream()
    }

    /// The connection, when this descriptor is one.
    ///
    /// # Errors
    /// `InvalidInput` for a descriptor that is no connection.
    #[cfg(windows)]
    pub fn stream(&self) -> io::Result<&crate::Stream> {
        self.0.stream()
    }
}

#[cfg(unix)]
impl From<std::os::fd::OwnedFd> for Descriptor {
    fn from(fd: std::os::fd::OwnedFd) -> Self {
        Self::of(fd)
    }
}

#[cfg(unix)]
impl From<Descriptor> for std::os::fd::OwnedFd {
    fn from(descriptor: Descriptor) -> Self {
        descriptor.0
    }
}

#[cfg(unix)]
impl std::os::fd::IntoRawFd for Descriptor {
    fn into_raw_fd(self) -> std::os::fd::RawFd {
        std::os::fd::IntoRawFd::into_raw_fd(self.0)
    }
}

#[cfg(unix)]
impl From<Descriptor> for crate::Stream {
    fn from(descriptor: Descriptor) -> Self {
        Self::from(descriptor.0)
    }
}

#[cfg(unix)]
impl From<crate::Stream> for Descriptor {
    fn from(stream: crate::Stream) -> Self {
        Self::of(stream.into())
    }
}

#[cfg(unix)]
impl std::os::fd::AsRawFd for Descriptor {
    fn as_raw_fd(&self) -> std::os::fd::RawFd {
        self.0.as_raw_fd()
    }
}

#[cfg(unix)]
impl std::os::fd::AsFd for Descriptor {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        self.0.as_fd()
    }
}

#[cfg(unix)]
impl std::os::fd::FromRawFd for Descriptor {
    /// # Safety
    /// `fd` is an open descriptor nothing else owns.
    unsafe fn from_raw_fd(fd: std::os::fd::RawFd) -> Self {
        // SAFETY: the caller hands over an open descriptor nothing else owns.
        Self::of(unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) })
    }
}

#[cfg(windows)]
impl Descriptor {
    /// A connection that carries no handles.
    #[must_use]
    pub fn new(stream: crate::Stream) -> Self {
        Self::of(windows::Held::Pipe(stream))
    }

    /// `handle`, to hand on in a message: a pipe is a connection, anything else is handed as it
    /// is.
    ///
    /// # Errors
    /// The system's error asking which kind of handle it is.
    pub fn from_handle(handle: std::os::windows::io::OwnedHandle) -> io::Result<Self> {
        windows::Held::of(handle).map(Self::of)
    }

    /// The handle, owned.
    #[must_use]
    pub fn into_handle(self) -> std::os::windows::io::OwnedHandle {
        self.0.into_handle()
    }
}

#[cfg(windows)]
impl From<crate::Stream> for Descriptor {
    fn from(stream: crate::Stream) -> Self {
        Self::of(windows::Held::Pipe(stream))
    }
}

#[cfg(windows)]
impl std::os::windows::io::AsRawHandle for Descriptor {
    fn as_raw_handle(&self) -> std::os::windows::io::RawHandle {
        self.0.as_raw_handle()
    }
}

#[cfg(windows)]
impl std::os::windows::io::AsHandle for Descriptor {
    fn as_handle(&self) -> std::os::windows::io::BorrowedHandle<'_> {
        self.0.as_handle()
    }
}

impl Descriptor {
    /// A second descriptor on the same open file or connection.
    ///
    /// # Errors
    /// The system's error duplicating it.
    #[cfg(unix)]
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self::of(self.0.try_clone()?))
    }

    /// A second descriptor on the same open file or connection.
    ///
    /// # Errors
    /// The system's error duplicating it.
    #[cfg(windows)]
    pub fn try_clone(&self) -> io::Result<Self> {
        Ok(Self::of(match &self.0 {
            windows::Held::Pipe(stream) => windows::Held::Pipe(stream.try_clone()?),
            windows::Held::Other(handle) => windows::Held::Other(handle.try_clone()?),
        }))
    }
}
