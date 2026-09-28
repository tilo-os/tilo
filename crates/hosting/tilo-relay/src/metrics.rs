//! Cheap per-workspace counters — accessible via `tilo-relay status` and,
//! when the config sets `metrics_listen`, as Prometheus text on
//! `GET /metrics` ([`serve_prometheus`]).
//!
//! These are `AtomicU64` counters; no peer observes them. Keeping them
//! lock-free means the hot path (Delta fan-out) does not contend on the
//! metrics mutex.

use std::fmt::Write as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

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
    /// Peer sessions ended for a protocol breach or a store failure.
    peer_sessions_ended: AtomicU64,
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

    /// Bump the ended-session counter.
    pub fn inc_peer_sessions_ended(&self) {
        self.peer_sessions_ended.fetch_add(1, Ordering::Relaxed);
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
            peer_sessions_ended: self.peer_sessions_ended.load(Ordering::Relaxed),
        }
    }
}

/// The persisted counter schema, shared with status readers.
pub use tilo_relay_protocol::metrics::MetricsSnapshot;

/// The live counters and gauges in the Prometheus text exposition format.
#[must_use]
pub fn render_prometheus(snapshot: &MetricsSnapshot, live_connections: usize) -> String {
    let counters: [(&str, &str, u64); 12] = [
        (
            "tilo_relay_connections_accepted_total",
            "Connections accepted since start, before authentication.",
            snapshot.conns_accepted,
        ),
        (
            "tilo_relay_connections_authenticated_total",
            "Connections that completed the certificate handshake.",
            snapshot.conns_authed,
        ),
        (
            "tilo_relay_auth_rejected_total",
            "Connections refused by authentication (bad, foreign or revoked certificate).",
            snapshot.auth_rejected,
        ),
        (
            "tilo_relay_deltas_received_total",
            "Room deltas received across every workspace.",
            snapshot.deltas_received,
        ),
        (
            "tilo_relay_deltas_fanned_out_total",
            "Room deltas delivered to live subscribers.",
            snapshot.deltas_fanned_out,
        ),
        (
            "tilo_relay_bytes_queued_total",
            "Delta bytes queued on disk for offline peers.",
            snapshot.bytes_queued,
        ),
        (
            "tilo_relay_subscribes_total",
            "Workspace subscriptions handled, each replayed and answered Ready.",
            snapshot.subscribes,
        ),
        (
            "tilo_relay_replayed_retained_total",
            "Retained checkpoint and tail envelopes replayed to subscribers.",
            snapshot.replayed_retained,
        ),
        (
            "tilo_relay_replayed_pending_total",
            "Per-device pending envelopes replayed to reconnecting peers.",
            snapshot.replayed_pending,
        ),
        (
            "tilo_relay_subscriber_dropped_total",
            "Peer sessions that ended with an error, for any reason.",
            snapshot.subscriber_dropped,
        ),
        (
            "tilo_relay_subscriber_dropped_backpressure_total",
            "Peer sessions ended because their outbound queue overflowed.",
            snapshot.subscriber_dropped_backpressure,
        ),
        (
            "tilo_relay_peer_sessions_ended_total",
            "Peer sessions ended for a protocol breach or a store failure.",
            snapshot.peer_sessions_ended,
        ),
    ];
    let mut out = String::new();
    for (name, help, value) in counters {
        let _ = writeln!(
            out,
            "# HELP {name} {help}\n# TYPE {name} counter\n{name} {value}"
        );
    }
    let _ = writeln!(
        out,
        "# HELP tilo_relay_live_connections Authenticated relay connections open now.\n\
         # TYPE tilo_relay_live_connections gauge\ntilo_relay_live_connections {live_connections}"
    );
    let _ = writeln!(
        out,
        "# HELP tilo_relay_build_info The running relay's version.\n\
         # TYPE tilo_relay_build_info gauge\ntilo_relay_build_info{{version=\"{}\"}} 1",
        crate::VERSION
    );
    out
}

/// The longest request head the metrics endpoint reads before answering.
const MAX_REQUEST_HEAD: usize = 8 * 1024;

/// Serve [`render_prometheus`] on `GET /metrics` to every connection
/// `listener` accepts, until the task is aborted. Anything else is a 404.
/// Plain HTTP/1.1, one response per connection: a scraper is the only client.
pub async fn serve_prometheus(
    listener: tokio::net::TcpListener,
    metrics: Arc<Metrics>,
    live_connections: Arc<AtomicUsize>,
) {
    loop {
        let Ok((mut stream, _)) = listener.accept().await else {
            tokio::time::sleep(Duration::from_millis(100)).await;
            continue;
        };
        let metrics = Arc::clone(&metrics);
        let live_connections = Arc::clone(&live_connections);
        tokio::spawn(async move {
            let mut head = Vec::with_capacity(512);
            let read = tokio::time::timeout(Duration::from_secs(5), async {
                let mut buffer = [0_u8; 1024];
                while !head.windows(4).any(|window| window == b"\r\n\r\n")
                    && head.len() < MAX_REQUEST_HEAD
                {
                    let n = stream.read(&mut buffer).await?;
                    if n == 0 {
                        break;
                    }
                    head.extend_from_slice(buffer.get(..n).unwrap_or_default());
                }
                Ok::<(), std::io::Error>(())
            })
            .await;
            if !matches!(read, Ok(Ok(()))) {
                return;
            }
            let response =
                if head.starts_with(b"GET /metrics ") || head.starts_with(b"GET /metrics?") {
                    let body = render_prometheus(
                        &metrics.snapshot(),
                        live_connections.load(Ordering::Relaxed),
                    );
                    format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/plain; version=0.0.4; \
                         charset=utf-8\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    )
                } else {
                    "HTTP/1.1 404 Not Found\r\ncontent-length: 0\r\nconnection: close\r\n\r\n"
                        .to_string()
                };
            let _ = stream.write_all(response.as_bytes()).await;
            let _ = stream.shutdown().await;
        });
    }
}

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

    #[test]
    fn prometheus_text_names_every_counter_once() {
        let m = Metrics::default();
        m.inc_conns_accepted();
        m.add_bytes_queued(42);
        let text = render_prometheus(&m.snapshot(), 3);
        assert!(text.contains("\ntilo_relay_connections_accepted_total 1\n"));
        assert!(text.contains("\ntilo_relay_bytes_queued_total 42\n"));
        assert!(text.contains("\ntilo_relay_live_connections 3\n"));
        assert!(text.contains("# TYPE tilo_relay_auth_rejected_total counter\n"));
        for line in text.lines().filter(|line| !line.starts_with('#')) {
            let (name, value) = line.rsplit_once(' ').unwrap();
            assert!(name.starts_with("tilo_relay_"), "{line}");
            value.parse::<u64>().unwrap();
        }
    }

    #[tokio::test]
    async fn the_endpoint_answers_metrics_and_nothing_else() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let metrics = Arc::new(Metrics::default());
        metrics.inc_conns_authed();
        let server = tokio::spawn(serve_prometheus(
            listener,
            Arc::clone(&metrics),
            Arc::new(AtomicUsize::new(0)),
        ));
        let fetch = |path: &'static str| async move {
            let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
            stream
                .write_all(format!("GET {path} HTTP/1.1\r\nhost: relay\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            response
        };
        let ok = fetch("/metrics").await;
        assert!(ok.starts_with("HTTP/1.1 200 OK\r\n"), "{ok}");
        assert!(ok.contains("text/plain; version=0.0.4"));
        assert!(ok.contains("\ntilo_relay_connections_authenticated_total 1\n"));
        let missing = fetch("/").await;
        assert!(missing.starts_with("HTTP/1.1 404 "), "{missing}");
        server.abort();
    }
}
