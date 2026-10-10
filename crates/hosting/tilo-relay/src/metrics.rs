//! Cheap per-workspace counters — accessible via
//! `tilo-relay-admin status` and eventually a Prometheus endpoint.
//!
//! These are `AtomicU64` counters with no external client; a peer
//! only observes them if it explicitly asks (`admin status`).
//! Keeping them lock-free means the hot path (Delta fan-out) does
//! not contend on the metrics mutex.
//!
//! Egress is counted three ways, all on the frames a peer session writes
//! (framed bytes before QUIC, so transport overhead is not included):
//!
//! - in total and for live fan-out ([`MetricsSnapshot::bytes_sent`],
//!   [`MetricsSnapshot::bytes_sent_live`]);
//! - per relay room, for the frames that belong to one
//!   ([`Metrics::room_egress`]): a room's counters are created on its first
//!   frame and read under a shared lock, so writers never wait on each
//!   other;
//! - per connected session ([`Metrics::session_egress`]), by the session's
//!   [`EgressWrite`], which wraps its stream.

use std::collections::{BTreeMap, HashMap};
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, RwLock};
use std::task::{Context, Poll};

/// Coarse global metrics for the daemon.
#[derive(Default, Debug)]
pub struct Metrics {
    /// Connections accepted since start (pre-auth).
    conns_accepted: AtomicU64,
    /// Connections that completed the auth handshake successfully.
    conns_authed: AtomicU64,
    /// Connections rejected by auth (bad cert, wrong user, revoked).
    auth_rejected: AtomicU64,
    /// Total deltas seen (across all workspaces).
    deltas_received: AtomicU64,
    /// Logical framed-envelope bytes newly written to retained/pending storage.
    /// Counts shared retention once and excludes metadata, snapshots, audit,
    /// duplicate retained retries, and physical filesystem allocation.
    bytes_queued: AtomicU64,
    /// Total deltas fanned out to live peers.
    deltas_fanned_out: AtomicU64,
    /// `Subscribe` envelopes handled (each one replays and answers `Ready`).
    subscribes: AtomicU64,
    /// Retained baseline envelopes (checkpoints and tails) written as replay.
    replayed_retained: AtomicU64,
    /// Per-device pending envelopes written as replay.
    replayed_pending: AtomicU64,
    /// Peer sessions that ended with an error (any reason).
    subscriber_dropped: AtomicU64,
    /// Peer sessions ended because their outbound queue overflowed.
    subscriber_dropped_backpressure: AtomicU64,
    /// Bytes written to peer sessions, all of them.
    bytes_sent: AtomicU64,
    /// Bytes of live fan-out frames written to peer sessions.
    bytes_sent_live: AtomicU64,
    /// Egress per relay room.
    rooms: RwLock<HashMap<u64, RoomCounters>>,
    /// Egress per connected session, keyed by a serial number.
    sessions: Mutex<HashMap<u64, Arc<SessionCounters>>>,
    /// Serial number of the next session.
    next_session: AtomicU64,
}

#[derive(Default, Debug)]
struct RoomCounters {
    bytes_sent: AtomicU64,
    bytes_sent_live: AtomicU64,
}

impl RoomCounters {
    fn add(&self, bytes: u64, live: bool) {
        self.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
        if live {
            self.bytes_sent_live.fetch_add(bytes, Ordering::Relaxed);
        }
    }
}

#[derive(Debug)]
struct SessionCounters {
    device_public_key: [u8; 32],
    bytes_sent: AtomicU64,
    writes: AtomicU64,
}

/// Bytes one connected peer session has been sent so far.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SessionEgress {
    /// The session's authenticated device.
    pub device_public_key: [u8; 32],
    /// Bytes written to the session's stream since it was authenticated.
    pub bytes_sent: u64,
    /// Writes that put those bytes on the stream. Each is handed to QUIC
    /// whole, so frames written together can share a packet.
    pub writes: u64,
}

impl Metrics {
    /// Bump the accepted-connection counter.
    pub fn inc_conns_accepted(&self) {
        self.conns_accepted.fetch_add(1, Ordering::Relaxed);
    }
    /// Bump the auth-success counter.
    pub fn inc_conns_authed(&self) {
        self.conns_authed.fetch_add(1, Ordering::Relaxed);
    }
    /// Bump the auth-rejection counter.
    pub fn inc_auth_rejected(&self) {
        self.auth_rejected.fetch_add(1, Ordering::Relaxed);
    }
    /// Bump the delta-receive counter.
    pub fn inc_deltas_received(&self) {
        self.deltas_received.fetch_add(1, Ordering::Relaxed);
    }
    /// Add to the queued-bytes counter.
    pub fn add_bytes_queued(&self, n: u64) {
        self.bytes_queued.fetch_add(n, Ordering::Relaxed);
    }
    /// Add to the fanned-out counter.
    pub fn add_deltas_fanned_out(&self, n: u64) {
        self.deltas_fanned_out.fetch_add(n, Ordering::Relaxed);
    }
    /// Bump the subscribe counter.
    pub fn inc_subscribes(&self) {
        self.subscribes.fetch_add(1, Ordering::Relaxed);
    }
    /// Add retained baseline envelopes replayed to a subscriber.
    pub fn add_replayed_retained(&self, n: u64) {
        self.replayed_retained.fetch_add(n, Ordering::Relaxed);
    }
    /// Add pending-queue envelopes replayed to a subscriber.
    pub fn add_replayed_pending(&self, n: u64) {
        self.replayed_pending.fetch_add(n, Ordering::Relaxed);
    }
    /// Bump the dropped-subscriber counter (any error reason).
    pub fn inc_subscriber_dropped(&self) {
        self.subscriber_dropped.fetch_add(1, Ordering::Relaxed);
    }
    /// Bump the backpressure-drop counter (a subset of `subscriber_dropped`).
    pub fn inc_subscriber_dropped_backpressure(&self) {
        self.subscriber_dropped_backpressure
            .fetch_add(1, Ordering::Relaxed);
    }

    /// Charge a frame of `bytes` that a session is about to write to `room`'s
    /// egress; `live` marks live fan-out (a delta or presence forwarded as it
    /// arrived), as opposed to replay, replies and positions. The session's
    /// [`EgressWrite`] counts the same bytes into the total.
    pub fn add_room_egress(&self, room: u64, bytes: u64, live: bool) {
        if live {
            self.bytes_sent_live.fetch_add(bytes, Ordering::Relaxed);
        }
        {
            let rooms = self.rooms.read().unwrap_or_else(PoisonError::into_inner);
            if let Some(counters) = rooms.get(&room) {
                counters.add(bytes, live);
                return;
            }
        }
        self.rooms
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .entry(room)
            .or_default()
            .add(bytes, live);
    }

    /// Egress per relay room since start, for rooms that were sent anything.
    pub fn room_egress(&self) -> BTreeMap<u64, RoomEgress> {
        self.rooms
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .map(|(room, counters)| {
                (
                    *room,
                    RoomEgress {
                        bytes_sent: counters.bytes_sent.load(Ordering::Relaxed),
                        bytes_sent_live: counters.bytes_sent_live.load(Ordering::Relaxed),
                    },
                )
            })
            .collect()
    }

    /// Egress of every peer session connected now.
    pub fn session_egress(&self) -> Vec<SessionEgress> {
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .values()
            .map(|session| SessionEgress {
                device_public_key: session.device_public_key,
                bytes_sent: session.bytes_sent.load(Ordering::Relaxed),
                writes: session.writes.load(Ordering::Relaxed),
            })
            .collect()
    }

    fn open_session(self: &Arc<Self>, device_public_key: [u8; 32]) -> SessionLease {
        let id = self.next_session.fetch_add(1, Ordering::Relaxed);
        let counters = Arc::new(SessionCounters {
            device_public_key,
            bytes_sent: AtomicU64::new(0),
            writes: AtomicU64::new(0),
        });
        self.sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(id, counters.clone());
        SessionLease {
            metrics: self.clone(),
            id,
            counters,
        }
    }

    /// Read a snapshot of every counter (for `admin status`).
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            conns_accepted: self.conns_accepted.load(Ordering::Relaxed),
            conns_authed: self.conns_authed.load(Ordering::Relaxed),
            auth_rejected: self.auth_rejected.load(Ordering::Relaxed),
            deltas_received: self.deltas_received.load(Ordering::Relaxed),
            bytes_queued: self.bytes_queued.load(Ordering::Relaxed),
            deltas_fanned_out: self.deltas_fanned_out.load(Ordering::Relaxed),
            subscribes: self.subscribes.load(Ordering::Relaxed),
            replayed_retained: self.replayed_retained.load(Ordering::Relaxed),
            replayed_pending: self.replayed_pending.load(Ordering::Relaxed),
            subscriber_dropped: self.subscriber_dropped.load(Ordering::Relaxed),
            subscriber_dropped_backpressure: self
                .subscriber_dropped_backpressure
                .load(Ordering::Relaxed),
            bytes_sent: self.bytes_sent.load(Ordering::Relaxed),
            bytes_sent_live: self.bytes_sent_live.load(Ordering::Relaxed),
            rooms: self.room_egress(),
        }
    }
}

/// A session's registration in [`Metrics::session_egress`]; dropping it
/// removes the session.
#[derive(Debug)]
struct SessionLease {
    metrics: Arc<Metrics>,
    id: u64,
    counters: Arc<SessionCounters>,
}

impl Drop for SessionLease {
    fn drop(&mut self) {
        self.metrics
            .sessions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.id);
    }
}

/// A peer session's outbound stream that counts what it writes into the
/// session's and the relay's egress. The session is listed in
/// [`Metrics::session_egress`] while this lives.
#[derive(Debug)]
pub struct EgressWrite<W> {
    inner: W,
    lease: SessionLease,
}

impl<W> EgressWrite<W> {
    /// Wrap `inner`, the stream of `device_public_key`'s session.
    pub fn new(inner: W, metrics: &Arc<Metrics>, device_public_key: [u8; 32]) -> Self {
        Self {
            inner,
            lease: metrics.open_session(device_public_key),
        }
    }

    /// Bytes written through this stream so far.
    pub fn bytes_sent(&self) -> u64 {
        self.lease.counters.bytes_sent.load(Ordering::Relaxed)
    }

    fn count(&self, written: usize) {
        if written == 0 {
            return;
        }
        let written = u64::try_from(written).unwrap_or(u64::MAX);
        self.lease.counters.writes.fetch_add(1, Ordering::Relaxed);
        self.lease
            .counters
            .bytes_sent
            .fetch_add(written, Ordering::Relaxed);
        self.lease
            .metrics
            .bytes_sent
            .fetch_add(written, Ordering::Relaxed);
    }
}

impl<W: tokio::io::AsyncWrite + Unpin> tokio::io::AsyncWrite for EgressWrite<W> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let polled = Pin::new(&mut self.inner).poll_write(cx, buf);
        if let Poll::Ready(Ok(written)) = &polled {
            self.count(*written);
        }
        polled
    }

    fn poll_write_vectored(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bufs: &[std::io::IoSlice<'_>],
    ) -> Poll<std::io::Result<usize>> {
        let polled = Pin::new(&mut self.inner).poll_write_vectored(cx, bufs);
        if let Poll::Ready(Ok(written)) = &polled {
            self.count(*written);
        }
        polled
    }

    fn is_write_vectored(&self) -> bool {
        self.inner.is_write_vectored()
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

/// The persisted counter schema, shared with status readers.
pub use tilo_relay_protocol::metrics::{MetricsSnapshot, RoomEgress};

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;

    #[test]
    fn counters_increment() {
        let m = Metrics::default();
        m.inc_conns_accepted();
        m.inc_conns_accepted();
        m.inc_conns_authed();
        let snap = m.snapshot();
        assert_eq!(snap.conns_accepted, 2);
        assert_eq!(snap.conns_authed, 1);
    }

    #[test]
    fn subscribe_replay_and_drop_counters_increment() {
        let m = Metrics::default();
        m.inc_subscribes();
        m.add_replayed_retained(4);
        m.add_replayed_pending(7);
        m.inc_subscriber_dropped();
        m.inc_subscriber_dropped_backpressure();
        let snap = m.snapshot();
        assert_eq!(snap.subscribes, 1);
        assert_eq!(snap.replayed_retained, 4);
        assert_eq!(snap.replayed_pending, 7);
        assert_eq!(snap.subscriber_dropped, 1);
        assert_eq!(snap.subscriber_dropped_backpressure, 1);
    }

    #[test]
    fn room_egress_splits_live_fan_out_from_the_rest() {
        let m = Metrics::default();
        m.add_room_egress(7, 100, true);
        m.add_room_egress(7, 30, false);
        m.add_room_egress(8, 5, true);
        let snap = m.snapshot();
        assert_eq!(snap.bytes_sent_live, 105);
        assert_eq!(
            snap.rooms,
            BTreeMap::from([
                (
                    7,
                    RoomEgress {
                        bytes_sent: 130,
                        bytes_sent_live: 100,
                    }
                ),
                (
                    8,
                    RoomEgress {
                        bytes_sent: 5,
                        bytes_sent_live: 5,
                    }
                ),
            ])
        );
        // Room frames are charged before the session writes them; the total
        // is what the sessions' streams actually wrote.
        assert_eq!(snap.bytes_sent, 0);
    }

    #[tokio::test]
    async fn a_session_stream_counts_what_it_writes_while_it_is_connected() {
        let metrics = Arc::new(Metrics::default());
        let mut first = EgressWrite::new(Vec::new(), &metrics, [1; 32]);
        let mut second = EgressWrite::new(Vec::new(), &metrics, [2; 32]);
        first.write_all(b"hello").await.unwrap();
        second.write_all(b"relay frame").await.unwrap();
        first.write_all(b"!").await.unwrap();
        assert_eq!(first.bytes_sent(), 6);
        assert_eq!(metrics.snapshot().bytes_sent, 17);
        let mut sessions = metrics.session_egress();
        sessions.sort_by_key(|session| session.device_public_key);
        assert_eq!(
            sessions,
            vec![
                SessionEgress {
                    device_public_key: [1; 32],
                    bytes_sent: 6,
                    writes: 2,
                },
                SessionEgress {
                    device_public_key: [2; 32],
                    bytes_sent: 11,
                    writes: 1,
                },
            ]
        );
        drop(first);
        assert_eq!(metrics.session_egress().len(), 1, "an ended session leaves");
        assert_eq!(
            metrics.snapshot().bytes_sent,
            17,
            "the relay keeps its total"
        );
    }
}
