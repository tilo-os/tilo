//! Downloading one page: text reduced to readable text, anything else
//! handed back whole for the caller to keep.

use std::fmt;
use std::time::Duration;

use serde::Serialize;
use url::Url;

use crate::guard::{Refusal, check_address, check_url};
use crate::readable;
use crate::search::read_bytes_capped;

/// Bounds on one fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FetchLimits {
    /// The most bytes downloaded; a longer page is cut there and marked
    /// truncated.
    pub max_bytes: usize,
    /// The longest the whole fetch may take, redirects included.
    pub timeout: Duration,
    /// The most redirects followed.
    pub max_redirects: u8,
}

impl Default for FetchLimits {
    fn default() -> Self {
        Self {
            max_bytes: 5 * 1024 * 1024,
            timeout: Duration::from_secs(15),
            max_redirects: 5,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FetchedPage {
    /// Where the page ended up, after redirects.
    pub url: String,
    pub status: u16,
    pub content_type: String,
    /// Whether the download stopped at [`FetchLimits::max_bytes`]. Only a
    /// text body is ever cut: a binary one that does not fit is refused
    /// ([`FetchError::TooLarge`]).
    pub truncated: bool,
    #[serde(flatten)]
    pub body: FetchedBody,
}

/// What a fetched address held.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum FetchedBody {
    /// A page as Markdown-flavoured text: HTML reduced to its main content,
    /// JSON pretty-printed, other text as sent.
    Text {
        #[serde(skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        text: String,
    },
    /// Anything else (a PDF, an image, an archive), whole, as the server
    /// sent it. Never interpreted here: the caller stores it as content.
    Binary {
        #[serde(skip)]
        bytes: Vec<u8>,
        /// How many bytes it is.
        len: usize,
    },
}

impl FetchedPage {
    /// The page's text, when it is text.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match &self.body {
            FetchedBody::Text { text, .. } => Some(text),
            FetchedBody::Binary { .. } => None,
        }
    }

    /// The page's title, when it is text that has one.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        match &self.body {
            FetchedBody::Text { title, .. } => title.as_deref(),
            FetchedBody::Binary { .. } => None,
        }
    }
}

/// Why a page could not be fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FetchError {
    /// The address is not one to fetch.
    InvalidUrl(String),
    /// The address, or one a redirect led to, is local or not on the web.
    Refused(Refusal),
    /// The server answered with an error status.
    Status(u16),
    /// The body is not text and larger than [`FetchLimits::max_bytes`], so
    /// it cannot be kept whole.
    TooLarge(usize),
    /// More redirects than [`FetchLimits::max_redirects`], or one without a
    /// usable address.
    Redirects,
    /// The fetch took longer than [`FetchLimits::timeout`].
    Timeout,
    /// The server could not be reached or the download broke off.
    Network(String),
}

impl FetchError {
    /// Whether trying again later may work.
    #[must_use]
    pub const fn retryable(&self) -> bool {
        match self {
            Self::Timeout | Self::Network(_) => true,
            Self::Status(status) => *status == 429 || *status >= 500,
            Self::InvalidUrl(_) | Self::Refused(_) | Self::TooLarge(_) | Self::Redirects => false,
        }
    }
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(reason) => write!(f, "not a web address: {reason}"),
            Self::Refused(refusal) => refusal.fmt(f),
            Self::Status(status) => write!(f, "the server answered HTTP {status}"),
            Self::TooLarge(limit) => write!(
                f,
                "the file is larger than {}, the most one fetch keeps",
                size(*limit)
            ),
            Self::Redirects => f.write_str("too many redirects, or one to an unusable address"),
            Self::Timeout => f.write_str("the page took too long"),
            Self::Network(reason) => write!(f, "could not download the page: {reason}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// `bytes` as a person reads a size.
fn size(bytes: usize) -> String {
    const MIB: usize = 1024 * 1024;
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MB", bytes / MIB)
    } else if bytes >= 1024 && bytes.is_multiple_of(1024) {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} bytes")
    }
}

/// Download `url`: text reduced to readable text, anything else whole.
///
/// Every hop, the first and each redirect, is checked by [`check_url`], and
/// where the name resolves here by [`check_address`], before a request goes
/// out, so a redirect cannot lead somewhere the first address could not.
pub async fn fetch(
    client: &tilo_net::Client,
    url: &str,
    limits: &FetchLimits,
) -> Result<FetchedPage, FetchError> {
    fetch_reaching(client, url, limits, Reach::Web).await
}

/// [`fetch`] from a fixture server on this machine, which [`fetch`] refuses
/// to reach: for tests of what a fetch makes of a body, never for a service.
///
/// # Errors
///
/// As [`fetch`], without its refusal of local addresses.
#[cfg(feature = "fixture-servers")]
pub async fn fetch_fixture(
    client: &tilo_net::Client,
    url: &str,
    limits: &FetchLimits,
) -> Result<FetchedPage, FetchError> {
    fetch_reaching(client, url, limits, Reach::Fixture).await
}

/// Which addresses a fetch may reach.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Reach {
    /// Public web addresses only.
    Web,
    /// Any address: a test's fixture server.
    #[cfg_attr(not(feature = "fixture-servers"), allow(dead_code))]
    Fixture,
}

async fn fetch_reaching(
    client: &tilo_net::Client,
    url: &str,
    limits: &FetchLimits,
    reach: Reach,
) -> Result<FetchedPage, FetchError> {
    let url = Url::parse(url.trim()).map_err(|e| FetchError::InvalidUrl(e.to_string()))?;
    tokio::time::timeout(limits.timeout, fetch_from(client, url, limits, reach))
        .await
        .unwrap_or(Err(FetchError::Timeout))
}

async fn fetch_from(
    client: &tilo_net::Client,
    mut url: Url,
    limits: &FetchLimits,
    reach: Reach,
) -> Result<FetchedPage, FetchError> {
    url.set_fragment(None);
    let mut hops = 0u8;
    let mut response = loop {
        if reach == Reach::Web {
            check_url(&url).map_err(FetchError::Refused)?;
            check_resolved(&url).await?;
        }
        let response = client
            .get(url.as_str())
            .header(
                "accept",
                "text/html,application/xhtml+xml,text/plain;q=0.9,application/json;q=0.8,*/*;q=0.1",
            )
            .send()
            .await
            .map_err(|e| FetchError::Network(e.to_string()))?;
        if !response.status().is_redirection() {
            break response;
        }
        hops = hops.saturating_add(1);
        if hops > limits.max_redirects {
            return Err(FetchError::Redirects);
        }
        url = response
            .headers()
            .get(tilo_net::header::LOCATION)
            .and_then(|location| location.to_str().ok())
            .and_then(|location| url.join(location).ok())
            .ok_or(FetchError::Redirects)?;
        url.set_fragment(None);
    };
    let status = response.status().as_u16();
    if !response.status().is_success() {
        return Err(FetchError::Status(status));
    }
    let content_type = response
        .headers()
        .get(tilo_net::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let (bytes, truncated) = read_bytes_capped(&mut response, limits.max_bytes)
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;
    let kind = match Kind::of(&content_type) {
        Kind::Unlabelled => Kind::sniff(&bytes),
        kind => kind,
    };
    let body = match kind {
        Kind::Binary => {
            if truncated {
                return Err(FetchError::TooLarge(limits.max_bytes));
            }
            FetchedBody::Binary {
                len: bytes.len(),
                bytes,
            }
        }
        Kind::Html | Kind::Unlabelled => {
            let page = readable::extract(&String::from_utf8_lossy(&bytes), &url);
            FetchedBody::Text {
                title: page.title,
                text: page.text,
            }
        }
        Kind::Json => FetchedBody::Text {
            title: None,
            text: pretty_json(&bytes, truncated),
        },
        Kind::Text => FetchedBody::Text {
            title: None,
            text: String::from_utf8_lossy(&bytes).trim().to_owned(),
        },
    };
    Ok(FetchedPage {
        url: url.into(),
        status,
        content_type: media_type(&content_type).to_owned(),
        truncated,
        body,
    })
}

/// JSON as indented text, so a reader sees its structure; a body cut short
/// or not valid JSON stays as sent.
fn pretty_json(bytes: &[u8], truncated: bool) -> String {
    let sent = || String::from_utf8_lossy(bytes).trim().to_owned();
    if truncated {
        return sent();
    }
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
        .unwrap_or_else(sent)
}

/// Refuse a name that resolves to this device or its network.
///
/// A lookup that fails is not a refusal: inside a sandbox without a resolver
/// the egress gate resolves instead, and it refuses local destinations
/// itself. A name that resolves to both public and local addresses is
/// refused, since the connection might take either.
async fn check_resolved(url: &Url) -> Result<(), FetchError> {
    let Some(url::Host::Domain(name)) = url.host() else {
        return Ok(());
    };
    let port = url.port_or_known_default().unwrap_or(443);
    let Ok(addresses) = tokio::net::lookup_host((name, port)).await else {
        return Ok(());
    };
    for address in addresses {
        check_address(address.ip())
            .map_err(|_| FetchError::Refused(Refusal::Local(name.to_owned())))?;
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Html,
    Json,
    Text,
    /// Not text: kept whole.
    Binary,
    /// No content type: decided by looking at the body.
    Unlabelled,
}

impl Kind {
    fn of(content_type: &str) -> Self {
        let media = media_type(content_type).to_ascii_lowercase();
        match media.as_str() {
            "" | "application/octet-stream" => Self::Unlabelled,
            "text/html" | "application/xhtml+xml" => Self::Html,
            "application/json" => Self::Json,
            _ if media.ends_with("+json") => Self::Json,
            "application/xml"
            | "application/rss+xml"
            | "application/atom+xml"
            | "application/javascript" => Self::Text,
            _ if media.ends_with("+xml") => Self::Text,
            _ if media.starts_with("text/") => Self::Text,
            _ => Self::Binary,
        }
    }

    /// What an unlabelled body is, from its first bytes: text is valid
    /// UTF-8 without NUL bytes, HTML text that says so.
    fn sniff(bytes: &[u8]) -> Self {
        let head = bytes.get(..bytes.len().min(1024)).unwrap_or_default();
        let text = match std::str::from_utf8(head) {
            Ok(text) => text,
            // A multi-byte character cut by the 1 KiB window is still text.
            Err(error) if error.error_len().is_none() => head
                .get(..error.valid_up_to())
                .and_then(|valid| std::str::from_utf8(valid).ok())
                .unwrap_or_default(),
            Err(_) => return Self::Binary,
        };
        if text.contains('\0') {
            return Self::Binary;
        }
        let start: String = text
            .trim_start()
            .chars()
            .take(256)
            .collect::<String>()
            .to_ascii_lowercase();
        if start.starts_with("<!doctype html") || start.contains("<html") {
            Self::Html
        } else if start.starts_with('{') || start.starts_with('[') {
            Self::Json
        } else {
            Self::Text
        }
    }
}

/// The media type of a `Content-Type` value, without its parameters.
fn media_type(content_type: &str) -> &str {
    content_type.split(';').next().unwrap_or("").trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_kinds_are_read_and_binary_ones_kept_whole() {
        assert_eq!(Kind::of("text/html; charset=utf-8"), Kind::Html);
        assert_eq!(Kind::of("application/xhtml+xml"), Kind::Html);
        assert_eq!(Kind::of("text/plain"), Kind::Text);
        assert_eq!(Kind::of("application/json; charset=utf-8"), Kind::Json);
        assert_eq!(Kind::of("application/ld+json"), Kind::Json);
        assert_eq!(Kind::of(""), Kind::Unlabelled);
        assert_eq!(Kind::of("application/pdf"), Kind::Binary);
        assert_eq!(Kind::of("image/png"), Kind::Binary);
        assert_eq!(Kind::sniff(b"  <!DOCTYPE html><p>x"), Kind::Html);
        assert_eq!(Kind::sniff(b"just words"), Kind::Text);
        assert_eq!(Kind::sniff(b"{\"a\": 1}"), Kind::Json);
        assert_eq!(Kind::sniff(b"%PDF-1.7\n\x00\xff\xfe"), Kind::Binary);
        assert_eq!(Kind::sniff(b"\x89PNG\r\n\x1a\n"), Kind::Binary);
    }

    #[test]
    fn json_is_indented_unless_cut_or_malformed() {
        assert_eq!(
            pretty_json(br#"{"name":"tilo","stars":[1,2]}"#, false),
            "{\n  \"name\": \"tilo\",\n  \"stars\": [\n    1,\n    2\n  ]\n}"
        );
        assert_eq!(pretty_json(br#"{"name":"ti"#, true), r#"{"name":"ti"#);
        assert_eq!(pretty_json(b"not json", false), "not json");
    }

    #[test]
    fn only_server_trouble_is_worth_retrying() {
        assert!(FetchError::Timeout.retryable());
        assert!(FetchError::Status(503).retryable());
        assert!(FetchError::Status(429).retryable());
        assert!(!FetchError::Status(404).retryable());
        assert!(!FetchError::Refused(Refusal::Credentials).retryable());
    }

    #[test]
    fn local_addresses_are_refused_before_any_request() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let client = crate::client().unwrap();
        for url in [
            "http://127.0.0.1:9/",
            "http://localhost/",
            "file:///etc/passwd",
            "not a url",
        ] {
            let error = runtime
                .block_on(fetch(&client, url, &FetchLimits::default()))
                .unwrap_err();
            assert!(
                matches!(error, FetchError::Refused(_) | FetchError::InvalidUrl(_)),
                "{url}: {error:?}"
            );
        }
    }
}
