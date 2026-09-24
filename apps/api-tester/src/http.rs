//! Request execution.
//!
//! [`build_outgoing`] resolves a [`Request`] against the active
//! environment's variables into a concrete, ready-to-send shape (URL with
//! query string, header list, body bytes). [`send_blocking`] then performs
//! the round-trip with `reqwest` and returns a [`ResponseData`]. The
//! builder is pure and unit-tested; the network call is isolated so the
//! rest of the app never depends on a live connection — the screenshot
//! seed uses [`sample_response`] instead.

use std::collections::HashMap;
use std::time::Instant;

use base64::Engine;

use crate::model::{Auth, BodyMode, KeyValue, Request, ResponseData};
use crate::vars;

/// A fully-resolved request, with all `{{vars}}` expanded and auth folded
/// into the header list. This is the exact thing that hits the wire.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Outgoing {
    pub method: String,
    /// Full URL including the assembled query string.
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
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
        Auth::None => {}
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

/// Perform the request and collect the response. Blocking — the caller
/// runs it on a worker thread so the UI stays responsive.
pub fn send_blocking(out: &Outgoing) -> Result<ResponseData, String> {
    let client = tilo_net::http::blocking_client()
        .user_agent("tilo-api-tester/0.1")
        .build()
        .map_err(|e| e.to_string())?;

    let method = reqwest::Method::from_bytes(out.method.as_bytes())
        .map_err(|e| format!("invalid method: {e}"))?;
    let mut builder = client.request(method, &out.url);
    for (k, v) in &out.headers {
        builder = builder.header(k, v);
    }
    if let Some(body) = &out.body {
        builder = builder.body(body.clone());
    }

    let started = Instant::now();
    let resp = builder.send().map_err(|e| e.to_string())?;
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
    let body = resp.text().map_err(|e| e.to_string())?;
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

    #[test]
    fn url_gets_query_string_from_enabled_params() {
        let mut req = Request::new("r");
        req.url = "{{base}}/search".into();
        req.params = vec![KeyValue::new("q", "rust lang"), {
            let mut d = KeyValue::new("debug", "1");
            d.enabled = false;
            d
        }];
        let out = build_outgoing(&req, &env(&[("base", "https://api.example.com")]));
        assert_eq!(out.url, "https://api.example.com/search?q=rust%20lang");
    }

    #[test]
    fn bearer_auth_adds_authorization_header() {
        let mut req = Request::new("r");
        req.url = "https://x.test".into();
        req.auth = Auth::Bearer {
            token: "{{tok}}".into(),
        };
        let out = build_outgoing(&req, &env(&[("tok", "secret123")]));
        assert!(
            out.headers
                .iter()
                .any(|(k, v)| k == "Authorization" && v == "Bearer secret123")
        );
    }

    #[test]
    fn basic_auth_base64_encodes_credentials() {
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
                .any(|(k, v)| k == "Authorization" && v == "Basic dXNlcjpwYXNz")
        );
    }

    #[test]
    fn json_body_defaults_content_type() {
        let mut req = Request::new("r");
        req.method = HttpMethod::Post;
        req.url = "https://x.test".into();
        req.body_mode = BodyMode::Json;
        req.body_text = "{\"a\":1}".into();
        let out = build_outgoing(&req, &HashMap::new());
        assert_eq!(out.body.as_deref(), Some("{\"a\":1}"));
        assert!(
            out.headers
                .iter()
                .any(|(k, v)| k == "Content-Type" && v == "application/json")
        );
    }

    #[test]
    fn form_body_encodes_pairs() {
        let mut req = Request::new("r");
        req.method = HttpMethod::Post;
        req.url = "https://x.test".into();
        req.body_mode = BodyMode::Form;
        req.form = vec![
            KeyValue::new("name", "Jane Doe"),
            KeyValue::new("role", "admin"),
        ];
        let out = build_outgoing(&req, &HashMap::new());
        assert_eq!(out.body.as_deref(), Some("name=Jane%20Doe&role=admin"));
    }

    #[test]
    fn sample_response_is_valid_json() {
        let r = sample_response();
        assert!(r.is_json());
        assert!(serde_json::from_str::<serde_json::Value>(&r.body).is_ok());
        assert_eq!(r.status, 200);
    }
}
