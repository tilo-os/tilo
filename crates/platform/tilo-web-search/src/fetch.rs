//! Downloading one page and reducing it to readable text.

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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Whether the download stopped at [`FetchLimits::max_bytes`].
    pub truncated: bool,
    /// The page as Markdown-flavoured text: HTML reduced to its main content,
    /// plain text and JSON as sent.
    pub text: String,
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
    /// The page is not text: an image, a PDF, an archive.
    NotText(String),
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
            Self::InvalidUrl(_) | Self::Refused(_) | Self::NotText(_) | Self::Redirects => false,
        }
    }
}

impl fmt::Display for FetchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidUrl(reason) => write!(f, "not a web address: {reason}"),
            Self::Refused(refusal) => refusal.fmt(f),
            Self::Status(status) => write!(f, "the server answered HTTP {status}"),
            Self::NotText(kind) => write!(f, "the page is {kind}, not text"),
            Self::Redirects => f.write_str("too many redirects, or one to an unusable address"),
            Self::Timeout => f.write_str("the page took too long"),
            Self::Network(reason) => write!(f, "could not download the page: {reason}"),
        }
    }
}

impl std::error::Error for FetchError {}

/// Download `url` and reduce it to readable text.
///
/// Every hop, the first and each redirect, is checked by [`check_url`], and
/// where the name resolves here by [`check_address`], before a request goes
/// out, so a redirect cannot lead somewhere the first address could not.
pub async fn fetch(
    client: &tilo_net::Client,
    url: &str,
    limits: &FetchLimits,
) -> Result<FetchedPage, FetchError> {
    let url = Url::parse(url.trim()).map_err(|e| FetchError::InvalidUrl(e.to_string()))?;
    tokio::time::timeout(limits.timeout, fetch_from(client, url, limits))
        .await
        .unwrap_or(Err(FetchError::Timeout))
}

async fn fetch_from(
    client: &tilo_net::Client,
    mut url: Url,
    limits: &FetchLimits,
) -> Result<FetchedPage, FetchError> {
    url.set_fragment(None);
    let mut hops = 0u8;
    let mut response = loop {
        check_url(&url).map_err(FetchError::Refused)?;
        check_resolved(&url).await?;
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
    let kind = Kind::of(&content_type)
        .ok_or_else(|| FetchError::NotText(media_type(&content_type).to_owned()))?;
    let (bytes, truncated) = read_bytes_capped(&mut response, limits.max_bytes)
        .await
        .map_err(|e| FetchError::Network(e.to_string()))?;
    let body = String::from_utf8_lossy(&bytes);
    let kind = if kind == Kind::Unlabelled {
        Kind::sniff(&body)
    } else {
        kind
    };
    let (title, text) = match kind {
        Kind::Html | Kind::Unlabelled => {
            let page = readable::extract(&body, &url);
            (page.title, page.text)
        }
        Kind::Text => (None, body.trim().to_owned()),
    };
    Ok(FetchedPage {
        url: url.into(),
        status,
        content_type: media_type(&content_type).to_owned(),
        title,
        truncated,
        text,
    })
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
    Text,
    /// No content type: decided by looking at the body.
    Unlabelled,
}

impl Kind {
    fn of(content_type: &str) -> Option<Self> {
        let media = media_type(content_type).to_ascii_lowercase();
        match media.as_str() {
            "" => Some(Self::Unlabelled),
            "text/html" | "application/xhtml+xml" => Some(Self::Html),
            "application/json"
            | "application/xml"
            | "application/rss+xml"
            | "application/atom+xml"
            | "application/javascript" => Some(Self::Text),
            _ if media.ends_with("+json") || media.ends_with("+xml") => Some(Self::Text),
            _ if media.starts_with("text/") => Some(Self::Text),
            _ => None,
        }
    }

    fn sniff(body: &str) -> Self {
        let head: String = body
            .trim_start()
            .chars()
            .take(256)
            .collect::<String>()
            .to_ascii_lowercase();
        if head.starts_with("<!doctype html") || head.contains("<html") {
            Self::Html
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
    fn text_kinds_are_read_and_binary_ones_refused() {
        assert_eq!(Kind::of("text/html; charset=utf-8"), Some(Kind::Html));
        assert_eq!(Kind::of("application/xhtml+xml"), Some(Kind::Html));
        assert_eq!(Kind::of("text/plain"), Some(Kind::Text));
        assert_eq!(Kind::of("application/ld+json"), Some(Kind::Text));
        assert_eq!(Kind::of(""), Some(Kind::Unlabelled));
        assert_eq!(Kind::of("application/pdf"), None);
        assert_eq!(Kind::of("image/png"), None);
        assert_eq!(Kind::sniff("  <!DOCTYPE html><p>x"), Kind::Html);
        assert_eq!(Kind::sniff("just words"), Kind::Text);
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
