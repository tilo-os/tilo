//! The carrier over iroh: one iroh [`Connection`] as a `moq-lite` transport.
//!
//! [`Session`] implements [`moq_net::transport::poll::Session`] over iroh, so
//! everything above [`crate::carrier`] — the admission preface, `moq-lite` and
//! its SETUP, the relay — runs unchanged on a direct connection between two
//! endpoints. WebTransport stays the browser's carrier; a native device speaks
//! this one instead of a browser-issued certificate and route.
//!
//! Datagrams carry voice as they do over WebTransport: no stream waits behind
//! a lost datagram. The connection's statistics map one to one:
//!
//! | [`moq_net::web_transport_trait::Stats`] | iroh [`ConnectionStats`] |
//! | --- | --- |
//! | `bytes_sent` | `udp_tx.bytes` |
//! | `bytes_received` | `udp_rx.bytes` |
//! | `bytes_lost` | `lost_bytes` |
//! | `packets_sent` | `udp_tx.datagrams` |
//! | `packets_received` | `udp_rx.datagrams` |
//! | `packets_lost` | `lost_packets` |
//! | `rtt` | [`Connection::rtt`] of the connection's first path |
//! | `estimated_send_rate` | none this layer reports |
//!
//! A connection reaches `MoQ` only through the same admission preface as a
//! WebTransport session ([`crate::carrier::present`] and `carrier::accept`):
//! the transport is merely open until the host admits it.

use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::Mutex;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use iroh::endpoint::{
    Connection, ConnectionError, PathId, QuicTransportConfig, ReadError, RecvStream, ResetError,
    SendDatagramError, SendStream, StoppedError, VarInt, WriteError,
};
use iroh::protocol::{AcceptError, ProtocolHandler, Router};
use iroh::{Endpoint, EndpointAddr};
use moq_net::web_transport_trait as wt;
use moq_net::web_transport_trait::poll;
use tilo_lock::MutexRecover as _;

use crate::carrier::{self, AdmissionError, PROTOCOL};

/// The ALPN every carrier connection negotiates: the `moq-lite` draft name,
/// which is also the WebTransport subprotocol.
pub const ALPN: &[u8] = PROTOCOL.as_bytes();

/// Bytes of datagrams one connection holds for a session that is slow to
/// read: a jitter buffer, not a call's backlog.
const RECEIVE_BUFFER: usize = 256 * 1024;
/// A carrier connection ends after this long without a packet from the peer.
const IDLE_TIMEOUT: Duration = Duration::from_secs(15);
/// Unidirectional streams one connection accepts at once: a picture stream per
/// source and layer, each with room to start over.
const MAX_UNI_STREAMS: u32 = 64;
/// Bidirectional streams one connection accepts at once: the admission stream
/// and `moq-lite`'s own.
const MAX_BI_STREAMS: u32 = 16;

/// The QUIC transport a carrier's connections need: datagrams enabled, the
/// session's idle timeout, and room for a call's picture streams. An endpoint
/// that carries calls is built with this
/// ([`Endpoint::builder`](iroh::EndpointBuilder::transport_config)).
#[must_use]
pub fn transport_config() -> QuicTransportConfig {
    let idle = u32::try_from(IDLE_TIMEOUT.as_millis()).unwrap_or(u32::MAX);
    QuicTransportConfig::builder()
        .max_idle_timeout(Some(VarInt::from_u32(idle).into()))
        .datagram_receive_buffer_size(Some(RECEIVE_BUFFER))
        .max_concurrent_uni_streams(VarInt::from_u32(MAX_UNI_STREAMS))
        .max_concurrent_bidi_streams(VarInt::from_u32(MAX_BI_STREAMS))
        .build()
}

/// One iroh connection as a `moq-lite` transport.
///
/// Cheap to clone: every clone drives the same connection, and each clone
/// keeps its own in-progress operations, as the poll contract requires.
pub struct Session {
    connection: Connection,
    pending: Mutex<Pending>,
}

/// The operations one [`Session`] clone has in flight. Their futures live
/// here between polls because each iroh future registers its waker when it is
/// polled and removes it when dropped: a future re-created on every poll would
/// miss the notification that ends it.
#[derive(Default)]
struct Pending {
    accept_uni: Option<Op<Result<RecvStream, ConnectionError>>>,
    accept_bi: Option<Op<Result<(SendStream, RecvStream), ConnectionError>>>,
    open_uni: Option<Op<Result<SendStream, ConnectionError>>>,
    open_bi: Option<Op<Result<(SendStream, RecvStream), ConnectionError>>>,
    datagram: Option<Op<Result<Bytes, ConnectionError>>>,
    closed: Option<Op<ConnectionError>>,
}

/// One in-progress iroh operation, owned by the session or stream polling it.
type Op<T> = Pin<Box<dyn Future<Output = T> + Send>>;

/// Poll the operation in `slot`, creating it on first use and clearing it once
/// it completes.
fn poll_op<T>(
    slot: &mut Option<Op<T>>,
    cx: &mut Context<'_>,
    make: impl FnOnce() -> Op<T>,
) -> Poll<T> {
    let future = slot.get_or_insert_with(make);
    let polled = future.as_mut().poll(cx);
    if polled.is_ready() {
        *slot = None;
    }
    polled
}

impl Clone for Session {
    fn clone(&self) -> Self {
        Self {
            connection: self.connection.clone(),
            pending: Mutex::new(Pending::default()),
        }
    }
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("connection", &self.connection)
            .finish_non_exhaustive()
    }
}

impl Session {
    /// The carrier over `connection`, which must have negotiated [`ALPN`].
    #[must_use]
    pub fn new(connection: Connection) -> Self {
        Self {
            connection,
            pending: Mutex::new(Pending::default()),
        }
    }

    /// The iroh connection underneath.
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Who is on the other end, as the handshake proved it: the endpoint id
    /// the connection was admitted from.
    #[must_use]
    pub fn remote(&self) -> [u8; 32] {
        *self.connection.remote_id().as_bytes()
    }
}

impl poll::Session for Session {
    type SendStream = Outgoing;
    type RecvStream = Incoming;
    type Error = NativeError;

    fn poll_accept_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Incoming, NativeError>> {
        let connection = self.connection.clone();
        let mut pending = self.pending.lock_recover();
        poll_op(&mut pending.accept_uni, cx, || {
            Box::pin(async move { connection.accept_uni().await })
        })
        .map(|result| result.map(Incoming::new).map_err(NativeError::Connection))
    }

    fn poll_accept_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<poll::BiStreams<Self>, NativeError>> {
        let connection = self.connection.clone();
        let mut pending = self.pending.lock_recover();
        poll_op(&mut pending.accept_bi, cx, || {
            Box::pin(async move { connection.accept_bi().await })
        })
        .map(|result| result.map(pair).map_err(NativeError::Connection))
    }

    fn poll_open_uni(&mut self, cx: &mut Context<'_>) -> Poll<Result<Outgoing, NativeError>> {
        let connection = self.connection.clone();
        let mut pending = self.pending.lock_recover();
        poll_op(&mut pending.open_uni, cx, || {
            Box::pin(async move { connection.open_uni().await })
        })
        .map(|result| result.map(Outgoing::new).map_err(NativeError::Connection))
    }

    fn poll_open_bi(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Result<poll::BiStreams<Self>, NativeError>> {
        let connection = self.connection.clone();
        let mut pending = self.pending.lock_recover();
        poll_op(&mut pending.open_bi, cx, || {
            Box::pin(async move { connection.open_bi().await })
        })
        .map(|result| result.map(pair).map_err(NativeError::Connection))
    }

    /// A datagram is accepted, handed to QUIC, or refused. iroh drops the
    /// oldest queued datagram rather than blocking the sender, and the
    /// network is allowed to drop it anyway, so this never waits.
    fn poll_send_datagram(
        &mut self,
        _cx: &mut Context<'_>,
        payload: &[u8],
    ) -> Poll<Result<(), NativeError>> {
        Poll::Ready(
            self.connection
                .send_datagram(Bytes::copy_from_slice(payload))
                .map_err(NativeError::Datagram),
        )
    }

    fn poll_recv_datagram(&mut self, cx: &mut Context<'_>) -> Poll<Result<Bytes, NativeError>> {
        let connection = self.connection.clone();
        let mut pending = self.pending.lock_recover();
        poll_op(&mut pending.datagram, cx, || {
            Box::pin(async move { connection.read_datagram().await })
        })
        .map_err(NativeError::Connection)
    }

    fn max_datagram_size(&self) -> usize {
        self.connection.max_datagram_size().unwrap_or(0)
    }

    fn protocol(&self) -> Option<&str> {
        std::str::from_utf8(self.connection.alpn()).ok()
    }

    fn close(&mut self, code: u32, reason: &str) {
        self.connection
            .close(VarInt::from_u32(code), reason.as_bytes());
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<NativeError> {
        let connection = self.connection.clone();
        let mut pending = self.pending.lock_recover();
        poll_op(&mut pending.closed, cx, || {
            Box::pin(async move { connection.closed().await })
        })
        .map(NativeError::Connection)
    }

    fn stats(&self) -> impl wt::Stats {
        self.clone()
    }
}

/// One datagram's worth of the pair `moq-lite` expects.
fn pair((send, recv): (SendStream, RecvStream)) -> poll::BiStreams<Session> {
    (Outgoing::new(send), Incoming::new(recv))
}

/// iroh's connection counters as [`wt::Stats`].
impl wt::Stats for Session {
    fn bytes_sent(&self) -> Option<u64> {
        Some(self.connection.stats().udp_tx.bytes)
    }

    fn bytes_received(&self) -> Option<u64> {
        Some(self.connection.stats().udp_rx.bytes)
    }

    fn bytes_lost(&self) -> Option<u64> {
        Some(self.connection.stats().lost_bytes)
    }

    fn packets_sent(&self) -> Option<u64> {
        Some(self.connection.stats().udp_tx.datagrams)
    }

    fn packets_received(&self) -> Option<u64> {
        Some(self.connection.stats().udp_rx.datagrams)
    }

    fn packets_lost(&self) -> Option<u64> {
        Some(self.connection.stats().lost_packets)
    }

    fn rtt(&self) -> Option<Duration> {
        self.connection.rtt(PathId::ZERO)
    }
}

/// One outgoing iroh stream as a [`poll::SendStream`].
pub struct Outgoing {
    stream: SendStream,
    /// The peer's stop notification, kept between polls for the same reason as
    /// [`Pending`].
    stopped: Option<Op<Result<Option<VarInt>, StoppedError>>>,
}

impl Outgoing {
    fn new(stream: SendStream) -> Self {
        Self {
            stream,
            stopped: None,
        }
    }
}

impl std::fmt::Debug for Outgoing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outgoing")
            .field("stream", &self.stream)
            .finish_non_exhaustive()
    }
}

impl poll::SendStream for Outgoing {
    type Error = NativeError;

    fn poll_write(&mut self, cx: &mut Context<'_>, buf: &[u8]) -> Poll<Result<usize, NativeError>> {
        Pin::new(&mut self.stream)
            .poll_write(cx, buf)
            .map_err(NativeError::Write)
    }

    fn set_priority(&mut self, order: u8) {
        let _ = self.stream.set_priority(i32::from(order));
    }

    /// A stream that is already finished or reset took the finish; there is
    /// nothing left for the caller to do about it.
    fn finish(&mut self) -> Result<(), NativeError> {
        let _ = self.stream.finish();
        Ok(())
    }

    fn reset(&mut self, code: u32) {
        let _ = self.stream.reset(VarInt::from_u32(code));
    }

    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), NativeError>> {
        let stream = &self.stream;
        let stopped = self
            .stopped
            .get_or_insert_with(|| Box::pin(stream.stopped()));
        let polled = stopped.as_mut().poll(cx);
        if polled.is_ready() {
            self.stopped = None;
        }
        polled.map(|result| result.map(|_| ()).map_err(NativeError::Stopped))
    }
}

/// One incoming iroh stream as a [`poll::RecvStream`].
#[derive(Debug)]
pub struct Incoming {
    stream: RecvStream,
}

impl Incoming {
    fn new(stream: RecvStream) -> Self {
        Self { stream }
    }
}

impl poll::RecvStream for Incoming {
    type Error = NativeError;

    fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        dst: &mut [u8],
    ) -> Poll<Result<Option<usize>, NativeError>> {
        // Asking for no bytes is not the end of the stream.
        if dst.is_empty() {
            return Poll::Ready(Ok(Some(0)));
        }
        self.stream.poll_read(cx, dst).map(|result| {
            result
                .map(|read| (read > 0).then_some(read))
                .map_err(NativeError::Read)
        })
    }

    fn stop(&mut self, code: u32) {
        let _ = self.stream.stop(VarInt::from_u32(code));
    }

    /// The reset wait needs no stored future: its waker lives in the
    /// connection's blocked-reader table, not in the future.
    fn poll_closed(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), NativeError>> {
        let reset = self.stream.received_reset();
        pin!(reset)
            .poll(cx)
            .map(|result| result.map(|_| ()).map_err(NativeError::Reset))
    }
}

/// Why a carrier operation on an iroh connection failed.
#[derive(Debug, thiserror::Error)]
pub enum NativeError {
    #[error("connection: {0}")]
    Connection(ConnectionError),
    #[error("write: {0}")]
    Write(WriteError),
    #[error("read: {0}")]
    Read(ReadError),
    #[error("reset: {0}")]
    Reset(ResetError),
    #[error("stopped: {0}")]
    Stopped(StoppedError),
    #[error("datagram: {0}")]
    Datagram(SendDatagramError),
}

impl wt::Error for NativeError {
    fn session_error(&self) -> Option<(u32, String)> {
        match self {
            Self::Connection(error) | Self::Write(WriteError::ConnectionLost(error)) => {
                session_error(error)
            }
            _ => None,
        }
    }

    fn stream_error(&self) -> Option<u32> {
        match self {
            Self::Write(WriteError::Stopped(code)) => u32::try_from(u64::from(*code)).ok(),
            _ => None,
        }
    }
}

/// The application close code and reason of `error`, if it was one.
fn session_error(error: &ConnectionError) -> Option<(u32, String)> {
    let ConnectionError::ApplicationClosed(application) = error else {
        return None;
    };
    Some((
        u32::try_from(u64::from(application.error_code)).unwrap_or(u32::MAX),
        String::from_utf8_lossy(&application.reason).into_owned(),
    ))
}

/// The iroh carrier: the relay's endpoint, dialled from this device's.
#[derive(Clone, Debug)]
pub struct Iroh {
    /// This device's endpoint: every carrier dials from it.
    pub endpoint: Endpoint,
    /// Where the relay of this call is reachable.
    pub addr: EndpointAddr,
}

impl Iroh {
    /// The carrier that dials `addr` from `endpoint`.
    #[must_use]
    pub fn new(endpoint: Endpoint, addr: EndpointAddr) -> Self {
        Self { endpoint, addr }
    }
}

impl carrier::Dial for Iroh {
    fn dial<'a>(
        &'a self,
        grant: &'a [u8],
        publish: Option<moq_net::origin::Consumer>,
        subscribe: Option<moq_net::origin::Producer>,
        timeout: Duration,
    ) -> carrier::Dialling<'a> {
        Box::pin(async move {
            tokio::time::timeout(timeout, async {
                let connection = self
                    .endpoint
                    .connect(self.addr.clone(), ALPN)
                    .await
                    .map_err(|_| AdmissionError::Unavailable)?;
                carrier::start(Session::new(connection), grant, publish, subscribe).await
            })
            .await
            .map_err(|_| AdmissionError::Unavailable)?
        })
    }
}

/// Serve the carrier on `endpoint`: every connection that negotiates [`ALPN`]
/// is handed to `serve` as a [`Session`], and closed when `serve` returns.
/// Dropping the returned router stops accepting.
///
/// # Errors
///
/// Returns a message when `endpoint` cannot accept another protocol.
pub fn serve<S, F>(endpoint: Endpoint, serve: S) -> Result<Router, String>
where
    S: Fn(Session) -> F + Send + Sync + Clone + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    Ok(Router::builder(endpoint)
        .accept(ALPN, Handler { serve })
        .spawn())
}

/// The [`ALPN`] handler alone, for a device that already runs a router: an
/// endpoint accepts one [`ProtocolHandler`] per protocol, so a live plane
/// registers beside the device's other protocols instead of spawning a second
/// router.
#[must_use]
pub fn handler<S, F>(serve: S) -> impl ProtocolHandler
where
    S: Fn(Session) -> F + Send + Sync + Clone + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    Handler { serve }
}

/// One accepted connection's `serve`.
struct Handler<S> {
    serve: S,
}

impl<S> std::fmt::Debug for Handler<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("carrier handler")
    }
}

impl<S, F> ProtocolHandler for Handler<S>
where
    S: Fn(Session) -> F + Send + Sync + Clone + 'static,
    F: Future<Output = ()> + Send + 'static,
{
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        (self.serve)(Session::new(connection)).await;
        Ok(())
    }
}
