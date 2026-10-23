//! Descriptors handed over a connected Cap'n Proto stream with `SCM_RIGHTS`.
//!
//! Cap'n Proto here has no descriptor capabilities, so a client that must
//! hand a service an open object sends the descriptor itself alongside the
//! connection's ordinary bytes: [`DescriptorSender::send`] queues it and the
//! connection's next write carries it. Each side numbers the descriptors on
//! one connection in order from zero; the single writer and the ordered stream
//! make both numberings agree, so a request names a descriptor by that number.
//! Every queued descriptor is written no later than the bytes written after
//! it, so a service holds it by the time it reads the request naming it.
//!
//! The peer chooses what it hands over, so no side needs rights over the
//! other's process (`pidfd_getfd` needs ptrace-attach rights, which hardened
//! hosts withhold from everyone). A receiver must still verify every
//! descriptor it takes exactly as it would any other peer input.
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, IoSlice, IoSliceMut};
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll, ready};

use rustix::net::{
    RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, SendAncillaryBuffer,
    SendAncillaryMessage, SendFlags,
};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::UnixStream;

/// Descriptors one write carries. Far below the kernel's `SCM_MAX_FD` (253)
/// and within the 2048-byte control buffer every receiver reads with.
const BATCH: usize = 64;

/// The highest descriptor number: requests carry it as a non-negative `i32`.
const LAST_NUMBER: u32 = i32::MAX.unsigned_abs();

#[derive(Default)]
struct Outbound {
    queue: VecDeque<OwnedFd>,
    next: u32,
}

/// The sending side of one connection's descriptor lane. Clones share it.
#[derive(Clone, Default)]
pub struct DescriptorSender(Arc<Mutex<Outbound>>);

impl DescriptorSender {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Queue `fd` for the connection's next write and return the number the
    /// receiver knows it by.
    pub fn send(&self, fd: OwnedFd) -> io::Result<u32> {
        let mut outbound = self
            .0
            .lock()
            .map_err(|_| io::Error::other("descriptor lane poisoned"))?;
        let number = outbound.next;
        if number > LAST_NUMBER {
            return Err(io::Error::other(
                "this connection's descriptor numbers are spent",
            ));
        }
        outbound.next = number.saturating_add(1);
        outbound.queue.push_back(fd);
        drop(outbound);
        Ok(number)
    }

    /// [`Self::send`] a close-on-exec duplicate of `fd`; the caller keeps its own.
    ///
    /// # Errors
    /// Returns the duplication error, or as [`Self::send`].
    pub fn send_duplicate(&self, fd: BorrowedFd<'_>) -> io::Result<u32> {
        self.send(fd.try_clone_to_owned()?)
    }
}

struct Received {
    limit: usize,
    next: u32,
    pending: BTreeMap<u32, OwnedFd>,
}

/// The receiving side of one connection's descriptor lane: what the peer
/// sent and nobody has taken yet, at most `limit` at a time. A descriptor
/// arriving while the limit is held is closed at once and its number stays
/// empty, so only the request naming it fails.
#[derive(Clone)]
pub struct ReceivedDescriptors(Arc<Mutex<Received>>);

impl ReceivedDescriptors {
    #[must_use]
    pub fn new(limit: usize) -> Self {
        Self(Arc::new(Mutex::new(Received {
            limit,
            next: 0,
            pending: BTreeMap::new(),
        })))
    }

    /// Take the descriptor the peer sent as `number`; each is taken once.
    #[must_use]
    pub fn take(&self, number: u32) -> Option<OwnedFd> {
        self.0.lock().ok()?.pending.remove(&number)
    }

    /// Close whichever of `numbers` are still held.
    pub fn discard(&self, numbers: impl IntoIterator<Item = u32>) {
        if let Ok(mut received) = self.0.lock() {
            for number in numbers {
                received.pending.remove(&number);
            }
        }
    }

    /// How many descriptors are held, untaken.
    #[must_use]
    pub fn held(&self) -> usize {
        self.0.lock().map_or(0, |received| received.pending.len())
    }

    fn admit(&self, fds: Vec<OwnedFd>) -> io::Result<()> {
        let mut received = self
            .0
            .lock()
            .map_err(|_| io::Error::other("descriptor lane poisoned"))?;
        for fd in fds {
            let number = received.next;
            if number > LAST_NUMBER {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "peer sent more descriptors than one connection numbers",
                ));
            }
            received.next = number.saturating_add(1);
            if received.pending.len() < received.limit {
                received.pending.insert(number, fd);
            }
        }
        drop(received);
        Ok(())
    }
}

/// A connected Unix stream that carries a descriptor lane: what it writes
/// carries descriptors queued on its [`DescriptorSender`], and what it reads
/// fills its [`ReceivedDescriptors`]. Without a receiver, descriptors the
/// peer sends are closed on receipt.
pub struct DescriptorStream {
    stream: UnixStream,
    outbound: Option<DescriptorSender>,
    inbound: Option<ReceivedDescriptors>,
}

impl DescriptorStream {
    /// A client end whose writes carry what `sender` queues.
    #[must_use]
    pub const fn sending(stream: UnixStream, sender: DescriptorSender) -> Self {
        Self {
            stream,
            outbound: Some(sender),
            inbound: None,
        }
    }

    /// A service end that keeps what the peer sends in `received`.
    #[must_use]
    pub const fn receiving(stream: UnixStream, received: ReceivedDescriptors) -> Self {
        Self {
            stream,
            outbound: None,
            inbound: Some(received),
        }
    }

    /// Whether a descriptor waits for this stream's next write.
    fn sends_descriptors(&self) -> bool {
        self.outbound.as_ref().is_some_and(|outbound| {
            outbound
                .0
                .lock()
                .map_or(true, |outbound| !outbound.queue.is_empty())
        })
    }

    fn try_send(&self, data: &[u8]) -> io::Result<usize> {
        let outbound = self
            .outbound
            .as_ref()
            .ok_or_else(|| io::Error::other("this stream sends no descriptors"))?;
        let mut outbound = outbound
            .0
            .lock()
            .map_err(|_| io::Error::other("descriptor lane poisoned"))?;
        let count = outbound.queue.len().min(BATCH);
        // Descriptors beyond this batch ride on the following bytes; keep
        // enough of them back, one byte per remaining batch.
        let data = if outbound.queue.len() > BATCH {
            data.get(..1).unwrap_or(data)
        } else {
            data
        };
        let sent = {
            let fds: Vec<BorrowedFd<'_>> =
                outbound.queue.iter().take(count).map(AsFd::as_fd).collect();
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(BATCH))];
            let mut control = SendAncillaryBuffer::new(&mut space);
            if !control.push(SendAncillaryMessage::ScmRights(&fds)) {
                return Err(io::Error::other("descriptor batch exceeds its buffer"));
            }
            #[cfg(any(target_os = "linux", target_os = "android"))]
            let flags = SendFlags::NOSIGNAL;
            #[cfg(not(any(target_os = "linux", target_os = "android")))]
            let flags = SendFlags::empty();
            rustix::net::sendmsg(&self.stream, &[IoSlice::new(data)], &mut control, flags)?
        };
        // The kernel holds its own references now.
        outbound.queue.drain(..count);
        drop(outbound);
        Ok(sent)
    }

    fn try_receive(
        inbound: &ReceivedDescriptors,
        stream: &UnixStream,
        buffer: &mut [u8],
    ) -> io::Result<usize> {
        let mut space = [MaybeUninit::uninit(); 2048];
        let mut control = RecvAncillaryBuffer::new(&mut space);
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let flags = RecvFlags::CMSG_CLOEXEC;
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let flags = RecvFlags::empty();
        let received =
            rustix::net::recvmsg(stream, &mut [IoSliceMut::new(buffer)], &mut control, flags)?;
        let mut fds = Vec::new();
        for message in control.drain() {
            if let RecvAncillaryMessage::ScmRights(rights) = message {
                fds.extend(rights);
            }
        }
        if received.flags.contains(ReturnFlags::CTRUNC) {
            // The kernel closed descriptors it could not deliver, so the
            // numbering would no longer match the peer's.
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "peer descriptors were truncated",
            ));
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        for fd in &fds {
            rustix::io::fcntl_setfd(fd, rustix::io::FdFlags::CLOEXEC)?;
        }
        inbound.admit(fds)?;
        Ok(received.bytes)
    }
}

impl AsyncRead for DescriptorStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        let Some(inbound) = &this.inbound else {
            // A plain read closes any descriptor the peer sent.
            return Pin::new(&mut this.stream).poll_read(cx, buf);
        };
        loop {
            ready!(this.stream.poll_read_ready(cx))?;
            let unfilled = buf.initialize_unfilled();
            match this.stream.try_io(tokio::io::Interest::READABLE, || {
                Self::try_receive(inbound, &this.stream, unfilled)
            }) {
                Ok(read) => {
                    buf.advance(read);
                    return Poll::Ready(Ok(()));
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Poll::Ready(Err(error)),
            }
        }
    }
}

impl AsyncWrite for DescriptorStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        data: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if data.is_empty() || !this.sends_descriptors() {
            // A descriptor queued after this check rides on a later write,
            // still ahead of the request naming it, which follows its queueing.
            return Pin::new(&mut this.stream).poll_write(cx, data);
        }
        loop {
            ready!(this.stream.poll_write_ready(cx))?;
            match this
                .stream
                .try_io(tokio::io::Interest::WRITABLE, || this.try_send(data))
            {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                result => return Poll::Ready(result),
            }
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().stream).poll_shutdown(cx)
    }
}

#[cfg(test)]
#[path = "descriptor_lane/tests.rs"]
mod tests;
