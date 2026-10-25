//! The host's fetch proxy: how a page reads cross-origin responses that the
//! site does not allow it to (CORS).
//!
//! A page's `fetch` only exposes a cross-origin response when the site sends
//! `Access-Control-Allow-Origin`. Most APIs and feeds do not, so an HTTP
//! tester or a feed reader in a page needs a relay. The workspace host the
//! page is admitted to offers one: `host` is its implementation, which
//! `tilo-relay` serves and, in development, the `tilo-fetch-proxy` program
//! behind `tilo-web-desktop/scripts/serve.mjs`. The app opts in per client
//! or per request with [`ProxyUse::CrossOrigin`]; the host enables it with
//! [`configure`], or the page names it in `globalThis.__tiloFetchProxy` (see
//! [`current`]). Both are off by default, and the desktop never uses it:
//! there is no CORS there.
//!
//! # Wire protocol, version 1
//!
//! A proxied request goes to the configured endpoint:
//!
//! - `METHOD {endpoint}?url={target}`: the method and body are the target
//!   request's, and `target` is the percent-encoded absolute `http`/`https`
//!   URL.
//! - [`MARKER_HEADER`] is set, so a cross-site form cannot drive the proxy.
//! - [`SESSION_HEADER`] carries the page's session at a host on another
//!   origin, whose cookies a page cannot send; a host on the page's own
//!   origin may authenticate it by its cookies instead.
//! - Headers a browser refuses to send (`User-Agent`, `Cookie`, `Referer`,
//!   `Origin`, …) travel as [`FORWARD_PREFIX`]`<name>`; the proxy restores
//!   them. Every other header is sent as is. The page's own cookies and
//!   session are credentials for the proxy and are never forwarded.
//!
//! The proxy validates the target, refuses destinations on its own network
//! (loopback, private, link-local and unique-local addresses, re-checked on
//! every redirect hop), follows redirects itself, and bounds bodies, time and
//! how much each device may ask of it. It answers:
//!
//! - the target's status, headers and body, with hop-by-hop headers removed,
//!   each header the host's origin would act on renamed to
//!   [`FORWARD_PREFIX`]`<name>`: `Set-Cookie`, so that origin never stores
//!   the cookie; `WWW-Authenticate`, so the browser never answers the
//!   challenge with a login prompt for that origin, which blocks the tab's
//!   input until the request ends; and caching, security-policy and
//!   site-data headers, which would apply to it. [`FINAL_URL_HEADER`] names
//!   the URL that answered after redirects;
//! - or, when the proxy itself fails, a status with [`ERROR_HEADER`] set to
//!   one of [`ProxyFailure`]'s codes and a text body.
//!
//! [`Client`](crate::Client) does all of this: a proxied response reads like
//! a direct one.

#[cfg(all(feature = "proxy-host", not(target_family = "wasm")))]
pub mod host;

use std::sync::RwLock;

use ::http::{HeaderMap, HeaderName, HeaderValue};
use url::Url;

use crate::NetError;
use tilo_lock::RwLockRecover as _;

/// Marks a request as meant for the proxy; value `1`.
pub const MARKER_HEADER: &str = "x-tilo-proxy";
/// Prefix of headers the proxy restores to their real name, both ways.
pub const FORWARD_PREFIX: &str = "x-tilo-forward-";
/// The URL that answered, after the proxy followed redirects.
pub const FINAL_URL_HEADER: &str = "x-tilo-proxy-final-url";
/// Set on responses the proxy made itself; the value is a [`ProxyFailure`] code.
pub const ERROR_HEADER: &str = "x-tilo-proxy-error";
/// The page's session at the host, for a host on another origin (lowercase
/// hex); the same header the host's browser routes take.
pub const SESSION_HEADER: &str = "x-tilo-session";
/// The protocol version in [`MARKER_HEADER`].
pub const VERSION: &str = "1";
/// [`MARKER_HEADER`] as a query parameter, for a `GET` or `HEAD` from a page
/// element that cannot send headers: a media element streaming a video.
pub const MARKER_PARAM: &str = "tilo-proxy";
/// [`SESSION_HEADER`] as a query parameter, beside [`MARKER_PARAM`].
pub const SESSION_PARAM: &str = "tilo-session";
/// What [`PEER_PARAM`] says: the target URL is not a site on the web but a
/// path on the machine that admitted the page's person, which its own `fs`
/// answered when it was still the page (`docs/architecture/network.md`).
pub const PEER_LOCAL: &str = "local";
/// Asks the host to reach the target as the local path it is on its own
/// machine (`{"path": "/home/ada/Notes"}`) rather than as a URL.
pub const PEER_PARAM: &str = "tilo-peer";

/// The proxy URL and headers for a request to `path` on the machine that
/// admitted the page's person, through `proxy`, whose host answers it as
/// the path it is on its own disk.
///
/// The target is written as the host reads it, `file://` and all, so a
/// host validates one URL either way; [`PEER_PARAM`] says it is this
/// machine's own path and not a site.
///
/// # Errors
///
/// See [`route`].
pub fn peer_route(
    proxy: &Proxy,
    path: &str,
    headers: &HeaderMap,
) -> Result<(Url, HeaderMap), NetError> {
    let mut url = proxy.endpoint.clone();
    url.query_pairs_mut()
        .append_pair("url", &format!("file://{path}"))
        .append_pair(PEER_PARAM, PEER_LOCAL);
    Ok((url, proxied_headers(proxy, headers)?))
}

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
    /// `rate-limited`: this device, or everyone together, asked too much of
    /// the proxy lately; `Retry-After` says when to try again.
    RateLimited,
    /// `over-budget`: the proxy relayed as much as it allows this visitor,
    /// or everyone in its room together, for now (a day, by default);
    /// `Retry-After` says when the budget turns over. A media element shows
    /// people this one, so the page can say why playback stopped.
    OverBudget,
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
            Self::RateLimited => "rate-limited",
            Self::OverBudget => "over-budget",
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
            Self::RateLimited | Self::OverBudget => 429,
        }
    }

    #[must_use]
    pub fn from_code(code: &str) -> Option<Self> {
        [
            Self::BadRequest,
            Self::Blocked,
            Self::Unreachable,
            Self::Timeout,
            Self::TooLarge,
            Self::Unauthorized,
            Self::RateLimited,
            Self::OverBudget,
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
            Self::RateLimited | Self::OverBudget => {
                NetError::Status(::http::StatusCode::TOO_MANY_REQUESTS)
            }
            Self::Unreachable | Self::Unauthorized => NetError::connect(message),
        }
    }
}

/// The proxy a page uses now: where it is, and the page's session there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proxy {
    /// The endpoint, `…/fetch` on the host.
    pub endpoint: Url,
    /// The page's session at the host, sent as [`SESSION_HEADER`]; `None`
    /// for a host that authenticates the page by its cookies.
    pub session: Option<String>,
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

fn check_endpoint(endpoint: &Url) -> Result<(), NetError> {
    if !matches!(endpoint.scheme(), "http" | "https") {
        return Err(NetError::InvalidUrl(format!(
            "{endpoint}: the fetch proxy must be an http(s) endpoint"
        )));
    }
    Ok(())
}

/// Enable the host's proxy at `endpoint`, or disable it with `None`.
///
/// Hosts call this, not apps: every proxied request carries the app's
/// headers and body to the endpoint, so it must be the page's own host or
/// the workspace host the page is admitted to.
///
/// # Errors
///
/// [`NetError::InvalidUrl`] for an endpoint that is not http(s).
pub fn configure(endpoint: Option<Url>) -> Result<(), NetError> {
    if let Some(endpoint) = &endpoint {
        check_endpoint(endpoint)?;
    }
    *ENDPOINT.write_recover() = Decision::Host(endpoint);
    Ok(())
}

/// The proxy in use: the host's [`configure`] choice, else what the page
/// names in `globalThis.__tiloFetchProxy`, else none.
///
/// The page names it either as a string, the endpoint (absolute or relative
/// to the page) of a host on its own origin, or as a function answering
/// `{endpoint, session}` (or nothing while there is no proxy), which the
/// browser runtime installs once the workspace host the page is admitted to
/// offers one: the session then follows the page's reconnects. It is read on
/// every request.
#[must_use]
pub fn current() -> Option<Proxy> {
    let decision = ENDPOINT.read_recover().clone();
    match decision {
        Decision::Page => page_default(),
        Decision::Host(endpoint) => endpoint.map(|endpoint| Proxy {
            endpoint,
            session: None,
        }),
    }
}

/// The endpoint of the proxy in use, see [`current`].
#[must_use]
pub fn endpoint() -> Option<Url> {
    current().map(|proxy| proxy.endpoint)
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn page_default() -> Option<Proxy> {
    use wasm_bindgen::JsCast as _;
    let named = crate::web::global_get("__tiloFetchProxy")?;
    let (endpoint, session) = if let Some(function) = named.dyn_ref::<js_sys::Function>() {
        let answer = function.call0(&wasm_bindgen::JsValue::NULL).ok()?;
        let endpoint = crate::web::get(&answer, "endpoint")?.as_string()?;
        let session = crate::web::get(&answer, "session").and_then(|value| value.as_string());
        (endpoint, session)
    } else {
        (named.as_string()?, None)
    };
    let endpoint = crate::web::page_url()?.join(&endpoint).ok()?;
    check_endpoint(&endpoint).ok()?;
    Some(Proxy { endpoint, session })
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn page_default() -> Option<Proxy> {
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

/// The proxy URL and headers for a request to `target` through `proxy`.
///
/// # Errors
///
/// [`NetError::InvalidRequest`] when a forwarded header name cannot be
/// formed, or the session is not a header value.
pub fn route(
    proxy: &Proxy,
    target: &Url,
    headers: &HeaderMap,
) -> Result<(Url, HeaderMap), NetError> {
    let mut url = proxy.endpoint.clone();
    url.query_pairs_mut().append_pair("url", target.as_str());
    Ok((url, proxied_headers(proxy, headers)?))
}

/// The headers of a request the host's proxy relays: the marker, the
/// page's session, and the request's own — the ones a browser will not let
/// a page set renamed under [`FORWARD_PREFIX`].
fn proxied_headers(proxy: &Proxy, headers: &HeaderMap) -> Result<HeaderMap, NetError> {
    let mut routed = HeaderMap::with_capacity(headers.len().saturating_add(2));
    routed.insert(
        HeaderName::from_static(MARKER_HEADER),
        HeaderValue::from_static(VERSION),
    );
    if let Some(session) = &proxy.session {
        routed.insert(
            HeaderName::from_static(SESSION_HEADER),
            HeaderValue::from_str(session)
                .map_err(|error| NetError::InvalidRequest(error.to_string()))?,
        );
    }
    for (name, value) in headers {
        let name = if browser_forbidden(name) {
            HeaderName::try_from(format!("{FORWARD_PREFIX}{name}"))
                .map_err(|error| NetError::InvalidRequest(error.to_string()))?
        } else {
            name.clone()
        };
        routed.append(name, value.clone());
    }
    Ok(routed)
}

/// The URL a page element that cannot send headers (a media element) loads
/// `target` from through `proxy`: the marker and the session ride in the
/// query ([`MARKER_PARAM`], [`SESSION_PARAM`]). Only `GET` and `HEAD` are
/// taken this way, and an open-ended byte range is answered a piece at a
/// time, as a media element asks for more.
#[must_use]
pub fn media_url(proxy: &Proxy, target: &Url) -> Url {
    let mut url = proxy.endpoint.clone();
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("url", target.as_str());
        query.append_pair(MARKER_PARAM, VERSION);
        if let Some(session) = &proxy.session {
            query.append_pair(SESSION_PARAM, session);
        }
    }
    url
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
        let proxy = Proxy {
            endpoint: url("https://cloud.example/_tilo/net/fetch"),
            session: Some("ab12".into()),
        };
        let (routed, sent) = route(
            &proxy,
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
        assert_eq!(sent[SESSION_HEADER], "ab12");
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
            ProxyFailure::RateLimited,
            ProxyFailure::OverBudget,
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
