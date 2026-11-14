//! The bounded HTTP calls the service sources make. Keys go in a header, never a URL, and no
//! refusal carries a response body.

use std::time::Duration;

use super::ServiceError;

const TIMEOUT: Duration = Duration::from_secs(60);
const MAX_ANSWER_BYTES: u64 = 64 * 1024 * 1024;
/// The largest picture a page keeps: what a Notes page keeps beside it.
pub const MAX_PICTURE_BYTES: u64 = 1024 * 1024;

/// How a service takes its key.
#[derive(Clone, Copy)]
pub enum Key {
    /// `Authorization: Bearer <key>`: Outline.
    Bearer,
    /// `Authorization: <key>`: Linear's personal API keys.
    Raw,
}

/// `server` as a base address: http(s), no credentials, query or fragment, no trailing slash.
pub fn base(server: &str) -> Result<String, ServiceError> {
    let trimmed = server.trim();
    let parsed = url::Url::parse(trimmed).map_err(|_| ServiceError::InvalidAddress)?;
    if !matches!(parsed.scheme(), "https" | "http")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(ServiceError::InvalidAddress);
    }
    Ok(trimmed.trim_end_matches('/').to_owned())
}

fn client() -> Result<reqwest::blocking::Client, ServiceError> {
    tilo_net::http::blocking_client()
        .timeout(TIMEOUT)
        .build()
        .map_err(|error| ServiceError::Unavailable(error.to_string()))
}

fn authorized(
    request: reqwest::blocking::RequestBuilder,
    key: &str,
    how: Key,
) -> reqwest::blocking::RequestBuilder {
    match how {
        Key::Bearer => request.bearer_auth(key),
        Key::Raw => request.header("authorization", key),
    }
}

/// `POST url` with `body` as JSON, the answer as JSON.
pub fn post_json(
    url: &str,
    body: &serde_json::Value,
    key: &str,
    how: Key,
) -> Result<serde_json::Value, ServiceError> {
    let request = client()?
        .post(url)
        .header("content-type", "application/json")
        .body(body.to_string());
    let response = authorized(request, key, how)
        .send()
        .map_err(|error| ServiceError::Unavailable(without_url(&error)))?;
    let status = response.status().as_u16();
    if matches!(status, 401 | 403) {
        return Err(ServiceError::Refused);
    }
    if !(200..300).contains(&status) {
        return Err(ServiceError::Unavailable(format!(
            "{url} answered {status}"
        )));
    }
    let bytes = bounded(response, MAX_ANSWER_BYTES)?;
    serde_json::from_slice(&bytes)
        .map_err(|_| ServiceError::Unavailable(format!("{url} answered something else")))
}

/// The picture at `url`, when the service answers one no larger than a page keeps. A picture
/// on the service's own host is fetched with the key; one elsewhere without it.
pub fn picture(url: &str, server: &str, key: &str, how: Key) -> Option<Vec<u8>> {
    let absolute = if url.starts_with("http://") || url.starts_with("https://") {
        url.to_owned()
    } else if url.starts_with('/') {
        format!("{server}{url}")
    } else {
        return None;
    };
    let same_host =
        url::Url::parse(&absolute).ok()?.origin() == url::Url::parse(server).ok()?.origin();
    let mut request = client().ok()?.get(&absolute);
    if same_host {
        request = authorized(request, key, how);
    }
    let response = request.send().ok()?;
    if !response.status().is_success() {
        return None;
    }
    let bytes = bounded(response, MAX_PICTURE_BYTES).ok()?;
    is_picture(&bytes).then_some(bytes)
}

/// PNG, JPEG or WebP: what a Notes page shows.
pub fn is_picture(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(&[0xff, 0xd8, 0xff])
        || (bytes.len() > 12 && bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP"))
}

fn bounded(response: reqwest::blocking::Response, limit: u64) -> Result<Vec<u8>, ServiceError> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    response
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| ServiceError::Unavailable(error.to_string()))?;
    if bytes.len() as u64 > limit {
        return Err(ServiceError::TooLarge);
    }
    Ok(bytes)
}

/// Why a request failed, without its address or anything the service said.
fn without_url(error: &reqwest::Error) -> String {
    if error.is_timeout() {
        "it did not answer in time".to_owned()
    } else if error.is_connect() {
        "it could not be reached".to_owned()
    } else {
        "the connection failed".to_owned()
    }
}
