//! Background fetch worker.
//!
//! Networking runs off the UI thread: a dedicated thread owns a Tokio
//! runtime and a reqwest client, receives [`FetchRequest`]s over a channel,
//! and posts [`FetchResult`]s back. The widget drains results during its
//! `sync` pass and folds them into the store + CRDT. HTTP feeds go through
//! reqwest + feed-rs; `gemini://` feeds go through a raw TLS socket (TOFU)
//! and the gemtext parser.
//!
//! None of this is touched by `--shots` — the worker is spawned lazily on
//! the first real fetch, so the offline screenshot stays hermetic.

use std::io::Cursor;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use uuid::Uuid;

use crate::model::{Article, Feed, FeedKind, Id};
use crate::{gemtext, html};

/// A request from the UI thread to the fetch worker.
#[derive(Clone, Debug)]
pub enum FetchRequest {
    /// Re-fetch a known feed and return any new articles.
    Refresh {
        feed_id: Id,
        url: String,
        kind: FeedKind,
    },
    /// Resolve a brand-new subscription URL: discover the feed metadata
    /// and its initial articles.
    Subscribe { url: String, folder: Option<Id> },
}

/// A result posted back to the UI thread.
#[derive(Clone, Debug)]
pub enum FetchResult {
    /// New articles for an existing feed (plus a possibly-updated title).
    Articles {
        feed_id: Id,
        title: Option<String>,
        articles: Vec<Article>,
    },
    /// A newly-discovered feed and its initial articles.
    Subscribed { feed: Feed, articles: Vec<Article> },
    /// Something went wrong; `feed_id` is set for a refresh failure.
    Failed {
        feed_id: Option<Id>,
        message: String,
    },
}

/// Handle to the worker. Dropping it closes the request channel, which
/// makes the worker thread exit after its current job.
pub struct FetchWorker {
    tx: Sender<FetchRequest>,
    rx: Receiver<FetchResult>,
    pending: usize,
}

impl FetchWorker {
    /// Spawn the worker thread + its Tokio runtime.
    pub fn spawn() -> Self {
        let (req_tx, req_rx) = std::sync::mpsc::channel::<FetchRequest>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<FetchResult>();

        std::thread::Builder::new()
            .name("feeds-fetch".into())
            .spawn(move || worker_main(req_rx, res_tx))
            .expect("spawn fetch worker");

        Self {
            tx: req_tx,
            rx: res_rx,
            pending: 0,
        }
    }

    /// Queue a request. Increments the in-flight counter.
    pub fn send(&mut self, req: FetchRequest) {
        if self.tx.send(req).is_ok() {
            self.pending += 1;
        }
    }

    /// Drain every result that has arrived since the last poll.
    pub fn poll(&mut self) -> Vec<FetchResult> {
        let mut out = Vec::new();
        while let Ok(r) = self.rx.try_recv() {
            self.pending = self.pending.saturating_sub(1);
            out.push(r);
        }
        out
    }

    pub fn pending(&self) -> usize {
        self.pending
    }

    /// Build a worker with no backing fetch thread, plus the sender half of its
    /// result channel. Used by tests to drive the *non-blocking* result path
    /// deterministically: enqueue requests (which never touch the network),
    /// inject `FetchResult`s through the returned sender exactly as a real
    /// worker thread would, and assert the UI drains them. Because there's no
    /// thread, `send()` here only records `pending` — proving that queuing a
    /// refresh is a constant-time, network-free operation.
    #[cfg(test)]
    pub fn for_test() -> (Self, Sender<FetchResult>) {
        let (req_tx, _req_rx) = std::sync::mpsc::channel::<FetchRequest>();
        let (res_tx, res_rx) = std::sync::mpsc::channel::<FetchResult>();
        // `_req_rx` is dropped, so `send()` would fail; keep it alive so the
        // pending counter advances just like the production path.
        std::mem::forget(_req_rx);
        (
            Self {
                tx: req_tx,
                rx: res_rx,
                pending: 0,
            },
            res_tx,
        )
    }
}

fn worker_main(req_rx: Receiver<FetchRequest>, res_tx: Sender<FetchResult>) {
    // A *multi-thread* runtime so a burst of refresh requests run concurrently
    // rather than head-of-line-blocking each other. Previously this used a
    // current-thread runtime and `block_on`'d each request in turn, so a single
    // slow (or 20s-timeout) feed stalled every feed queued behind it, making a
    // "Refresh all" feel like it hung. Now each request is `spawn`ed and posts
    // its own result back the instant it completes — fast feeds land first.
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(6)
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = res_tx.send(FetchResult::Failed {
                feed_id: None,
                message: format!("runtime: {e}"),
            });
            return;
        }
    };

    let client = reqwest::Client::builder()
        .user_agent("qOS-Feeds/0.1 (+https://qos.local)")
        .timeout(Duration::from_secs(20))
        .build()
        .ok();

    // Receive requests on this (blocking) thread and hand each one to the
    // runtime as an independent task. The channel `recv()` blocks here, off the
    // UI thread, so the UI never waits on the network.
    while let Ok(req) = req_rx.recv() {
        let client = client.clone();
        let res_tx = res_tx.clone();
        runtime.spawn(async move {
            let result = handle(&client, req).await;
            // If the UI is gone the send fails; the task simply ends.
            let _ = res_tx.send(result);
        });
    }
    // Drop the runtime — outstanding tasks are detached and torn down when the
    // process exits; for app shutdown that's fine (best-effort fetches).
}

async fn handle(client: &Option<reqwest::Client>, req: FetchRequest) -> FetchResult {
    match req {
        FetchRequest::Refresh { feed_id, url, kind } => {
            match fetch_feed(client, &url, kind).await {
                Ok((title, articles)) => {
                    let articles = articles
                        .into_iter()
                        .map(|mut a| {
                            a.feed_id = feed_id;
                            a
                        })
                        .collect();
                    FetchResult::Articles {
                        feed_id,
                        title,
                        articles,
                    }
                }
                Err(e) => FetchResult::Failed {
                    feed_id: Some(feed_id),
                    message: e,
                },
            }
        }
        FetchRequest::Subscribe { url, folder } => {
            let kind = if url.starts_with("gemini://") {
                FeedKind::Gemini
            } else {
                FeedKind::Rss
            };
            match fetch_feed(client, &url, kind).await {
                Ok((title, articles)) => {
                    let feed_id = Uuid::new_v4();
                    let site_url = derive_site_url(&url);
                    let feed = Feed {
                        id: feed_id,
                        title: title.unwrap_or_else(|| url.clone()),
                        url: url.clone(),
                        site_url,
                        folder,
                        order: 0,
                        kind,
                    };
                    let articles = articles
                        .into_iter()
                        .map(|mut a| {
                            a.feed_id = feed_id;
                            a
                        })
                        .collect();
                    FetchResult::Subscribed { feed, articles }
                }
                Err(e) => FetchResult::Failed {
                    feed_id: None,
                    message: e,
                },
            }
        }
    }
}

/// Fetch + parse a feed, returning (title, articles).
async fn fetch_feed(
    client: &Option<reqwest::Client>,
    url: &str,
    kind: FeedKind,
) -> Result<(Option<String>, Vec<Article>), String> {
    if kind == FeedKind::Gemini || url.starts_with("gemini://") {
        return fetch_gemini(url).await;
    }
    let client = client.as_ref().ok_or("no http client")?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let bytes = resp.bytes().await.map_err(|e| format!("body: {e}"))?;
    parse_http_feed(&bytes)
}

/// Parse RSS/Atom/JSON-feed bytes into our model via feed-rs.
fn parse_http_feed(bytes: &[u8]) -> Result<(Option<String>, Vec<Article>), String> {
    let parsed = feed_rs::parser::parse(Cursor::new(bytes)).map_err(|e| format!("parse: {e}"))?;
    let title = parsed.title.map(|t| t.content);

    let mut articles = Vec::new();
    for entry in parsed.entries {
        let title = entry
            .title
            .map(|t| t.content)
            .unwrap_or_else(|| "(untitled)".to_string());
        let url = entry
            .links
            .first()
            .map(|l| l.href.clone())
            .unwrap_or_default();
        let author = entry
            .authors
            .first()
            .map(|p| p.name.clone())
            .unwrap_or_default();
        let published = entry
            .published
            .or(entry.updated)
            .map(|d| d.timestamp())
            .unwrap_or(0);

        // Prefer full content; fall back to summary.
        let (summary, body_html) = match (&entry.summary, &entry.content) {
            (Some(s), Some(c)) => (
                plain_summary(&s.content),
                c.body.clone().unwrap_or_else(|| s.content.clone()),
            ),
            (Some(s), None) => (plain_summary(&s.content), s.content.clone()),
            (None, Some(c)) => {
                let body = c.body.clone().unwrap_or_default();
                (plain_summary(&body), body)
            }
            (None, None) => (String::new(), String::new()),
        };
        let blocks = html::to_blocks(&body_html);

        articles.push(Article {
            id: Uuid::new_v4(),
            feed_id: Uuid::nil(), // filled in by caller
            title,
            author,
            url,
            published,
            summary,
            blocks,
            read: false,
            starred: false,
        });
    }
    Ok((title, articles))
}

/// A short, tag-free preview line for the article list.
fn plain_summary(html_text: &str) -> String {
    let blocks = html::to_blocks(html_text);
    let text = blocks
        .iter()
        .map(|b| b.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    let trimmed: String = text.chars().take(240).collect();
    trimmed
}

pub(crate) fn derive_site_url(feed_url: &str) -> String {
    // scheme://host  (drop path) as a reasonable homepage guess.
    if let Some(scheme_end) = feed_url.find("://") {
        let after = &feed_url[scheme_end + 3..];
        let host_end = after.find('/').unwrap_or(after.len());
        return format!("{}{}", &feed_url[..scheme_end + 3], &after[..host_end]);
    }
    feed_url.to_string()
}

// =====================================================================
// Gemini (gemini://, port 1965, TOFU)
// =====================================================================

async fn fetch_gemini(url: &str) -> Result<(Option<String>, Vec<Article>), String> {
    let body = gemini_request(url).await?;
    let blocks = gemtext::to_blocks(&body);
    // Use the first heading as the feed/article title if present.
    let title = blocks
        .iter()
        .find(|b| matches!(b.kind, crate::model::BlockKind::Heading1))
        .map(|b| b.text.clone());
    let summary: String = body.chars().take(200).collect();
    let article = Article {
        id: Uuid::new_v4(),
        feed_id: Uuid::nil(),
        title: title.clone().unwrap_or_else(|| url.to_string()),
        author: "gemini".into(),
        url: url.to_string(),
        published: chrono::Utc::now().timestamp(),
        summary,
        blocks,
        read: false,
        starred: false,
    };
    Ok((title, vec![article]))
}

async fn gemini_request(url: &str) -> Result<String, String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;
    use tokio_rustls::TlsConnector;

    let rest = url.strip_prefix("gemini://").unwrap_or(url);
    let host_port_end = rest.find('/').unwrap_or(rest.len());
    let host = &rest[..host_port_end];
    let (host_only, port) = match host.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(1965)),
        None => (host.to_string(), 1965u16),
    };

    let config = gemini_tls_config()?;
    let connector = TlsConnector::from(std::sync::Arc::new(config));
    let server_name = tokio_rustls::rustls::pki_types::ServerName::try_from(host_only.clone())
        .map_err(|_| "bad host".to_string())?;

    let tcp = TcpStream::connect((host_only.as_str(), port))
        .await
        .map_err(|e| format!("connect: {e}"))?;
    let mut tls = connector
        .connect(server_name, tcp)
        .await
        .map_err(|e| format!("tls: {e}"))?;

    let request = format!("{url}\r\n");
    tls.write_all(request.as_bytes())
        .await
        .map_err(|e| format!("write: {e}"))?;

    let mut raw = Vec::new();
    tls.read_to_end(&mut raw)
        .await
        .map_err(|e| format!("read: {e}"))?;

    let text = String::from_utf8_lossy(&raw);
    // Response = "<status><space><meta>\r\n" then body.
    let Some(nl) = text.find("\r\n") else {
        return Err("malformed gemini response".into());
    };
    let header = &text[..nl];
    let status = header.chars().take(2).collect::<String>();
    if !status.starts_with('2') {
        return Err(format!("gemini status {header}"));
    }
    Ok(text[nl + 2..].to_string())
}

fn gemini_tls_config() -> Result<tokio_rustls::rustls::ClientConfig, String> {
    use tokio_rustls::rustls::ClientConfig;

    // Pin the `ring` provider explicitly rather than depending on a
    // process-wide default being installed (reqwest may register a
    // different backend; relying on the global default would be a panic
    // risk). `ring` is enabled via this crate's tokio-rustls feature.
    let provider = std::sync::Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let config = ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("tls config: {e}"))?
        .dangerous()
        .with_custom_certificate_verifier(std::sync::Arc::new(TofuVerifier))
        .with_no_client_auth();
    Ok(config)
}

/// Trust-on-first-use cert verifier. Gemini servers overwhelmingly use
/// self-signed certificates, so the protocol relies on TOFU rather than a
/// CA chain. We accept any presented certificate; a production reader
/// would pin the fingerprint on first contact.
#[derive(Debug)]
struct TofuVerifier;

impl tokio_rustls::rustls::client::danger::ServerCertVerifier for TofuVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &tokio_rustls::rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[tokio_rustls::rustls::pki_types::CertificateDer<'_>],
        _server_name: &tokio_rustls::rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: tokio_rustls::rustls::pki_types::UnixTime,
    ) -> Result<tokio_rustls::rustls::client::danger::ServerCertVerified, tokio_rustls::rustls::Error>
    {
        Ok(tokio_rustls::rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &tokio_rustls::rustls::pki_types::CertificateDer<'_>,
        _dss: &tokio_rustls::rustls::DigitallySignedStruct,
    ) -> Result<
        tokio_rustls::rustls::client::danger::HandshakeSignatureValid,
        tokio_rustls::rustls::Error,
    > {
        Ok(tokio_rustls::rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &tokio_rustls::rustls::pki_types::CertificateDer<'_>,
        _dss: &tokio_rustls::rustls::DigitallySignedStruct,
    ) -> Result<
        tokio_rustls::rustls::client::danger::HandshakeSignatureValid,
        tokio_rustls::rustls::Error,
    > {
        Ok(tokio_rustls::rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<tokio_rustls::rustls::SignatureScheme> {
        use tokio_rustls::rustls::SignatureScheme as S;
        vec![
            S::RSA_PKCS1_SHA256,
            S::RSA_PKCS1_SHA384,
            S::RSA_PKCS1_SHA512,
            S::ECDSA_NISTP256_SHA256,
            S::ECDSA_NISTP384_SHA384,
            S::RSA_PSS_SHA256,
            S::RSA_PSS_SHA384,
            S::RSA_PSS_SHA512,
            S::ED25519,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_site_url_drops_path() {
        assert_eq!(
            derive_site_url("https://blog.rust-lang.org/feed.xml"),
            "https://blog.rust-lang.org"
        );
        assert_eq!(
            derive_site_url("gemini://example.org/foo/bar"),
            "gemini://example.org"
        );
    }

    #[test]
    fn worker_send_is_nonblocking_and_poll_drains() {
        use std::time::Instant;

        // A worker with no fetch thread. Enqueuing must be instant (no I/O).
        let (mut w, res_tx) = FetchWorker::for_test();
        let start = Instant::now();
        for _ in 0..50 {
            w.send(FetchRequest::Refresh {
                feed_id: Uuid::new_v4(),
                url: "https://example.invalid/feed.xml".into(),
                kind: FeedKind::Rss,
            });
        }
        // 50 enqueues with bogus URLs return in well under a frame budget,
        // proving `send` never fetches.
        assert!(start.elapsed().as_millis() < 50, "enqueue must not block");
        assert_eq!(w.pending(), 50);

        // Nothing drained yet.
        assert!(w.poll().is_empty());

        // A "worker thread" posts a result; the UI drains it non-blockingly.
        let fid = Uuid::new_v4();
        res_tx
            .send(FetchResult::Articles {
                feed_id: fid,
                title: Some("T".into()),
                articles: vec![],
            })
            .unwrap();
        let drained = w.poll();
        assert_eq!(drained.len(), 1);
        assert_eq!(w.pending(), 49, "poll decrements the in-flight counter");
        match &drained[0] {
            FetchResult::Articles { feed_id, .. } => assert_eq!(*feed_id, fid),
            _ => panic!("wrong result variant"),
        }
    }

    #[test]
    fn parse_rss_bytes() {
        let rss = r#"<?xml version="1.0"?>
        <rss version="2.0"><channel>
          <title>Test Feed</title>
          <item><title>First</title><link>https://x.com/1</link>
            <description>&lt;p&gt;Hello world&lt;/p&gt;</description>
            <pubDate>Mon, 30 Jun 2025 12:00:00 GMT</pubDate></item>
        </channel></rss>"#;
        let (title, articles) = parse_http_feed(rss.as_bytes()).unwrap();
        assert_eq!(title.as_deref(), Some("Test Feed"));
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0].title, "First");
        assert_eq!(articles[0].url, "https://x.com/1");
        assert!(
            articles[0]
                .blocks
                .iter()
                .any(|b| b.text.contains("Hello world"))
        );
    }
}
