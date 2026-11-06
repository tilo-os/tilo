//! Fetching from outside Tilo: `web.get` and `web.post`.
//!
//! A request runs once, on the device whose code makes it, through the transport its host
//! installs ([`crate::Runtime::set_web`]). The answer is a [`JournalEntry::Web`] entry of the
//! invocation's journal, and a replay answers from it instead of asking the network again
//! ([`crate::flow::JournalEntry::Web`]).
//!
//! What can go wrong is a value, never a trap: a refused host, a timeout and a network error
//! answer with status 0 and a `status_text` that says what happened, and `ok` is false. One
//! request is bounded in size and time ([`MAX_BODY`], [`TIMEOUT`]); nothing streams.
//!
//! [`JournalEntry::Web`]: crate::flow::JournalEntry::Web

use crate::flow::{WebRequest, WebResponse};

/// The most a reply's body may be; a larger one is refused as a value.
pub const MAX_BODY: usize = 2 * 1024 * 1024;

/// How long a request may take before the answer is a failure of status 0.
pub const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// What a transport answered, before the invocation records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WebReply {
    pub status: u16,
    pub status_text: String,
    /// Each header as `Name: value`.
    pub headers: Vec<String>,
    pub body: Vec<u8>,
}

impl WebReply {
    /// No answer: a timeout, no network, or a refused request. `reason` is what `status_text`
    /// says.
    #[must_use]
    pub fn failed(reason: impl Into<String>) -> Self {
        Self {
            status: 0,
            status_text: reason.into(),
            headers: Vec::new(),
            body: Vec::new(),
        }
    }
}

/// Where the requests of one host reach: the network of the device whose code makes them, or
/// the host's own for a host-run invocation. A platform that reaches nothing answers
/// [`WebReply::failed`], saying so.
pub trait WebTransport {
    fn fetch(&self, request: &WebRequest) -> WebReply;
}

/// The bytes of a body as a journal keeps them: the text they read as, else base64.
#[must_use]
pub fn body_json(bytes: &[u8]) -> Option<serde_json::Value> {
    use base64::Engine as _;
    if bytes.is_empty() {
        return None;
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        return Some(serde_json::Value::String(text.to_owned()));
    }
    Some(serde_json::json!({ "base64": base64::engine::general_purpose::STANDARD.encode(bytes) }))
}

/// The host a request reaches, as its app header declares it: the lowercased host of an
/// `http` or `https` URL, without its port. `None` for any other URL.
#[must_use]
pub fn host_of(url: &str) -> Option<String> {
    let (scheme, rest) = url.split_once("://")?;
    if !scheme.eq_ignore_ascii_case("http") && !scheme.eq_ignore_ascii_case("https") {
        return None;
    }
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default()
        .rsplit('@')
        .next()
        .unwrap_or_default();
    let host = match authority.rsplit_once(':') {
        // A port, not an IPv6 address's colons (which need brackets, and no app declares one).
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) && !host.contains(':') => {
            host
        }
        _ => authority,
    };
    (!host.is_empty()).then(|| host.to_ascii_lowercase())
}

/// Whether the declared grant `entry` (`example.com`, or `*.rss.org` for its subdomains)
/// covers `host`.
#[must_use]
pub fn covers(entry: &str, host: &str) -> bool {
    let entry = entry.trim().to_ascii_lowercase();
    let host = host.to_ascii_lowercase();
    match entry.strip_prefix("*.") {
        Some(suffix) => host
            .strip_suffix(suffix)
            .is_some_and(|before| before.ends_with('.') && before.len() > 1),
        None => entry == host,
    }
}

/// Whether the app declaring `hosts` may reach `host`.
#[must_use]
pub fn granted(hosts: &[String], host: &str) -> bool {
    hosts.iter().any(|entry| covers(entry, host))
}

impl WebResponse {
    /// The answer to record, from what the transport answered.
    #[must_use]
    pub fn of(reply: WebReply) -> Self {
        let WebReply {
            status,
            status_text,
            headers,
            body,
        } = reply;
        let body = if body.len() > MAX_BODY {
            return Self {
                status: 0,
                status_text: format!("the reply was larger than {MAX_BODY} bytes"),
                headers: Vec::new(),
                body: None,
            };
        } else {
            body_json(&body)
        };
        Self {
            status,
            status_text,
            headers,
            body,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declared_host_covers_itself_or_its_subdomains() {
        assert!(granted(&["example.com".to_owned()], "example.com"));
        assert!(granted(&["*.rss.org".to_owned()], "feeds.rss.org"));
        assert!(!granted(&["*.rss.org".to_owned()], "rss.org"));
        assert!(!granted(&["*.rss.org".to_owned()], "evilrss.org"));
        assert!(!granted(
            &["example.com".to_owned()],
            "example.com.evil.org"
        ));
        assert!(!granted(&[], "example.com"));
    }

    #[test]
    fn a_url_names_its_host_without_its_port() {
        assert_eq!(
            host_of("https://Example.COM:8443/a?b").as_deref(),
            Some("example.com")
        );
        assert_eq!(host_of("http://a.b/c").as_deref(), Some("a.b"));
        assert_eq!(host_of("ftp://a.b/c"), None);
        assert_eq!(host_of("no scheme"), None);
    }

    #[test]
    fn a_body_reads_back_as_the_bytes_it_was() {
        let response = WebResponse::of(WebReply {
            status: 200,
            status_text: "OK".to_owned(),
            headers: vec![],
            body: vec![0xff, 0xfe],
        });
        assert_eq!(response.bytes(), vec![0xff, 0xfe]);
        let text = WebResponse::of(WebReply {
            status: 200,
            status_text: "OK".to_owned(),
            headers: vec![],
            body: b"hi".to_vec(),
        });
        assert_eq!(text.bytes(), b"hi");
        assert_eq!(text.body, Some(serde_json::json!("hi")));
    }
}
