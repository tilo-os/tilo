//! Request execution.
//!
//! [`build_outgoing`] resolves a [`Request`] against the active
//! environment's variables into a concrete, ready-to-send shape (URL with
//! query string, header list, body bytes). [`send`] then performs the
//! round-trip through `tilo_net` and returns a [`ResponseData`]. The
//! builder is pure and unit-tested; the network call is isolated so the
//! rest of the app never depends on a live connection — the screenshot
//! seed uses [`sample_response`] instead.

use std::collections::HashMap;
use std::sync::OnceLock;

use base64::Engine;

use crate::model::{Auth, BodyMode, KeyValue, Request, ResponseData};
use crate::vars;

/// A fully-resolved request, with all `{{vars}}` expanded and auth folded
/// into the header list. This is the exact thing that hits the wire.
#[derive(Clone, PartialEq, Eq)]
pub struct Outgoing {
    pub method: String,
    /// Full URL including the assembled query string.
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

impl std::fmt::Debug for Outgoing {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Outgoing")
            .field("method", &self.method)
            .finish_non_exhaustive()
    }
}

/// Appends the encoded `query` pairs to `base`, keeping any `#fragment` last.
fn append_query(base: String, query: &[String]) -> String {
    if query.is_empty() {
        base
    } else {
        // Fragments are not sent to the server; params must precede them.
        let (target, fragment) = base
            .split_once('#')
            .map_or((base.as_str(), None), |(target, fragment)| {
                (target, Some(fragment))
            });
        let separator = if target.ends_with('?') || (target.contains('?') && target.ends_with('&'))
        {
            ""
        } else if target.contains('?') {
            "&"
        } else {
            "?"
        };
        let mut url = format!("{target}{separator}{}", query.join("&"));
        if let Some(fragment) = fragment {
            url.push('#');
            url.push_str(fragment);
        }
        url
    }
}

/// Resolve a [`Request`] against `env_vars` into a concrete [`Outgoing`].
/// Query params, headers, the chosen auth scheme, and the body are all
/// substituted and assembled here.
pub fn build_outgoing(req: &Request, env_vars: &HashMap<String, String>) -> Outgoing {
    let sub = |s: &str| vars::substitute(s, env_vars);

    // Assemble the URL with its query string from enabled params.
    let base = sub(&req.url);
    let query: Vec<String> = req
        .params
        .iter()
        .filter(|p| p.enabled && !p.key.trim().is_empty())
        .map(|p| {
            format!(
                "{}={}",
                ui::uri::encode_component(&sub(&p.key)),
                ui::uri::encode_component(&sub(&p.value))
            )
        })
        .collect();
    let url = append_query(base, &query);

    // Headers from the request, then auth-derived headers.
    let mut headers: Vec<(String, String)> = req
        .headers
        .iter()
        .filter(|h| h.enabled && !h.key.trim().is_empty())
        .map(|h| (sub(&h.key), sub(&h.value)))
        .collect();

    match &req.auth {
        Auth::None | Auth::SecretRef { .. } => {}
        Auth::Bearer { token } => {
            headers.push(("Authorization".into(), format!("Bearer {}", sub(token))));
        }
        Auth::Basic { username, password } => {
            let raw = format!("{}:{}", sub(username), sub(password));
            let encoded = base64::engine::general_purpose::STANDARD.encode(raw.as_bytes());
            headers.push(("Authorization".into(), format!("Basic {encoded}")));
        }
        Auth::ApiKey { header, value } => {
            if !header.trim().is_empty() {
                headers.push((sub(header), sub(value)));
            }
        }
    }

    // Body, by mode.
    let body = match req.body_mode {
        BodyMode::None => None,
        BodyMode::Json => {
            if !headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            {
                headers.push(("Content-Type".into(), "application/json".into()));
            }
            Some(sub(&req.body_text))
        }
        BodyMode::Raw => Some(sub(&req.body_text)),
        BodyMode::Form => {
            if !headers
                .iter()
                .any(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            {
                headers.push((
                    "Content-Type".into(),
                    "application/x-www-form-urlencoded".into(),
                ));
            }
            let pairs: Vec<String> = req
                .form
                .iter()
                .filter(|f| f.enabled && !f.key.trim().is_empty())
                .map(|f| {
                    format!(
                        "{}={}",
                        ui::uri::encode_component(&sub(&f.key)),
                        ui::uri::encode_component(&sub(&f.value))
                    )
                })
                .collect();
            Some(pairs.join("&"))
        }
    };

    Outgoing {
        method: req.method.as_str().to_string(),
        url,
        headers,
        body,
    }
}

/// The shared client. In a page, requests to sites that do not allow the
/// page to read their responses go through the host's fetch proxy when it
/// offers one.
/// The longest one request may take, from sending it to the last byte of its
/// body. A server that never answers is a failure with a reason, not an app
/// that waits forever with its only primary action disabled.
pub const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// How often a request in flight looks at its cancellation flag.
const CANCEL_SLICE: std::time::Duration = std::time::Duration::from_millis(50);

/// Why a request stopped before the server answered.
pub const CANCELLED: &str = "The request was cancelled.";

fn client() -> Result<&'static tilo_net::Client, String> {
    static CLIENT: OnceLock<Result<tilo_net::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            tilo_net::Client::builder()
                .user_agent("tilo-api-tester/0.1")
                .timeout(REQUEST_TIMEOUT)
                .fetch_proxy(tilo_net::proxy::ProxyUse::CrossOrigin)
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Perform the request and collect the response, giving up when
/// `cancelled` is set (the person pressed Cancel, or the app is closing) or
/// when the request outlives [`REQUEST_TIMEOUT`]. The caller runs it on the
/// app runtime so the UI stays responsive.
pub async fn send(
    out: &Outgoing,
    cancelled: &std::sync::atomic::AtomicBool,
) -> Result<ResponseData, String> {
    let request = send_once(out);
    let watch = watch_for_cancel(cancelled);
    match race(request, watch).await {
        Ok(response) => response,
        Err(Cancelled) => Err(CANCELLED.to_owned()),
    }
}

/// Wait in slices, reporting as soon as `cancelled` is set.
async fn watch_for_cancel(cancelled: &std::sync::atomic::AtomicBool) -> Cancelled {
    loop {
        ui::rt::sleep(CANCEL_SLICE).await;
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Cancelled;
        }
    }
}

/// A request that was cancelled.
struct Cancelled;

/// Which of two futures finished first, dropping the other. Written out
/// rather than pulled in: the toolkit has no `select` of its own, and this
/// app needs exactly one.
async fn race<T>(
    work: impl std::future::Future<Output = T>,
    stop: impl std::future::Future<Output = Cancelled>,
) -> Result<T, Cancelled> {
    let mut work = std::pin::pin!(work);
    let mut stop = std::pin::pin!(stop);
    std::future::poll_fn(move |cx| {
        if let std::task::Poll::Ready(value) = work.as_mut().poll(cx) {
            return std::task::Poll::Ready(Ok(value));
        }
        if let std::task::Poll::Ready(stop) = stop.as_mut().poll(cx) {
            return std::task::Poll::Ready(Err(stop));
        }
        std::task::Poll::Pending
    })
    .await
}

/// Perform the request and collect the response.
async fn send_once(out: &Outgoing) -> Result<ResponseData, String> {
    let client = client()?;
    let method = tilo_net::Method::from_bytes(out.method.as_bytes())
        .map_err(|e| format!("invalid method: {e}"))?;
    let mut builder = client.request(method, &out.url);
    for (k, v) in &out.headers {
        builder = builder.header(k.as_str(), v.as_str());
    }
    if let Some(body) = &out.body {
        builder = builder.body(body.clone());
    }

    let started = ui::time::now();
    let mut resp = builder.send().await.map_err(|e| describe(&e))?;
    let status = resp.status();
    let status_text = status.canonical_reason().unwrap_or("").to_string();
    let headers: Vec<KeyValue> = resp
        .headers()
        .iter()
        .map(|(k, v)| KeyValue::new(k.as_str(), v.to_str().unwrap_or("").to_string()))
        .collect();
    let content_type = headers
        .iter()
        .find(|h| h.key.eq_ignore_ascii_case("content-type"))
        .map(|h| h.value.clone())
        .unwrap_or_default();
    let declared = resp.content_length();
    let (bytes, complete) = read_shown(&mut resp).await?;
    let duration_ms = started.elapsed().as_millis() as u64;
    let received = bytes.len();
    let size_bytes = match (complete, declared.and_then(|n| usize::try_from(n).ok())) {
        (true, _) | (false, None) => received,
        (false, Some(declared)) => declared.max(received),
    };
    let shown = ShownBody::of(bytes, complete, textual(&content_type));

    Ok(ResponseData {
        status: status.as_u16(),
        status_text,
        duration_ms,
        size_bytes,
        size_exact: complete || declared.is_some(),
        shown_bytes: shown.bytes,
        binary: shown.binary,
        headers,
        body: shown.text,
        content_type,
    })
}

/// What a sent request answers whoever sent it: the response as the pane
/// shows it, with its headers by name.
#[must_use]
pub fn answer(response: &ResponseData) -> serde_json::Value {
    serde_json::json!({
        "status": response.status,
        "status_text": response.status_text,
        "duration_ms": response.duration_ms,
        "content_type": response.content_type,
        "headers": response
            .headers
            .iter()
            .map(|header| serde_json::json!({"name": header.key, "value": header.value}))
            .collect::<Vec<_>>(),
        "size_bytes": response.size_bytes,
        "size_exact": response.size_exact,
        "shown_bytes": response.shown_bytes,
        "binary": response.binary,
        "body": response.body,
    })
}

/// The most of a response body the app downloads to show (2 MiB): the
/// response pane lays its text out line by line, so a download of gigabytes
/// would hold all of it in memory and freeze the window. The rest is never
/// read; the pane says how much it shows.
pub const MAX_SHOWN_BODY_BYTES: usize = 2 * 1024 * 1024;

/// The most of a binary body shown as hex (64 KiB): each byte takes a few
/// characters of the dump.
pub const MAX_HEX_BYTES: usize = 64 * 1024;

/// The body up to [`MAX_SHOWN_BODY_BYTES`], and whether that is all of it.
async fn read_shown(resp: &mut tilo_net::Response) -> Result<(Vec<u8>, bool), String> {
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(|e| describe(&e))? {
        let room = MAX_SHOWN_BODY_BYTES.saturating_sub(body.len());
        if chunk.len() > room {
            body.extend_from_slice(chunk.get(..room).unwrap_or_default());
            return Ok((body, false));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((body, true))
}

/// A body as the response pane shows it.
struct ShownBody {
    text: String,
    /// How many of the body's bytes `text` shows.
    bytes: usize,
    binary: bool,
}

impl ShownBody {
    /// Text as text; anything else that is not UTF-8 as a hex dump of its
    /// start. A body cut short may end inside a character, which is not
    /// binary, and a `textual` one in another encoding is shown with its
    /// stray bytes replaced.
    fn of(bytes: Vec<u8>, complete: bool, textual: bool) -> Self {
        match String::from_utf8(bytes) {
            Ok(text) => Self {
                bytes: text.len(),
                text,
                binary: false,
            },
            Err(error) => {
                let utf8 = error.utf8_error();
                let bytes = error.into_bytes();
                if textual || (!complete && utf8.error_len().is_none()) {
                    let end = if textual {
                        bytes.len()
                    } else {
                        utf8.valid_up_to()
                    };
                    let shown = bytes.get(..end).unwrap_or_default();
                    return Self {
                        bytes: shown.len(),
                        text: String::from_utf8_lossy(shown).into_owned(),
                        binary: false,
                    };
                }
                let dumped = bytes.get(..MAX_HEX_BYTES).unwrap_or(&bytes);
                Self {
                    text: hex_dump(dumped),
                    bytes: dumped.len(),
                    binary: true,
                }
            }
        }
    }
}

/// Whether `content_type` names text, which is shown as text even when it
/// is not UTF-8.
fn textual(content_type: &str) -> bool {
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    essence.starts_with("text/")
        || ["json", "xml", "javascript", "x-www-form-urlencoded"]
            .iter()
            .any(|kind| essence.contains(kind))
}

/// `bytes` as lines of sixteen: the offset, the bytes in hex, and the bytes
/// that are printable ASCII (a dot for the rest).
fn hex_dump(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len().div_ceil(16).saturating_mul(78));
    for (row, line) in bytes.chunks(16).enumerate() {
        let _ = write!(out, "{:08x} ", row.saturating_mul(16));
        for column in 0..16 {
            match line.get(column) {
                Some(byte) => {
                    let _ = write!(out, " {byte:02x}");
                }
                None => out.push_str("   "),
            }
        }
        out.push_str("  ");
        out.extend(line.iter().map(|&byte| {
            if byte.is_ascii_graphic() || byte == b' ' {
                char::from(byte)
            } else {
                '.'
            }
        }));
        out.push('\n');
    }
    out
}

/// A request error for the response pane, naming the fix for a browser that
/// refused a cross-origin response.
fn describe(error: &tilo_net::NetError) -> String {
    if error.may_be_cors() && tilo_net::proxy::endpoint().is_none() {
        format!(
            "{error}. In a browser, this host has no fetch proxy to relay requests to sites that do not allow it."
        )
    } else {
        error.to_string()
    }
}

/// A representative populated response used to seed the headless
/// screenshot (and the first-run example). Mirrors a real JSON API reply
/// so the response viewer renders pretty-printed, highlighted output
/// without needing the network.
pub fn sample_response() -> ResponseData {
    let body = r#"{
  "id": 4291,
  "name": "Ada Lovelace",
  "email": "ada@analytical.dev",
  "active": true,
  "roles": ["admin", "engineer"],
  "profile": {
    "title": "Computing Pioneer",
    "joined": "1843-10-01",
    "location": null
  },
  "metrics": {
    "requests": 18204,
    "uptime": 99.98
  }
}"#;
    ResponseData {
        status: 200,
        status_text: "OK".into(),
        duration_ms: 84,
        size_bytes: body.len(),
        size_exact: true,
        shown_bytes: body.len(),
        binary: false,
        headers: vec![
            KeyValue::new("content-type", "application/json; charset=utf-8"),
            KeyValue::new("cache-control", "no-cache, private"),
            KeyValue::new("x-request-id", "b1f4c2a9-77de-4e1a-9f0c-1a2b3c4d5e6f"),
            KeyValue::new("vary", "Accept-Encoding"),
            KeyValue::new("server", "tilo-edge/2.4"),
        ],
        body: body.to_string(),
        content_type: "application/json; charset=utf-8".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{HttpMethod, Request};

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn query_params_precede_fragments_and_reuse_query_separators() {
        for (base, expected) in [
            (
                "/search#section?ignored",
                "/search?q=rust%20lang#section?ignored",
            ),
            ("/search?sort=id#top", "/search?sort=id&q=rust%20lang#top"),
            ("/search?#", "/search?q=rust%20lang#"),
            ("/search?sort=id&", "/search?sort=id&q=rust%20lang"),
        ] {
            let mut req = Request::new("r");
            req.url = "{{base}}".into();
            req.params = vec![KeyValue::new("q", "rust lang")];
            let out = build_outgoing(&req, &env(&[("base", base)]));
            assert_eq!(out.url, expected, "base: {base}");

            req.params.clear();
            assert_eq!(build_outgoing(&req, &env(&[("base", base)])).url, base);
        }
    }

    #[test]
    fn trailing_path_ampersand_does_not_start_a_query() {
        for suffix in ["", "#section?ignored"] {
            let mut req = Request::new("r");
            req.url = format!("https://example.com/items&{suffix}");
            req.params = vec![KeyValue::new("q", "test")];
            assert_eq!(
                build_outgoing(&req, &HashMap::new()).url,
                format!("https://example.com/items&?q=test{suffix}")
            );
        }
    }

    /// `build_outgoing` turns a saved request plus environment into the wire
    /// request: query string, auth header and encoded body.
    #[test]
    fn build_outgoing_encodes_query_auth_and_body() {
        // Case "query string": only enabled params, percent-encoded, after
        // environment substitution.
        {
            let mut req = Request::new("r");
            req.url = "{{base}}/search".into();
            req.params = vec![KeyValue::new("q", "rust lang"), {
                let mut d = KeyValue::new("debug", "1");
                d.enabled = false;
                d
            }];
            let out = build_outgoing(&req, &env(&[("base", "https://api.example.com")]));
            assert_eq!(
                out.url, "https://api.example.com/search?q=rust%20lang",
                "case query string"
            );
        }

        // Case "bearer auth": adds a substituted Authorization header.
        {
            let mut req = Request::new("r");
            req.url = "https://x.test".into();
            req.auth = Auth::Bearer {
                token: "{{tok}}".into(),
            };
            let out = build_outgoing(&req, &env(&[("tok", "secret123")]));
            assert!(
                out.headers
                    .iter()
                    .any(|(k, v)| k == "Authorization" && v == "Bearer secret123"),
                "case bearer auth"
            );
        }

        // Case "basic auth": base64-encodes the credentials.
        {
            let mut req = Request::new("r");
            req.url = "https://x.test".into();
            req.auth = Auth::Basic {
                username: "user".into(),
                password: "pass".into(),
            };
            let out = build_outgoing(&req, &HashMap::new());
            // base64("user:pass") == "dXNlcjpwYXNz"
            assert!(
                out.headers
                    .iter()
                    .any(|(k, v)| k == "Authorization" && v == "Basic dXNlcjpwYXNz"),
                "case basic auth"
            );
        }

        // Case "json body": the body passes through and Content-Type defaults.
        {
            let mut req = Request::new("r");
            req.method = HttpMethod::Post;
            req.url = "https://x.test".into();
            req.body_mode = BodyMode::Json;
            req.body_text = "{\"a\":1}".into();
            let out = build_outgoing(&req, &HashMap::new());
            assert_eq!(out.body.as_deref(), Some("{\"a\":1}"), "case json body");
            assert!(
                out.headers
                    .iter()
                    .any(|(k, v)| k == "Content-Type" && v == "application/json"),
                "case json body"
            );
        }

        // Case "form body": pairs are url-encoded.
        {
            let mut req = Request::new("r");
            req.method = HttpMethod::Post;
            req.url = "https://x.test".into();
            req.body_mode = BodyMode::Form;
            req.form = vec![
                KeyValue::new("name", "Jane Doe"),
                KeyValue::new("role", "admin"),
            ];
            let out = build_outgoing(&req, &HashMap::new());
            assert_eq!(
                out.body.as_deref(),
                Some("name=Jane%20Doe&role=admin"),
                "case form body"
            );
        }
    }
}
