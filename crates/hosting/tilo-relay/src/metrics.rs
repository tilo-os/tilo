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
    /// Total delta bytes queued to disk for offline peers.
    bytes_queued: AtomicU64,
    /// Total deltas fanned out to live peers.
    deltas_fanned_out: AtomicU64,
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

    /// Read a snapshot of every counter (for `admin status`).
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            conns_accepted: self.conns_accepted.load(Ordering::Relaxed),
            conns_authed: self.conns_authed.load(Ordering::Relaxed),
            auth_rejected: self.auth_rejected.load(Ordering::Relaxed),
            deltas_received: self.deltas_received.load(Ordering::Relaxed),
            bytes_queued: self.bytes_queued.load(Ordering::Relaxed),
            deltas_fanned_out: self.deltas_fanned_out.load(Ordering::Relaxed),
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
}
