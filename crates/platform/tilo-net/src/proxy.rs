//! The host's same-origin fetch proxy: how a page reads cross-origin
//! responses that the site does not allow it to (CORS).
//!
//! A page's `fetch` only exposes a cross-origin response when the site sends
//! `Access-Control-Allow-Origin`. Most APIs and feeds do not, so an HTTP
//! tester or a feed reader in a page needs a relay. The host that serves the
//! page (Tilo Cloud; `tilo-web-desktop/scripts/serve.mjs` in development)
//! may offer one on its own origin. The app opts in per client or per request
//! with [`ProxyUse::CrossOrigin`]; the host enables it with [`configure`], or
//! by setting `globalThis.__tiloFetchProxy` to the endpoint (absolute or
//! relative to the page) before the runtime loads. Both are off by default,
//! and the desktop never uses it: there is no CORS there.
//!
//! # Wire protocol, version 1
//!
//! A proxied request goes to the configured endpoint, which must be on the
//! page's own origin:
//!
//! - `METHOD {endpoint}?url={target}`: the method and body are the target
//!   request's, and `target` is the percent-encoded absolute `http`/`https`
//!   URL.
//! - [`MARKER_HEADER`] is set, so a cross-site form cannot drive the proxy.
//! - Headers a browser refuses to send (`User-Agent`, `Cookie`, `Referer`,
//!   `Origin`, …) travel as [`FORWARD_PREFIX`]`<name>`; the proxy restores
//!   them. Every other header is sent as is. The page's own cookies are
//!   same-origin credentials for the proxy and are never forwarded.
//!
//! The proxy validates the target, refuses destinations on its own network
//! (loopback, private, link-local and unique-local addresses, re-checked on
//! every redirect hop), follows redirects itself, and bounds bodies and time.
//! It answers:
//!
//! - the target's status, headers and body, with hop-by-hop headers removed,
//!   each `Set-Cookie` and `WWW-Authenticate` renamed to
//!   [`FORWARD_PREFIX`]`<name>` (so the page origin never stores the cookie,
//!   and the browser never answers the challenge with a login prompt for the
//!   page's own origin, which blocks the tab's input until the request
//!   ends), and [`FINAL_URL_HEADER`] naming the URL that answered after
//!   redirects;
//! - or, when the proxy itself fails, a status with [`ERROR_HEADER`] set to
//!   one of [`ProxyFailure`]'s codes and a text body.
//!
//! [`Client`](crate::Client) does all of this: a proxied response reads like
//! a direct one.

use std::sync::RwLock;

use ::http::{HeaderMap, HeaderName, HeaderValue};
use url::Url;

use crate::NetError;

/// Marks a request as meant for the proxy; value `1`.
pub const MARKER_HEADER: &str = "x-tilo-proxy";
/// Prefix of headers the proxy restores to their real name, both ways.
pub const FORWARD_PREFIX: &str = "x-tilo-forward-";
/// The URL that answered, after the proxy followed redirects.
pub const FINAL_URL_HEADER: &str = "x-tilo-proxy-final-url";
/// Set on responses the proxy made itself; the value is a [`ProxyFailure`] code.
pub const ERROR_HEADER: &str = "x-tilo-proxy-error";
/// The protocol version in [`MARKER_HEADER`].
pub const VERSION: &str = "1";

/// Whether a request may go through the host's fetch proxy.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum ProxyUse {
    /// Always fetch directly. The default.
    #[default]
    Never,
    /// In a page, send cross-origin requests through the host's proxy when
    /// the host has configured one; fetch directly otherwise, and always on
    /// the desktop.
    CrossOrigin,
}

/// Why the proxy answered instead of the target, from [`ERROR_HEADER`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ProxyFailure {
    /// `bad-request`: the target URL is missing, malformed or not http(s).
    BadRequest,
    /// `blocked`: the destination is on the proxy's own network.
    Blocked,
    /// `unreachable`: the target could not be reached.
    Unreachable,
    /// `timeout`: the target did not answer in time.
    Timeout,
    /// `too-large`: a body exceeded the proxy's limit.
    TooLarge,
    /// `unauthorized`: the page is not signed in to the host.
    Unauthorized,
}

impl ProxyFailure {
    /// The code in [`ERROR_HEADER`].
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::BadRequest => "bad-request",
            Self::Blocked => "blocked",
            Self::Unreachable => "unreachable",
            Self::Timeout => "timeout",
            Self::TooLarge => "too-large",
            Self::Unauthorized => "unauthorized",
        }
    }

    /// The HTTP status a proxy answers with.
    #[must_use]
    pub const fn status(self) -> u16 {
        match self {
            Self::BadRequest => 400,
            Self::Blocked => 403,
            Self::Unreachable => 502,
            Self::Timeout => 504,
            Self::TooLarge => 413,
            Self::Unauthorized => 401,
        }
    }

    /// The failure for a code, if it is one.
    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        [
            Self::BadRequest,
            Self::Blocked,
            Self::Unreachable,
            Self::Timeout,
            Self::TooLarge,
            Self::Unauthorized,
        ]
        .into_iter()
        .find(|failure| failure.code() == code)
    }

    fn into_error(self, message: String) -> NetError {
        match self {
            Self::Blocked => NetError::Blocked(message),
            Self::Timeout => NetError::Timeout,
            Self::TooLarge => NetError::TooLarge { limit: 0 },
            Self::BadRequest => NetError::InvalidUrl(message),
            Self::Unreachable | Self::Unauthorized => NetError::connect(message),
        }
    }
}

/// Who names the endpoint.
#[derive(Clone)]
enum Decision {
    /// The page, until the host calls [`configure`].
    Page,
    /// The host: an endpoint, or no proxy.
    Host(Option<Url>),
}

static ENDPOINT: RwLock<Decision> = RwLock::new(Decision::Page);

/// Whether `endpoint` may be a proxy for this page.
fn check_endpoint(endpoint: &Url) -> Result<(), NetError> {
    if !matches!(endpoint.scheme(), "http" | "https") {
        return Err(NetError::InvalidUrl(format!(
            "{endpoint}: the fetch proxy must be an http(s) endpoint"
        )));
    }
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    if crate::web::page_origin().is_some_and(|origin| origin != endpoint.origin()) {
        return Err(NetError::InvalidUrl(format!(
            "{endpoint}: the fetch proxy must be on the page's own origin"
        )));
    }
    Ok(())
}

/// Enable the host's proxy at `endpoint`, or disable it with `None`.
///
/// Hosts call this, not apps. In a page the endpoint must be on the page's
/// own origin, because every proxied request carries the app's headers and
/// body there.
///
/// # Errors
///
/// [`NetError::InvalidUrl`] for an endpoint that is not http(s) or, in a
/// page, not on the page's origin.
pub fn configure(endpoint: Option<Url>) -> Result<(), NetError> {
    if let Some(endpoint) = &endpoint {
        check_endpoint(endpoint)?;
    }
    *ENDPOINT
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Decision::Host(endpoint);
    Ok(())
}

/// The endpoint in use: the host's [`configure`] choice, else the page's
/// `globalThis.__tiloFetchProxy`, else none.
#[must_use]
pub fn endpoint() -> Option<Url> {
    let decision = ENDPOINT
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    match decision {
        Decision::Page => page_default(),
        Decision::Host(endpoint) => endpoint,
    }
}

/// The endpoint the page names, read once.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn page_default() -> Option<Url> {
    static DEFAULT: std::sync::OnceLock<Option<Url>> = std::sync::OnceLock::new();
    DEFAULT
        .get_or_init(|| {
            let named = crate::web::global_get("__tiloFetchProxy")?.as_string()?;
            let endpoint = crate::web::page_url()?.join(&named).ok()?;
            check_endpoint(&endpoint).ok()?;
            Some(endpoint)
        })
        .clone()
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn page_default() -> Option<Url> {
    None
}

/// Headers a browser will not let a page set; they travel under
/// [`FORWARD_PREFIX`].
fn browser_forbidden(name: &HeaderName) -> bool {
    const FORBIDDEN: &[&str] = &[
        "accept-charset",
        "accept-encoding",
        "access-control-request-headers",
        "access-control-request-method",
        "connection",
        "content-length",
        "cookie",
        "date",
        "dnt",
        "expect",
        "host",
        "keep-alive",
        "origin",
        "referer",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        "user-agent",
        "via",
    ];
    let name = name.as_str();
    FORBIDDEN.contains(&name) || name.starts_with("proxy-") || name.starts_with("sec-")
}

/// The proxy URL and headers for a request to `target`.
///
/// # Errors
///
/// [`NetError::InvalidRequest`] when a forwarded header name cannot be
/// formed.
pub fn route(
    endpoint: &Url,
    target: &Url,
    headers: &HeaderMap,
) -> Result<(Url, HeaderMap), NetError> {
    let mut url = endpoint.clone();
    url.query_pairs_mut().append_pair("url", target.as_str());
    let mut routed = HeaderMap::with_capacity(headers.len().saturating_add(1));
    routed.insert(
        HeaderName::from_static(MARKER_HEADER),
        HeaderValue::from_static(VERSION),
    );
    for (name, value) in headers {
        let name = if browser_forbidden(name) {
            HeaderName::try_from(format!("{FORWARD_PREFIX}{name}"))
                .map_err(|error| NetError::InvalidRequest(error.to_string()))?
        } else {
            name.clone()
        };
        routed.append(name, value.clone());
    }
    Ok((url, routed))
}

/// Undo [`route`] on a response: the target's headers and the URL that
/// answered, or the proxy's own failure.
///
/// # Errors
///
/// The [`ProxyFailure`] as a [`NetError`], with `body` as its message.
pub fn unwrap_response(
    target: &Url,
    headers: &HeaderMap,
    body_if_failed: impl FnOnce() -> String,
) -> Result<(HeaderMap, Url), NetError> {
    if let Some(code) = headers.get(ERROR_HEADER) {
        let failure = code
            .to_str()
            .ok()
            .and_then(ProxyFailure::from_code)
            .unwrap_or(ProxyFailure::Unreachable);
        return Err(failure.into_error(body_if_failed()));
    }
    let mut url = target.clone();
    let mut unwrapped = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        if name == FINAL_URL_HEADER {
            if let Some(final_url) = value.to_str().ok().and_then(|v| Url::parse(v).ok()) {
                url = final_url;
            }
        } else if let Some(original) = name.as_str().strip_prefix(FORWARD_PREFIX) {
            if let Ok(original) = HeaderName::try_from(original) {
                unwrapped.append(original, value.clone());
            }
        } else {
            unwrapped.append(name.clone(), value.clone());
        }
    }
    Ok((unwrapped, url))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn a_request_routes_through_the_endpoint() {
        let mut headers = HeaderMap::new();
        headers.insert("accept", HeaderValue::from_static("application/json"));
        headers.insert("user-agent", HeaderValue::from_static("Tilo"));
        headers.insert("cookie", HeaderValue::from_static("a=1"));
        let (routed, sent) = route(
            &url("https://cloud.example/_tilo/net/fetch"),
            &url("https://api.example/v1/items?page=2&q=a b"),
            &headers,
        )
        .unwrap();
        assert_eq!(
            routed.as_str(),
            "https://cloud.example/_tilo/net/fetch?url=https%3A%2F%2Fapi.example%2Fv1%2Fitems%3Fpage%3D2%26q%3Da%2520b"
        );
        let target: String = routed
            .query_pairs()
            .find(|(k, _)| k == "url")
            .map(|(_, v)| v.into_owned())
            .unwrap();
        assert_eq!(target, "https://api.example/v1/items?page=2&q=a%20b");
        assert_eq!(sent[MARKER_HEADER], VERSION);
        assert_eq!(sent["accept"], "application/json");
        assert_eq!(sent["x-tilo-forward-user-agent"], "Tilo");
        assert_eq!(sent["x-tilo-forward-cookie"], "a=1");
        assert!(!sent.contains_key("user-agent"));
    }

    #[test]
    fn a_response_unwraps_to_the_target_view() {
        let mut headers = HeaderMap::new();
        headers.insert("content-type", HeaderValue::from_static("text/xml"));
        headers.insert(
            FINAL_URL_HEADER,
            HeaderValue::from_static("https://api.example/moved"),
        );
        headers.append("x-tilo-forward-set-cookie", HeaderValue::from_static("s=1"));
        headers.append(
            "x-tilo-forward-www-authenticate",
            HeaderValue::from_static("Basic realm=\"api\""),
        );
        let (unwrapped, final_url) =
            unwrap_response(&url("https://api.example/"), &headers, String::new).unwrap();
        assert_eq!(final_url.as_str(), "https://api.example/moved");
        assert_eq!(unwrapped["set-cookie"], "s=1");
        assert_eq!(unwrapped["www-authenticate"], "Basic realm=\"api\"");
        assert_eq!(unwrapped["content-type"], "text/xml");
        assert!(!unwrapped.contains_key(FINAL_URL_HEADER));
    }

    #[test]
    fn a_proxy_failure_is_an_error_not_a_response() {
        let mut headers = HeaderMap::new();
        headers.insert(ERROR_HEADER, HeaderValue::from_static("blocked"));
        let failed = unwrap_response(&url("http://10.0.0.1/"), &headers, || {
            "private address".into()
        });
        assert_eq!(failed, Err(NetError::Blocked("private address".into())));
        for failure in [
            ProxyFailure::BadRequest,
            ProxyFailure::Blocked,
            ProxyFailure::Unreachable,
            ProxyFailure::Timeout,
            ProxyFailure::TooLarge,
            ProxyFailure::Unauthorized,
        ] {
            assert_eq!(ProxyFailure::from_code(failure.code()), Some(failure));
        }
    }

    #[test]
    fn configure_refuses_non_http_endpoints() {
        assert!(configure(Some(url("ftp://x/"))).is_err());
        assert_eq!(endpoint(), None);
    }
}
