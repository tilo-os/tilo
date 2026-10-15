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
    let url = if query.is_empty() {
        base
    } else if base.contains('?') {
        format!("{base}&{}", query.join("&"))
    } else {
        format!("{base}?{}", query.join("&"))
    };

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
fn client() -> Result<&'static tilo_net::Client, String> {
    static CLIENT: OnceLock<Result<tilo_net::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            tilo_net::Client::builder()
                .user_agent("tilo-api-tester/0.1")
                .fetch_proxy(tilo_net::proxy::ProxyUse::CrossOrigin)
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Perform the request and collect the response. The caller runs it on the
/// app runtime so the UI stays responsive.
pub async fn send(out: &Outgoing) -> Result<ResponseData, String> {
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
    let resp = builder.send().await.map_err(|e| describe(&e))?;
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
    let body = resp.text().await.map_err(|e| describe(&e))?;
    let duration_ms = started.elapsed().as_millis() as u64;

    Ok(ResponseData {
        status: status.as_u16(),
        status_text,
        duration_ms,
        size_bytes: body.len(),
        headers,
        body,
        content_type,
    })
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
