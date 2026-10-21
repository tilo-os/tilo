//! Background fetching.
//!
//! Networking runs off the UI thread: each [`FetchRequest`] becomes its own
//! task on the shared app runtime, and posts its [`FetchResult`] back over a
//! channel the moment it completes, so one slow feed never holds up the
//! others. The widget drains results during its `sync` pass and folds them
//! into the store + CRDT. HTTP feeds go through `tilo_net` + feed-rs;
//! `gemini://` feeds go through a raw TLS socket (desktop only) and the
//! gemtext parser, trusting the server certificate on first use: the first
//! fetch pins it on the feed and every later fetch must present the same one. Parsing runs on the CPU pool, not on the runtime thread.
//! In a page, feeds whose sites do not allow cross-origin reads go through
//! the host's fetch proxy when it offers one.
//!
//! An article's own page is fetched too ([`FetchRequest::Page`]): its article
//! is found the way reader modes find it ([`ui::reading::extract::article`])
//! and the page is kept on disk, so the whole article reads offline. Pages
//! are fetched a few at a time and their results wake the window, so a
//! batch of articles fetched in the background costs no frames.
//!
//! None of this is touched by `--shots` — the worker is created lazily on the
//! first real fetch, so the offline screenshot stays hermetic.

use std::io::Cursor;
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use std::collections::VecDeque;

use ui::reading::Document;

use crate::gemtext;
use crate::model::{FeedKind, Fetched, Id};

/// A request from the UI thread to the fetch worker.
#[derive(Clone, Debug)]
pub enum FetchRequest {
    /// Re-fetch a known feed and return any new articles.
    Refresh {
        feed_id: Id,
        url: String,
        kind: FeedKind,
        /// The feed's [`crate::model::Feed::certificate_pin`]; the server must match it.
        certificate_pin: Option<String>,
    },
    /// Fetch an article's own page (or read the copy kept from before) and
    /// find its article.
    Page { article: Id, url: String },
}

/// A result posted back to the UI thread.
#[derive(Clone, Debug)]
pub enum FetchResult {
    /// New articles for an existing feed (plus a possibly-updated title).
    Articles {
        feed_id: Id,
        title: Option<String>,
        articles: Vec<Fetched>,
        /// The certificate a gemini server presented, for a feed to pin on
        /// its first fetch.
        certificate_pin: Option<String>,
    },
    /// Something went wrong; `feed_id` is set for a refresh failure.
    Failed {
        feed_id: Option<Id>,
        message: String,
    },
    /// The article an article's page holds, when it holds one.
    Page {
        article: Id,
        document: Option<Box<Document>>,
    },
}

/// Where queued requests go.
enum Dispatch {
    /// Each request runs as a task on the app runtime.
    Runtime(&'static ui::rt::AppRuntime),
    /// The runtime could not be started; requests are dropped (the failure
    /// was reported once as a [`FetchResult::Failed`]).
    Unavailable,
    /// Tests: requests are only counted, never fetched.
    #[cfg(test)]
    Record,
}

/// Handle to the fetcher. Dropping it drops the result channel; tasks still
/// in flight finish and their results are discarded.
pub struct FetchWorker {
    dispatch: Dispatch,
    results: Sender<FetchResult>,
    rx: Receiver<FetchResult>,
    pending: usize,
    /// Pages are fetched [`PAGES_AT_ONCE`] at a time; the rest wait here.
    pages: VecDeque<(Id, String)>,
    pages_in_flight: usize,
    /// Page results wake the window, which drains them on its next pass.
    page_results: Option<ui::wake::WakingSender<FetchResult>>,
    page_rx: Option<Receiver<FetchResult>>,
}

/// How many article pages are fetched at once.
const PAGES_AT_ONCE: usize = 3;

impl FetchWorker {
    /// Create a fetcher backed by the app runtime.
    pub fn spawn() -> Self {
        let (res_tx, res_rx) = std::sync::mpsc::channel::<FetchResult>();
        let dispatch = match ui::rt::try_app() {
            Ok(runtime) => Dispatch::Runtime(runtime),
            Err(e) => {
                let _ = res_tx.send(FetchResult::Failed {
                    feed_id: None,
                    message: format!("runtime: {e}"),
                });
                Dispatch::Unavailable
            }
        };
        let (page_results, page_rx) = ui::wake::waking_channel::<FetchResult>();
        Self {
            dispatch,
            results: res_tx,
            rx: res_rx,
            pending: 0,
            pages: VecDeque::new(),
            pages_in_flight: 0,
            page_results: Some(page_results),
            page_rx: Some(page_rx),
        }
    }

    /// Queue a request. Increments the in-flight counter. Never waits: the
    /// fetch runs as a task on the app runtime.
    pub fn send(&mut self, req: FetchRequest) {
        if let FetchRequest::Page { article, url } = req {
            if !self.pages.iter().any(|(queued, _)| *queued == article) {
                self.pages.push_back((article, url));
            }
            self.start_pages();
            return;
        }
        match self.dispatch {
            Dispatch::Runtime(runtime) => {
                let results = self.results.clone();
                runtime.spawn(async move {
                    let result = handle(http_client(), req).await;
                    // If the UI is gone the send fails; the task simply ends.
                    let _ = results.send(result);
                });
                self.pending += 1;
            }
            Dispatch::Unavailable => {}
            #[cfg(test)]
            Dispatch::Record => self.pending += 1,
        }
    }

    /// Fetch the next queued pages, up to [`PAGES_AT_ONCE`] at a time.
    fn start_pages(&mut self) {
        while self.pages_in_flight < PAGES_AT_ONCE {
            let Some((article, url)) = self.pages.pop_front() else {
                return;
            };
            match self.dispatch {
                Dispatch::Runtime(runtime) => {
                    let Some(results) = self.page_results.clone() else {
                        return;
                    };
                    runtime.spawn(async move {
                        let document = fetch_page(http_client(), &url).await.map(Box::new);
                        let _ = results.send(FetchResult::Page { article, document });
                    });
                    self.pages_in_flight += 1;
                }
                Dispatch::Unavailable => {}
                #[cfg(test)]
                Dispatch::Record => self.pages_in_flight += 1,
            }
        }
    }

    /// Drain every result that has arrived since the last poll.
    pub fn poll(&mut self) -> Vec<FetchResult> {
        let mut out = Vec::new();
        while let Ok(r) = self.rx.try_recv() {
            self.pending = self.pending.saturating_sub(1);
            out.push(r);
        }
        let mut pages = 0;
        if let Some(rx) = &self.page_rx {
            while let Ok(r) = rx.try_recv() {
                pages += 1;
                out.push(r);
            }
        }
        if pages > 0 {
            self.pages_in_flight = self.pages_in_flight.saturating_sub(pages);
            self.start_pages();
        }
        out
    }

    /// Article pages queued or being fetched.
    pub fn pages_pending(&self) -> usize {
        self.pages.len() + self.pages_in_flight
    }

    pub fn pending(&self) -> usize {
        self.pending
    }

    /// Build a worker that never fetches, plus the sender half of its result
    /// channel. Used by tests to drive the *non-blocking* result path
    /// deterministically: enqueue requests (which never touch the network),
    /// inject `FetchResult`s through the returned sender exactly as a real
    /// fetch task would, and assert the UI drains them. `send()` here only
    /// records `pending` — proving that queuing a refresh is a constant-time,
    /// network-free operation.
    #[cfg(test)]
    pub fn for_test() -> (Self, Sender<FetchResult>) {
        let (res_tx, res_rx) = std::sync::mpsc::channel::<FetchResult>();
        (
            Self {
                dispatch: Dispatch::Record,
                results: res_tx.clone(),
                rx: res_rx,
                pending: 0,
                pages: VecDeque::new(),
                pages_in_flight: 0,
                page_results: None,
                page_rx: None,
            },
            res_tx,
        )
    }
}

/// The shared HTTP client, built on first use by the first fetch task.
fn http_client() -> Option<&'static tilo_net::Client> {
    static CLIENT: OnceLock<Option<tilo_net::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            tilo_net::Client::builder()
                .user_agent("Tilo-Feeds/0.1 (+https://tilo.so)")
                .timeout(Duration::from_secs(20))
                .fetch_proxy(tilo_net::proxy::ProxyUse::CrossOrigin)
                .build()
                .ok()
        })
        .as_ref()
}

async fn handle(client: Option<&tilo_net::Client>, req: FetchRequest) -> FetchResult {
    match req {
        FetchRequest::Refresh {
            feed_id,
            url,
            kind,
            certificate_pin,
        } => match fetch_feed(client, &url, kind, certificate_pin.as_deref()).await {
            Ok(Found {
                title,
                articles,
                certificate_pin,
            }) => FetchResult::Articles {
                feed_id,
                title,
                articles,
                certificate_pin,
            },
            Err(e) => FetchResult::Failed {
                feed_id: Some(feed_id),
                message: e,
            },
        },
        FetchRequest::Page { article, url } => {
            let document = fetch_page(client, &url).await.map(Box::new);
            FetchResult::Page { article, document }
        }
    }
}

/// The largest article page read; a bigger one is not taken.
const MAX_PAGE_BYTES: usize = 6 * 1024 * 1024;

/// The pages kept for reading offline, in the app's state.
fn page_dir() -> Option<tilo_files::Dir> {
    ui::storage::AppFiles::for_app(crate::resource::FEEDS_APP_ID)
        .state()
        .ok()
}

/// Where the page at `url` is kept.
fn page_path(url: &str) -> Option<tilo_files::VPath> {
    let name = format!("pages/{}.html", blake3::hash(url.as_bytes()).to_hex());
    tilo_files::VPath::new(&name).ok()
}

/// The article the page at `url` holds: from the copy kept on disk, else
/// fetched (and kept). `None` when it cannot be read or holds no article.
async fn fetch_page(client: Option<&tilo_net::Client>, url: &str) -> Option<Document> {
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return None;
    }
    let dir = page_dir();
    let path = page_path(url);
    let kept = match (&dir, &path) {
        (Some(dir), Some(path)) => dir.read(path).await.ok(),
        _ => None,
    };
    let bytes = match kept {
        Some(bytes) if !bytes.is_empty() => bytes,
        _ => {
            let response = client?.get(url).send().await.ok()?;
            if !response.status().is_success() {
                return None;
            }
            let bytes = response.bytes_limited(MAX_PAGE_BYTES).await.ok()?.to_vec();
            if let (Some(dir), Some(path)) = (&dir, &path) {
                if let Ok(pages) = tilo_files::VPath::new("pages") {
                    let _ = dir.create_dir_all(&pages).await;
                }
                let _ = dir.write_atomic(path, &bytes).await;
            }
            bytes
        }
    };
    let url = url.to_owned();
    ui::rt::cpu(move || {
        let html = String::from_utf8_lossy(&bytes);
        ui::reading::extract::article(&html, &url)
    })
    .await
    .ok()
    .flatten()
}

/// Where an article's pictures come from: the copy kept on disk, else the
/// network (and kept), so an article read once shows its pictures offline.
pub fn image_store() -> ui::ImageStore {
    let fetch: ui::Fetcher = std::sync::Arc::new(|url: &str| {
        let runtime = ui::rt::try_app().ok()?;
        let url = url.to_owned();
        runtime
            .block_on(async move {
                let dir = page_dir();
                let path = tilo_files::VPath::new(&format!(
                    "pictures/{}",
                    blake3::hash(url.as_bytes()).to_hex()
                ))
                .ok();
                if let (Some(dir), Some(path)) = (&dir, &path)
                    && let Ok(bytes) = dir.read(path).await
                    && !bytes.is_empty()
                {
                    return Some(bytes);
                }
                let response = http_client()?.get(&url).send().await.ok()?;
                if !response.status().is_success() {
                    return None;
                }
                let bytes = response
                    .bytes_limited(MAX_PICTURE_BYTES)
                    .await
                    .ok()?
                    .to_vec();
                if let (Some(dir), Some(path)) = (&dir, &path) {
                    if let Ok(pictures) = tilo_files::VPath::new("pictures") {
                        let _ = dir.create_dir_all(&pictures).await;
                    }
                    let _ = dir.write_atomic(path, &bytes).await;
                }
                Some(bytes)
            })
            .ok()
            .flatten()
    });
    ui::ImageStore::with_max_dimension(&fetch, 2048)
}

/// The largest picture read.
const MAX_PICTURE_BYTES: usize = 12 * 1024 * 1024;

/// What one fetch found.
struct Found {
    title: Option<String>,
    articles: Vec<Fetched>,
    /// The certificate a gemini server presented; `None` over HTTP.
    certificate_pin: Option<String>,
}

/// Fetch + parse a feed. A gemini server must present `certificate_pin`
/// when the feed has one.
async fn fetch_feed(
    client: Option<&tilo_net::Client>,
    url: &str,
    kind: FeedKind,
    certificate_pin: Option<&str>,
) -> Result<Found, String> {
    if kind == FeedKind::Gemini || url.starts_with("gemini://") {
        return fetch_gemini(url, certificate_pin).await;
    }
    let client = client.ok_or("no http client")?;
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("request: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("HTTP {}", resp.status().as_u16()));
    }
    let bytes = resp
        .bytes_limited(MAX_FEED_BYTES)
        .await
        .map_err(|e| format!("body: {e}"))?;
    let (title, articles) = ui::rt::cpu(move || parse_http_feed(&bytes))
        .await
        .map_err(|e| format!("parse: {e}"))??;
    Ok(Found {
        title,
        articles,
        certificate_pin: None,
    })
}

/// The largest feed or gemini page read; bigger answers are refused.
const MAX_FEED_BYTES: usize = 16 * 1024 * 1024;

/// Parse RSS/Atom/JSON-feed bytes into our model via feed-rs.
fn parse_http_feed(bytes: &[u8]) -> Result<(Option<String>, Vec<Fetched>), String> {
    let parsed = feed_rs::parser::parse(Cursor::new(bytes)).map_err(|e| format!("parse: {e}"))?;
    let title = parsed.title.map(|t| t.content);

    let mut articles = Vec::new();
    for entry in parsed.entries {
        let title = entry
            .title
            .map_or_else(|| "(untitled)".to_string(), |t| t.content);
        // Atom links are unordered: a self link or enclosure is not the
        // article's web page. Prefer an HTML alternate, then any alternate;
        // keep the first link as a fallback for feeds without one.
        let url = entry
            .links
            .iter()
            .filter(|link| link.rel.as_deref().is_none_or(|rel| rel == "alternate"))
            .min_by_key(|link| match link.media_type.as_deref() {
                Some("text/html" | "application/xhtml+xml") => 0,
                None => 1,
                _ => 2,
            })
            .or_else(|| entry.links.first())
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
            .map_or(0, |d| d.timestamp());

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
        let summary = without_title(&summary, &title);
        articles.push(Fetched {
            title,
            author,
            url,
            published,
            summary,
            blocks: Vec::new(),
            content: body_html,
        });
    }
    Ok((title, articles))
}

/// How much of an article's text its excerpt keeps.
const EXCERPT_CHARS: usize = 280;

/// A short, tag-free preview for the article list.
fn plain_summary(html_text: &str) -> String {
    ui::reading::extract::plain_text(html_text)
        .chars()
        .take(EXCERPT_CHARS + 120)
        .collect()
}

/// `summary` without the article's title it may open with (many feeds put
/// the title at the top of the body), cut to the excerpt's length.
pub(crate) fn without_title(summary: &str, title: &str) -> String {
    ui::reading::strip_leading_title(summary, title)
        .chars()
        .take(EXCERPT_CHARS)
        .collect()
}

pub(crate) fn derive_site_url(feed_url: &str) -> String {
    // scheme://host  (drop path) as a reasonable homepage guess.
    if let Some(scheme_end) = feed_url.find("://") {
        let after = feed_url.get(scheme_end + 3..).unwrap_or_default();
        let host_end = after.find('/').unwrap_or(after.len());
        return format!(
            "{}{}",
            feed_url.get(..scheme_end + 3).unwrap_or_default(),
            after.get(..host_end).unwrap_or_default()
        );
    }
    feed_url.to_string()
}

// =====================================================================
// Gemini (gemini://, port 1965, TOFU)
// =====================================================================

async fn fetch_gemini(url: &str, certificate_pin: Option<&str>) -> Result<Found, String> {
    let (body, presented) = gemini_request(url, certificate_pin).await?;
    let (body, blocks) = ui::rt::cpu(move || {
        let blocks = gemtext::to_blocks(&body);
        (body, blocks)
    })
    .await
    .map_err(|e| format!("parse: {e}"))?;
    // Use the first heading as the feed/article title if present.
    let title = blocks
        .iter()
        .find(|b| matches!(b.kind, crate::model::BlockKind::Heading1))
        .map(|b| b.text.clone());
    let summary: String = body.chars().take(200).collect();
    let article = Fetched {
        title: title.clone().unwrap_or_else(|| url.to_string()),
        author: String::new(),
        url: url.to_string(),
        published: ui::clock::local_now().timestamp(),
        summary,
        blocks,
        content: String::new(),
    };
    Ok(Found {
        title,
        articles: vec![article],
        certificate_pin: Some(presented),
    })
}

/// The pin of the certificate `host` presented (BLAKE3 of its DER, hex),
/// or why it is refused: there is none, or it is not the `expected` pin.
fn check_certificate(
    host: &str,
    der: Option<&[u8]>,
    expected: Option<&str>,
) -> Result<String, String> {
    let presented = blake3::hash(der.ok_or("the server presented no certificate")?)
        .to_hex()
        .to_string();
    if expected.is_some_and(|expected| expected != presented) {
        return Err(format!(
            "{host} presented a different certificate than when you subscribed; \
             someone may be impersonating it"
        ));
    }
    Ok(presented)
}

/// Request `url` and return its body with the pin of the certificate the
/// server presented, refusing a server whose certificate is not `expected`.
async fn gemini_request(url: &str, expected: Option<&str>) -> Result<(String, String), String> {
    use tilo_net::sockets::{TlsStream, Trust};

    let rest = url.strip_prefix("gemini://").unwrap_or(url);
    let host_port_end = rest.find('/').unwrap_or(rest.len());
    let host = rest.get(..host_port_end).unwrap_or_default();
    let (host_only, port) = match host.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<u16>().unwrap_or(1965)),
        None => (host.to_string(), 1965u16),
    };

    // Gemini servers overwhelmingly use self-signed certificates, so the
    // protocol relies on trust on first use rather than a CA chain: any
    // certificate completes the handshake, and the pin decides whether this
    // is the server the feed was subscribed to. Nothing is sent before that.
    let mut tls = TlsStream::connect(&host_only, port, Trust::AnyCertificate)
        .await
        .map_err(|e| format!("connect: {e}"))?;
    let presented = check_certificate(&host_only, tls.peer_certificate().as_deref(), expected)?;

    let request = format!("{url}\r\n");
    tls.write_all(request.as_bytes())
        .await
        .map_err(|e| format!("write: {e}"))?;

    let raw = tls
        .read_to_end(MAX_FEED_BYTES)
        .await
        .map_err(|e| format!("read: {e}"))?;

    let text = String::from_utf8_lossy(&raw);
    // Response = "<status><space><meta>\r\n" then body.
    let Some(nl) = text.find("\r\n") else {
        return Err("malformed gemini response".into());
    };
    let Some(header) = text.get(..nl) else {
        return Err("malformed gemini response".into());
    };
    let status = header.chars().take(2).collect::<String>();
    if !status.starts_with('2') {
        return Err(format!("gemini status {header}"));
    }
    Ok((
        text.get(nl + 2..).unwrap_or_default().to_string(),
        presented,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atom_articles_use_the_web_alternate_instead_of_self_or_enclosure() {
        let atom = r#"<feed xmlns="http://www.w3.org/2005/Atom">
          <id>https://example.org/feed</id><title>News</title>
          <updated>2025-06-30T12:00:00Z</updated>
          <entry><id>https://example.org/posts/1</id><title>First</title>
            <updated>2025-06-30T12:00:00Z</updated>
            <link rel="self" type="application/atom+xml" href="https://example.org/api/1"/>
            <link rel="enclosure" type="audio/mpeg" href="https://example.org/audio/1"/>
            <link rel="alternate" type="application/json" href="https://example.org/json/1"/>
            <link rel="alternate" type="text/html" href="https://example.org/posts/1"/>
          </entry>
          <entry><id>https://example.org/posts/2</id><title>Second</title>
            <updated>2025-06-30T12:00:00Z</updated>
            <link rel="self" href="https://example.org/api/2"/>
            <link href="https://example.org/posts/2"/>
          </entry>
        </feed>"#;
        let (_, articles) = parse_http_feed(atom.as_bytes()).unwrap();
        assert_eq!(articles.len(), 2);
        assert_eq!(articles[0].url, "https://example.org/posts/1");
        assert_eq!(articles[1].url, "https://example.org/posts/2");
    }

    /// Fetching derives site URLs, parses RSS bytes and polls the worker without blocking.
    #[test]
    fn fetch_parses_feeds_and_polls_the_worker_without_blocking() {
        // --- phase: derive_site_url_drops_path ---
        {
            assert_eq!(
                derive_site_url("https://blog.rust-lang.org/feed.xml"),
                "https://blog.rust-lang.org"
            );
            assert_eq!(
                derive_site_url("gemini://example.org/foo/bar"),
                "gemini://example.org"
            );
        }

        // --- phase: a gemini server must present the pinned certificate ---
        {
            let pin = check_certificate("example.org", Some(b"cert"), None).unwrap();
            assert_eq!(pin, blake3::hash(b"cert").to_hex().as_str());
            assert_eq!(
                check_certificate("example.org", Some(b"cert"), Some(&pin)),
                Ok(pin.clone())
            );
            let refused = check_certificate("example.org", Some(b"other"), Some(&pin));
            assert!(refused.is_err_and(|e| e.contains("different certificate")));
            assert!(check_certificate("example.org", None, None).is_err());
        }

        // --- phase: worker_send_is_nonblocking_and_poll_drains ---
        {
            fn worker_send_is_nonblocking_and_poll_drains() {
                // A worker that records requests instead of fetching them.
                let (mut w, res_tx) = FetchWorker::for_test();
                for n in 0..50 {
                    w.send(FetchRequest::Refresh {
                        feed_id: Id::from_raw(format!("feed-{n}")),
                        url: "https://example.invalid/feed.xml".into(),
                        kind: FeedKind::Rss,
                        certificate_pin: None,
                    });
                }
                assert_eq!(w.pending(), 50);

                assert!(w.poll().is_empty());

                // A "worker thread" posts a result; the UI drains it non-blockingly.
                let fid = Id::from_raw("refreshed");
                res_tx
                    .send(FetchResult::Articles {
                        feed_id: fid.clone(),
                        title: Some("T".into()),
                        articles: vec![],
                        certificate_pin: None,
                    })
                    .unwrap();
                let drained = w.poll();
                assert_eq!(drained.len(), 1);
                assert_eq!(w.pending(), 49, "poll decrements the in-flight counter");
                match &drained[0] {
                    FetchResult::Articles { feed_id, .. } => assert_eq!(feed_id, &fid),
                    FetchResult::Failed { .. } | FetchResult::Page { .. } => {
                        panic!("wrong result variant")
                    }
                }
            }
            worker_send_is_nonblocking_and_poll_drains();
        }

        // --- phase: parse_rss_bytes ---
        {
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
            assert!(articles[0].content.contains("Hello world"));
            assert_eq!(articles[0].summary, "Hello world");
        }
    }
}
