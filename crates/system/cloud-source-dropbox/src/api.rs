//! The Dropbox API v2 transport: RPC, content-upload, content-download and
//! notification endpoints, credentials, the path-root header and the
//! mapping of HTTP statuses to provider-neutral errors.
//!
//! Route errors (HTTP 409) are handed back with their `error_summary` so each
//! operation maps them itself: `path/conflict/file` is "already exists" for
//! a create and "changed remotely" for a conditional replace.

use std::fmt::Write as _;
use std::io::Read;
use std::ops::Range;
use std::sync::Arc;
use std::time::Duration;

use cloud_source::{Credential, CredentialSource, Error, Result};
use reqwest::blocking::{Client, RequestBuilder, Response};
use serde_json::Value;

/// Largest JSON response read from an RPC endpoint.
const MAX_JSON_BYTES: u64 = 16 * 1024 * 1024;

/// Where the three Dropbox API hosts are. Tests point all three at fakes.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Endpoints {
    /// RPC endpoints (`https://api.dropboxapi.com`).
    pub api: String,
    /// Content upload and download (`https://content.dropboxapi.com`).
    pub content: String,
    /// Long-poll notifications (`https://notify.dropboxapi.com`).
    pub notify: String,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            api: "https://api.dropboxapi.com".into(),
            content: "https://content.dropboxapi.com".into(),
            notify: "https://notify.dropboxapi.com".into(),
        }
    }
}

/// A failed call: a route error the caller interprets, or a mapped error.
#[derive(Debug)]
pub enum Failure {
    /// HTTP 409 with a structured error.
    Route {
        summary: String,
        error: Value,
    },
    Error(Error),
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Self::Error(error)
    }
}

impl Failure {
    /// Whether `tag` is one of the route error's `error_summary` tags.
    pub fn has(&self, tag: &str) -> bool {
        match self {
            Self::Route { summary, .. } => summary.split('/').any(|t| t.trim() == tag),
            Self::Error(_) => false,
        }
    }

    /// The outermost tag of a route error (`path` of `path/not_found/..`).
    pub fn first_tag(&self) -> Option<&str> {
        match self {
            Self::Route { summary, .. } => summary.split('/').next().map(str::trim),
            Self::Error(_) => None,
        }
    }

    /// The provider-neutral error for route errors no operation-specific
    /// mapping claimed.
    pub fn into_error(self) -> Error {
        match self {
            Self::Error(error) => error,
            route @ Self::Route { .. } => {
                if route.has("not_found") || route.has("not_folder") {
                    Error::NotFound
                } else if route.has("reset") {
                    Error::CursorExpired
                } else if route.has("insufficient_space") || route.has("insufficient_quota") {
                    Error::InsufficientStorage
                } else if route.has("no_write_permission")
                    || route.has("no_permission")
                    || route.has("email_not_verified")
                    || route.has("access_denied")
                {
                    Error::Forbidden
                } else if route.has("too_many_write_operations") || route.has("too_many_files") {
                    Error::RateLimited { retry_after: None }
                } else if route.has("conflict") {
                    Error::AlreadyExists
                } else if route.has("content_hash_mismatch") {
                    Error::Io(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "uploaded bytes do not match the content hash",
                    ))
                } else {
                    Error::Status(409)
                }
            }
        }
    }
}

/// A successful download: the streaming response and its metadata.
pub struct Downloaded {
    pub response: Response,
    /// The `Dropbox-API-Result` header.
    pub result: Value,
    pub partial: bool,
    pub content_range: Option<(Range<u64>, Option<u64>)>,
    pub content_length: Option<u64>,
}

/// One account's transport. Cloning shares the connection pool.
#[derive(Clone)]
pub struct Api {
    http: Client,
    endpoints: Endpoints,
    credentials: Arc<dyn CredentialSource>,
    path_root: Option<String>,
}

/// `serde_json` with every non-ASCII character escaped, as HTTP headers
/// (`Dropbox-API-Arg`) require.
pub fn ascii_json(value: &Value) -> String {
    let text = value.to_string();
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if c.is_ascii() && c != '\u{7f}' {
            out.push(c);
        } else {
            let mut units = [0_u16; 2];
            for unit in c.encode_utf16(&mut units) {
                let _ = write!(out, "\\u{unit:04x}");
            }
        }
    }
    out
}

fn transport(_: reqwest::Error) -> Error {
    Error::Offline
}

fn header<'a>(response: &'a Response, name: &str) -> Option<&'a str> {
    response.headers().get(name).and_then(|v| v.to_str().ok())
}

fn retry_after(response: &Response) -> Option<Duration> {
    header(response, "retry-after")
        .and_then(|v| v.trim().parse().ok())
        .map(Duration::from_secs)
}

fn read_json(response: Response) -> Result<Value> {
    let mut body = Vec::new();
    response
        .take(MAX_JSON_BYTES.saturating_add(1))
        .read_to_end(&mut body)
        .map_err(|_| Error::Offline)?;
    if body.len() as u64 > MAX_JSON_BYTES {
        return Err(Error::TooLarge);
    }
    if body.is_empty() {
        return Ok(Value::Null);
    }
    serde_json::from_slice(&body).map_err(|_| Error::InvalidResponse("json"))
}

/// `bytes a-b/total` of a `Content-Range` header, as a half-open range.
fn content_range(value: &str) -> Option<(Range<u64>, Option<u64>)> {
    let rest = value.trim().strip_prefix("bytes ")?;
    let (span, total) = rest.split_once('/')?;
    let (start, end) = span.split_once('-')?;
    let start: u64 = start.trim().parse().ok()?;
    let end: u64 = end.trim().parse().ok()?;
    let total = total.trim().parse().ok();
    Some((start..end.checked_add(1)?, total))
}

impl Api {
    pub fn new(
        endpoints: Endpoints,
        credentials: Arc<dyn CredentialSource>,
        path_root: Option<String>,
    ) -> Result<Self> {
        let http = tilo_net::http::blocking_client()
            .user_agent("Tilo-Storage/0.1")
            // Whole-request deadline: chunk uploads, large ranged reads and
            // long polls (up to 480 s plus Dropbox's jitter) take a while;
            // the connect timeout catches dead servers.
            .timeout(Duration::from_secs(30 * 60))
            .connect_timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| Error::Offline)?;
        Ok(Self {
            http,
            endpoints,
            credentials,
            path_root,
        })
    }

    fn authorized(&self, request: RequestBuilder) -> Result<RequestBuilder> {
        let token = match self.credentials.credential()? {
            Credential::Bearer { token, .. } => token,
            Credential::Basic { .. } => return Err(Error::Unsupported("basic credentials")),
            Credential::Session { .. } => return Err(Error::Unsupported("session credentials")),
        };
        let mut request = request.bearer_auth(token.as_str());
        if let Some(namespace) = &self.path_root {
            let root = serde_json::json!({".tag": "root", "root": namespace});
            request = request.header("Dropbox-API-Path-Root", ascii_json(&root));
        }
        Ok(request)
    }

    /// Map a response status; successes pass through.
    fn check(
        &self,
        sent: reqwest::Result<Response>,
        authenticated: bool,
    ) -> Result<Response, Failure> {
        let response = sent.map_err(transport)?;
        let status = response.status().as_u16();
        if response.status().is_success() {
            return Ok(response);
        }
        Err(match status {
            401 if authenticated => {
                self.credentials.rejected();
                Error::Unauthorized.into()
            }
            403 => Error::Forbidden.into(),
            409 => {
                let body = read_json(response)?;
                let summary = body
                    .get("error_summary")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let error = body.get("error").cloned().unwrap_or(Value::Null);
                Failure::Route { summary, error }
            }
            429 => Error::RateLimited {
                retry_after: retry_after(&response),
            }
            .into(),
            500.. if retry_after(&response).is_some() => Error::RateLimited {
                retry_after: retry_after(&response),
            }
            .into(),
            status => Error::Status(status).into(),
        })
    }

    /// Call an RPC endpoint with a JSON argument (`Value::Null` for none).
    pub fn rpc(&self, route: &str, arg: &Value) -> Result<Value, Failure> {
        let url = format!("{}/2/{route}", self.endpoints.api);
        let request = self
            .authorized(self.http.post(url))?
            .header("Content-Type", "application/json")
            .body(arg.to_string());
        let response = self.check(request.send(), true)?;
        Ok(read_json(response)?)
    }

    /// Call a content-upload endpoint with `body`.
    pub fn upload(&self, route: &str, arg: &Value, body: Vec<u8>) -> Result<Value, Failure> {
        let url = format!("{}/2/{route}", self.endpoints.content);
        let request = self
            .authorized(self.http.post(url))?
            .header("Dropbox-API-Arg", ascii_json(arg))
            .header("Content-Type", "application/octet-stream")
            .body(body);
        let response = self.check(request.send(), true)?;
        Ok(read_json(response)?)
    }

    /// `files/download`, optionally for a byte range.
    pub fn download(&self, arg: &Value, range: Option<&Range<u64>>) -> Result<Downloaded, Failure> {
        let url = format!("{}/2/files/download", self.endpoints.content);
        let mut request = self
            .authorized(self.http.post(url))?
            .header("Dropbox-API-Arg", ascii_json(arg));
        if let Some(range) = range {
            let last = range.end.saturating_sub(1).max(range.start);
            request = request.header("Range", format!("bytes={}-{last}", range.start));
        }
        let response = self.check(request.send(), true)?;
        let result = header(&response, "dropbox-api-result")
            .and_then(|v| serde_json::from_str(v).ok())
            .ok_or(Error::InvalidResponse("download result header"))?;
        let partial = response.status().as_u16() == 206;
        let content_range = header(&response, "content-range").and_then(content_range);
        let content_length = response.content_length();
        Ok(Downloaded {
            response,
            result,
            partial,
            content_range,
            content_length,
        })
    }

    /// `files/list_folder/longpoll`. Unauthenticated by design: the cursor
    /// is the capability, and Dropbox refuses an `Authorization` header here.
    pub fn longpoll(&self, cursor: &str, timeout_secs: u64) -> Result<Value, Failure> {
        let url = format!("{}/2/files/list_folder/longpoll", self.endpoints.notify);
        let body = serde_json::json!({"cursor": cursor, "timeout": timeout_secs});
        let request = self
            .http
            .post(url)
            .header("Content-Type", "application/json")
            // Dropbox adds up to 90 s of jitter to the requested timeout.
            .timeout(Duration::from_secs(timeout_secs.saturating_add(120)))
            .body(body.to_string());
        let response = self.check(request.send(), false)?;
        Ok(read_json(response)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_arguments_escape_non_ascii() {
        let arg = serde_json::json!({"path": "/Café/😀"});
        assert_eq!(
            ascii_json(&arg),
            "{\"path\":\"/Caf\\u00e9/\\ud83d\\ude00\"}"
        );
        let back: Value = serde_json::from_str(&ascii_json(&arg)).unwrap();
        assert_eq!(back, arg);
    }

    #[test]
    fn content_ranges_parse_half_open() {
        assert_eq!(content_range("bytes 10-19/100"), Some((10..20, Some(100))));
        assert_eq!(content_range("bytes 0-0/*"), Some((0..1, None)));
        assert_eq!(content_range("items 0-1/2"), None);
    }

    #[test]
    fn route_errors_match_whole_tags() {
        let failure = Failure::Route {
            summary: "path/conflict/file/...".into(),
            error: Value::Null,
        };
        assert!(failure.has("conflict"));
        assert!(!failure.has("conf"));
        assert!(matches!(failure.into_error(), Error::AlreadyExists));
        let reset = Failure::Route {
            summary: "reset/..".into(),
            error: Value::Null,
        };
        assert!(matches!(reset.into_error(), Error::CursorExpired));
    }
}
