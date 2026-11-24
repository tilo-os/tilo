//! Descriptors handed over a connected stream, on Windows: none are. A pipe passes no handles,
//! and a handle can be duplicated only into a process this one holds (`packet::windows`), which
//! a peer that dialed a socket is not. So a send is refused, nothing ever arrives, and the
//! stream carries the connection's bytes alone, with the same types as on Unix.

use std::io;
use std::os::windows::io::{BorrowedHandle, OwnedHandle};
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use tilo_socket::tokio::Stream;

/// What a connection hands over: an open handle.
pub type Owned = OwnedHandle;

fn refused() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "a pipe hands no handles to a process that dialed it",
    )
}

#[derive(Clone, Default)]
pub struct DescriptorSender(());

impl DescriptorSender {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Refused: see the module.
    ///
    /// # Errors
    /// Always [`io::ErrorKind::Unsupported`].
    pub fn send(&self, _: Owned) -> io::Result<u32> {
        Err(refused())
    }

    /// Refused: see the module.
    ///
    /// # Errors
    /// Always [`io::ErrorKind::Unsupported`].
    pub fn send_duplicate(&self, _: BorrowedHandle<'_>) -> io::Result<u32> {
        Err(refused())
    }
}

/// What the peer handed over: never anything.
#[derive(Clone)]
pub struct ReceivedDescriptors(());

impl ReceivedDescriptors {
    #[must_use]
    pub const fn new(_limit: usize) -> Self {
        Self(())
    }

    #[must_use]
    pub const fn take(&self, _number: u32) -> Option<Owned> {
        None
    }

    pub fn discard(&self, numbers: impl IntoIterator<Item = u32>) {
        drop(numbers.into_iter());
    }

    #[must_use]
    pub const fn held(&self) -> usize {
        0
    }
}

pub struct DescriptorStream {
    stream: Stream,
}

impl DescriptorStream {
    #[must_use]
    pub const fn sending(stream: Stream, _: DescriptorSender) -> Self {
        Self { stream }
    }

    #[must_use]
    pub const fn receiving(stream: Stream, _: ReceivedDescriptors) -> Self {
        Self { stream }
    }

    #[must_use]
    pub const fn duplex(stream: Stream, _: DescriptorSender, _: ReceivedDescriptors) -> Self {
        Self { stream }
    }
}

impl AsyncRead for DescriptorStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_read(cx, buf)
    }
}

impl AsyncWrite for DescriptorStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.get_mut().stream).poll_write(cx, data)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}
