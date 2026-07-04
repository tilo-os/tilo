//! Prometheus metrics + a single-endpoint exposition server.
//!
//! Kept deliberately tiny: a hand-rolled TCP/HTTP/1.1 handler that
//! responds to `GET /metrics` and 404s everything else. Avoids
//! pulling hyper-v1 + hyper-util through the dep graph for a single
//! GET handler that produces text.
//!
//! ## Counters surfaced
//!
//! - `qos_relay_connected_peers` — gauge of currently-connected peers.
//! - `qos_relay_messages_forwarded_total{topic}` — counter, per-topic
//!   gossipsub messages observed (effectively "forwarded" since the
//!   relay's job is mesh participation, not filtering).
//! - `qos_relay_bytes_in_total` / `qos_relay_bytes_out_total` —
//!   approximate transport bytes seen by gossipsub (the metric
//!   reflects payload bytes; libp2p wire framing isn't counted).
//! - `qos_relay_kademlia_queries_total` — counter of Kademlia
//!   query-completed events.
//!
//! The libp2p `metrics` feature also exposes its own internal
//! counters into the same registry — they appear under
//! `libp2p_*`.

use std::net::SocketAddr;
use std::sync::Arc;

use prometheus_client::encoding::text::encode;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::registry::Registry;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Label-set for per-topic counters.
#[derive(Clone, Debug, Hash, PartialEq, Eq, prometheus_client::encoding::EncodeLabelSet)]
pub struct TopicLabels {
    pub topic: String,
}

/// Bundle of relay-owned counters. Shared across the swarm task and
/// the metrics HTTP server via `Arc`.
#[derive(Debug)]
pub struct RelayMetrics {
    pub registry: Registry,
    pub connected_peers: Gauge,
    pub messages_forwarded: Family<TopicLabels, Counter>,
    pub bytes_in: Counter,
    pub bytes_out: Counter,
    pub kademlia_queries: Counter,
}

impl RelayMetrics {
    pub fn new() -> Self {
        let mut registry = Registry::default();

        let connected_peers = Gauge::default();
        registry.register(
            "qos_relay_connected_peers",
            "Currently-connected libp2p peers",
            connected_peers.clone(),
        );

        let messages_forwarded = Family::<TopicLabels, Counter>::default();
        registry.register(
            "qos_relay_messages_forwarded",
            "Gossipsub messages observed on each topic",
            messages_forwarded.clone(),
        );

        let bytes_in = Counter::default();
        registry.register(
            "qos_relay_bytes_in",
            "Approximate gossipsub payload bytes received",
            bytes_in.clone(),
        );

        let bytes_out = Counter::default();
        registry.register(
            "qos_relay_bytes_out",
            "Approximate gossipsub payload bytes broadcast by this node",
            bytes_out.clone(),
        );

        let kademlia_queries = Counter::default();
        registry.register(
            "qos_relay_kademlia_queries",
            "Kademlia outbound query-completed events",
            kademlia_queries.clone(),
        );

        Self {
            registry,
            connected_peers,
            messages_forwarded,
            bytes_in,
            bytes_out,
            kademlia_queries,
        }
    }
}

impl Default for RelayMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Spawn the metrics HTTP server on `addr`. Returns the bound
/// [`SocketAddr`] (useful when `addr.port() == 0` for tests) and a
/// `JoinHandle` for the listener task. The server stays alive until
/// the handle is dropped or aborted.
pub async fn spawn_server(
    addr: SocketAddr,
    metrics: Arc<RelayMetrics>,
) -> std::io::Result<(SocketAddr, tokio::task::JoinHandle<()>)> {
    let listener = TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    let handle = tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _peer)) => {
                    let metrics = metrics.clone();
                    tokio::spawn(async move {
                        if let Err(err) = handle_request(stream, metrics).await {
                            tracing::debug!(?err, "metrics request handler errored");
                        }
                    });
                }
                Err(err) => {
                    tracing::warn!(?err, "metrics listener accept failed");
                    // Brief back-off so a torrent of accept errors doesn't
                    // pin a CPU.
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    });
    Ok((bound, handle))
}

/// Minimal HTTP/1.1 request handler. Reads bytes until the
/// double-CRLF, parses the request line, dispatches.
async fn handle_request(
    mut stream: tokio::net::TcpStream,
    metrics: Arc<RelayMetrics>,
) -> std::io::Result<()> {
    // Read up to 4 KiB looking for the request line + headers. That's
    // ample for a GET with no body.
    let mut buf = [0u8; 4096];
    let mut filled = 0;
    loop {
        let n = stream.read(&mut buf[filled..]).await?;
        if n == 0 {
            break;
        }
        filled += n;
        if buf[..filled].windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
        if filled == buf.len() {
            break;
        }
    }
    let head = std::str::from_utf8(&buf[..filled]).unwrap_or("");
    let request_line = head.lines().next().unwrap_or("");
    let path = parse_request_path(request_line);

    let (status, body, content_type) = match path {
        Some("/metrics") => {
            let mut buf = String::new();
            // `encode` only fails on a `Write` error; writing to a
            // `String` is infallible.
            encode(&mut buf, &metrics.registry).unwrap();
            (
                "200 OK",
                buf,
                "application/openmetrics-text; version=1.0.0; charset=utf-8",
            )
        }
        Some("/healthz") => ("200 OK", "ok\n".to_string(), "text/plain; charset=utf-8"),
        _ => ("404 Not Found", "not found\n".to_string(), "text/plain; charset=utf-8"),
    };

    let response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.shutdown().await?;
    Ok(())
}

/// Extract the request path from a "GET /path HTTP/1.1" line. Returns
/// `None` if the line is malformed; callers respond `404`.
fn parse_request_path(request_line: &str) -> Option<&str> {
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?;
    let path = parts.next()?;
    // Only handle GET; everything else is a 404. This is exposition,
    // not a REST API.
    if method != "GET" {
        return None;
    }
    Some(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_request_path_extracts_path() {
        assert_eq!(parse_request_path("GET /metrics HTTP/1.1"), Some("/metrics"));
        assert_eq!(parse_request_path("GET / HTTP/1.0"), Some("/"));
        assert_eq!(parse_request_path("POST /metrics HTTP/1.1"), None);
        assert_eq!(parse_request_path("garbage"), None);
        assert_eq!(parse_request_path(""), None);
    }

    #[test]
    fn metrics_register_without_panic() {
        let m = RelayMetrics::new();
        m.connected_peers.set(3);
        m.messages_forwarded
            .get_or_create(&TopicLabels {
                topic: "qos.test".to_string(),
            })
            .inc();
        m.bytes_in.inc_by(1024);
        m.bytes_out.inc_by(512);
        m.kademlia_queries.inc();
        // Round-trip through the encoder so we know the families are
        // wired correctly.
        let mut buf = String::new();
        encode(&mut buf, &m.registry).unwrap();
        assert!(buf.contains("qos_relay_connected_peers"));
        assert!(buf.contains("qos_relay_messages_forwarded"));
        assert!(buf.contains("qos.test"));
    }

    #[tokio::test]
    async fn metrics_endpoint_serves_text() {
        let metrics = Arc::new(RelayMetrics::new());
        metrics.connected_peers.set(7);
        let (bound, _handle) =
            spawn_server(([127u8, 0, 0, 1], 0).into(), metrics.clone())
                .await
                .expect("bind ok");
        // Hand-roll the request to avoid a hyper-client dep.
        let mut s = tokio::net::TcpStream::connect(bound).await.expect("connect");
        s.write_all(b"GET /metrics HTTP/1.1\r\nHost: localhost\r\n\r\n")
            .await
            .unwrap();
        let mut buf = Vec::new();
        s.read_to_end(&mut buf).await.unwrap();
        let text = String::from_utf8_lossy(&buf);
        assert!(text.contains("200 OK"), "response: {text}");
        assert!(text.contains("qos_relay_connected_peers"), "response: {text}");
    }
}
