//! Cheap per-workspace counters — accessible via
//! `tilo-relay-admin status` and eventually a Prometheus endpoint.
//!
//! These are `AtomicU64` counters with no external client; a peer
//! only observes them if it explicitly asks (`admin status`).
//! Keeping them lock-free means the hot path (Delta fan-out) does
//! not contend on the metrics mutex.

use std::sync::atomic::{AtomicU64, Ordering};

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
        }
    }
}

/// The persisted counter schema, shared with status readers.
pub use tilo_relay_protocol::metrics::MetricsSnapshot;

#[cfg(test)]
mod tests {
    use super::*;

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
}
