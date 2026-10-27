//! The session's wire, shared by its writer and its heartbeat answers.
//!
//! A heartbeat proves the connection's reader and writer are alive and
//! nothing more: no client takes a `Pong` as an acknowledgement of what it
//! sent before. So a `Pong` must never wait for anything the writer waits
//! for: a live frame's authorization under the room's lock, a replay's
//! reads of the store, a state run. Queued behind those, a slow disk held
//! it past the client's silence limit, and every member reconnected at once
//! and waited again.
//!
//! The writer still writes every frame it starts whole: [`WireWriter`]
//! takes each frame entire into a buffer the first time it is offered and
//! answers only once the whole frame is on the wire. Owed `Pong`s are
//! appended to that buffer from [`Wire::answer_heartbeats`], which runs
//! beside the writer, so they always land between two frames and go out
//! while the writer awaits anything at all.

use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll, ready};

use tilo_lock::MutexRecover as _;

use crate::peer::{Envelope, EnvelopeV2, PeerError};

/// A buffer this large is given back once its frame is written.
const KEEP_BUFFER: usize = 1 << 20;

/// Heartbeats the reader has read and the wire not yet answered.
#[derive(Default)]
pub(super) struct Heartbeats {
    owed: AtomicUsize,
    wake: tokio::sync::Notify,
}

impl Heartbeats {
    /// One more `Ping` to answer, at once.
    pub(super) fn owe(&self) {
        self.owed.fetch_add(1, Ordering::SeqCst);
        self.wake.notify_one();
    }
}

struct WireState<W> {
    io: W,
    /// Bytes accepted for the wire and not yet written, whole frames only.
    frame: Vec<u8>,
    written: usize,
    /// The length of the writer's frame in `frame`, answered once written.
    accepted: Option<usize>,
}

impl<W: tokio::io::AsyncWrite + Unpin> WireState<W> {
    /// Write everything accepted.
    fn drive(&mut self, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        while self.written < self.frame.len() {
            let rest = self.frame.get(self.written..).unwrap_or_default();
            match ready!(Pin::new(&mut self.io).poll_write(cx, rest)) {
                Ok(0) => return Poll::Ready(Err(std::io::ErrorKind::WriteZero.into())),
                Ok(n) => self.written = self.written.saturating_add(n),
                Err(error) => return Poll::Ready(Err(error)),
            }
        }
        self.written = 0;
        if self.frame.capacity() > KEEP_BUFFER {
            self.frame = Vec::new();
        } else {
            self.frame.clear();
        }
        Poll::Ready(Ok(()))
    }
}

/// The wire of one session.
pub(super) struct Wire<W> {
    state: Mutex<WireState<W>>,
}

impl<W: tokio::io::AsyncWrite + Unpin> Wire<W> {
    pub(super) const fn new(io: W) -> Self {
        Self {
            state: Mutex::new(WireState {
                io,
                frame: Vec::new(),
                written: 0,
                accepted: None,
            }),
        }
    }

    /// The writer's handle on the wire.
    pub(super) const fn writer(&self) -> WireWriter<'_, W> {
        WireWriter(self)
    }

    /// Read the underlying stream.
    pub(super) fn io<T>(&self, read: impl FnOnce(&W) -> T) -> T {
        read(&self.state.lock_recover().io)
    }

    /// Answer every heartbeat `heartbeats` owes as soon as it is owed, between
    /// two of the writer's frames. Runs until the wire fails.
    pub(super) async fn answer_heartbeats(&self, heartbeats: &Heartbeats) -> Result<(), PeerError> {
        let pong =
            tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::State(Envelope::Pong))?;
        loop {
            let owed = heartbeats.owed.swap(0, Ordering::SeqCst);
            if owed == 0 {
                heartbeats.wake.notified().await;
                continue;
            }
            {
                let mut state = self.state.lock_recover();
                for _ in 0..owed {
                    state.frame.extend_from_slice(&pong);
                }
            }
            std::future::poll_fn(|cx| self.state.lock_recover().drive(cx)).await?;
        }
    }
}

/// The writer's handle on a [`Wire`]: every `write_all` it is given is one
/// whole frame, written whole.
pub(super) struct WireWriter<'w, W>(&'w Wire<W>);

impl<W: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for WireWriter<'_, W> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let mut state = self.0.state.lock_recover();
        if state.accepted.is_none() {
            // A write that returned pending is offered again unchanged: only
            // a new one is taken.
            state.frame.extend_from_slice(buf);
            state.accepted = Some(buf.len());
        }
        let driven = ready!(state.drive(cx));
        let accepted = state.accepted.take().unwrap_or_default();
        drop(state);
        Poll::Ready(driven.map(|()| accepted))
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let mut state = self.0.state.lock_recover();
        ready!(state.drive(cx))?;
        Pin::new(&mut state.io).poll_flush(cx)
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let mut state = self.0.state.lock_recover();
        ready!(state.drive(cx))?;
        Pin::new(&mut state.io).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    #[tokio::test]
    async fn an_owed_pong_goes_out_between_frames_while_the_writer_waits() {
        let (client, mut server) = tokio::io::duplex(64);
        let wire = Wire::new(client);
        let heartbeats = Heartbeats::default();
        let mut writer = wire.writer();
        let frame = vec![7_u8; 200];
        let pong = tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::State(Envelope::Pong))
            .unwrap();
        let writing = async {
            writer.write_all(&frame).await.unwrap();
            // The writer now waits on something else for as long as it likes.
            std::future::pending::<()>().await;
        };
        heartbeats.owe();
        let mut read = vec![0_u8; frame.len() + pong.len()];
        tokio::select! {
            biased;
            () = writing => unreachable!(),
            result = wire.answer_heartbeats(&heartbeats) => panic!("the wire failed: {result:?}"),
            result = server.read_exact(&mut read) => { result.unwrap(); }
        }
        // The frame in progress was written whole, then the Pong.
        assert_eq!(read.get(..frame.len()), Some(frame.as_slice()));
        assert_eq!(read.get(frame.len()..), Some(pong.as_slice()));
    }
}
