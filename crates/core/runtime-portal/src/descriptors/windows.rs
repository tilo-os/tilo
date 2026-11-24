//! Descriptors handed over a connected stream, on Windows: handles, moved by the side that holds
//! the other process (`crate::handles`), with the same types as on Unix.
//!
//! A pipe carries bytes alone, so a handle is named in a request by its value in one of the two
//! processes, and that value is the number the request names it by. The server end of a
//! connection, where the daemon accepted it, holds the process that connected when it is one the
//! daemon started or one of this account's own programs: it duplicates what it sends into that
//! process and sends the value it has there, and it takes the handle a request names out of that
//! process when it takes the number. The connecting process gives up a copy of each handle it
//! sends, numbered by its value, and owns the values the server sends it. Nothing crosses until a
//! number is taken, so the bytes need no framing and a peer that sends none reads as on Unix.
//!
//! A server end whose peer it does not hold sends no handles and takes none: a send is refused,
//! and every number reads as nothing.

use std::io;
use std::os::windows::io::{AsHandle as _, BorrowedHandle, IntoRawHandle as _, OwnedHandle};
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use tilo_socket::tokio::Stream;

use crate::handles;

/// What a connection hands over: an open handle.
pub type Owned = OwnedHandle;

/// How handles move on one connection.
#[derive(Clone, Debug)]
enum Side {
    /// Not on a connection yet.
    Unbound,
    /// The server end, holding the process that connected.
    Holds(Arc<handles::Peer>),
    /// The connecting end: the server holds this process.
    Held,
    /// A server end whose peer this process does not hold, and why.
    Refused(String),
}

impl Side {
    /// How handles move on `stream`.
    fn of(stream: &Stream) -> Self {
        if !stream.is_server_end() {
            return Self::Held;
        }
        match tilo_socket::tokio::peer_pid(stream).and_then(handles::Peer::of) {
            Ok(peer) => Self::Holds(Arc::new(peer)),
            Err(error) => Self::Refused(error.to_string()),
        }
    }
}

/// A handle number as a request carries it: a handle's value fits, as Windows keeps every value
/// within 32 bits for processes of either width.
fn number(value: u64) -> io::Result<u32> {
    u32::try_from(value)
        .map_err(|_| io::Error::other("a handle's value is wider than a descriptor number"))
}

fn shared<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

#[derive(Clone)]
pub struct DescriptorSender(Arc<Mutex<Side>>);

impl Default for DescriptorSender {
    fn default() -> Self {
        Self(Arc::new(Mutex::new(Side::Unbound)))
    }
}

impl DescriptorSender {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Hand `handle` over on the connection and return the number the receiver knows it by.
    ///
    /// # Errors
    /// The sender is on no connection yet, its server end does not hold the peer, or the handle
    /// could not be duplicated.
    pub fn send(&self, handle: Owned) -> io::Result<u32> {
        let side = shared(&self.0).clone();
        let value = match &side {
            Side::Holds(peer) => peer.push(handle.as_handle())?,
            // The receiver closes it here as it takes it.
            Side::Held => handle.into_raw_handle() as usize as u64,
            Side::Unbound => {
                return Err(io::Error::other(
                    "a descriptor sender hands nothing before its connection is made",
                ));
            }
            Side::Refused(why) => {
                return Err(io::Error::new(io::ErrorKind::PermissionDenied, why.clone()));
            }
        };
        number(value).inspect_err(|_| match &side {
            Side::Holds(peer) => peer.close(&[value]),
            _ => handles::unrelease(&[value]),
        })
    }

    /// [`Self::send`] a duplicate of `handle`; the caller keeps its own.
    ///
    /// # Errors
    /// As [`Self::send`], or the duplication's error.
    pub fn send_duplicate(&self, handle: BorrowedHandle<'_>) -> io::Result<u32> {
        self.send(handle.try_clone_to_owned()?)
    }
}

struct Received {
    side: Side,
    /// Whether the connection takes handles at all.
    takes: bool,
}

/// The receiving side of one connection's handles: the handle a number names is taken out of the
/// peer (or owned, when the peer pushed it here) when a request names it, and a number is named
/// once. Nothing is held in between, so `limit` only says whether any is taken: with none, every
/// number reads as nothing and the peer's handle stays its own.
#[derive(Clone)]
pub struct ReceivedDescriptors(Arc<Mutex<Received>>);

impl ReceivedDescriptors {
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self(Arc::new(Mutex::new(Received {
            side: Side::Unbound,
            takes: limit > 0,
        })))
    }

    #[must_use]
    pub fn take(&self, number: u32) -> Option<Owned> {
        let received = shared(&self.0);
        if !received.takes {
            return None;
        }
        match &received.side {
            Side::Holds(peer) => peer.pull(u64::from(number)).ok(),
            // SAFETY: the server end, which holds this process, pushed this number into it for
            // this connection, and it is taken once.
            Side::Held => unsafe { handles::pushed(u64::from(number)) },
            Side::Unbound | Side::Refused(_) => None,
        }
    }

    /// Close the handles `numbers` name, which no request took.
    pub fn discard(&self, numbers: impl IntoIterator<Item = u32>) {
        let received = shared(&self.0);
        if !received.takes {
            return;
        }
        let numbers: Vec<u64> = numbers.into_iter().map(u64::from).collect();
        match &received.side {
            Side::Holds(peer) => peer.close(&numbers),
            Side::Held => {
                for value in numbers {
                    // SAFETY: as in `take`: pushed for this connection, closed once.
                    drop(unsafe { handles::pushed(value) });
                }
            }
            Side::Unbound | Side::Refused(_) => {}
        }
    }

    /// None is held here: a handle stays the peer's until its number is taken.
    #[must_use]
    pub const fn held(&self) -> usize {
        0
    }
}

pub struct DescriptorStream {
    stream: Stream,
    /// What the stream sends and takes handles by, bound to its peer for as long as it lives.
    _sender: Option<DescriptorSender>,
    _received: Option<ReceivedDescriptors>,
}

impl DescriptorStream {
    fn bound(
        stream: Stream,
        sender: Option<DescriptorSender>,
        received: Option<ReceivedDescriptors>,
    ) -> Self {
        let side = Side::of(&stream);
        if let Some(sender) = &sender {
            *shared(&sender.0) = side.clone();
        }
        if let Some(received) = &received {
            shared(&received.0).side = side;
        }
        Self {
            stream,
            _sender: sender,
            _received: received,
        }
    }

    #[must_use]
    pub fn sending(stream: Stream, sender: DescriptorSender) -> Self {
        Self::bound(stream, Some(sender), None)
    }

    #[must_use]
    pub fn receiving(stream: Stream, received: ReceivedDescriptors) -> Self {
        Self::bound(stream, None, Some(received))
    }

    /// A stream that carries handles both ways: what `sender` sends, and what the peer numbers,
    /// taken from `received`.
    #[must_use]
    pub fn duplex(stream: Stream, sender: DescriptorSender, received: ReceivedDescriptors) -> Self {
        Self::bound(stream, Some(sender), Some(received))
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
