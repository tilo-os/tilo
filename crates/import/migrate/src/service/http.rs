//! The bounded HTTP calls the service sources make. Keys go in a header, never a URL, and no
//! refusal carries a response body.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use super::ServiceError;

const TIMEOUT: Duration = Duration::from_secs(60);
const MAX_ANSWER_BYTES: u64 = 64 * 1024 * 1024;
/// The most requests in flight at once: a service's rate limit, not its latency times its
/// items, sets the pace of a fetch.
pub const IN_FLIGHT: usize = 8;
/// How often a request a service asked to wait for is sent again.
const RETRIES: usize = 4;
/// How long a service's request to wait is honoured at most.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(30);
/// The largest picture a page keeps: what a Notes page keeps beside it.
pub const MAX_PICTURE_BYTES: u64 = 1024 * 1024;

/// How a service takes its key.
#[derive(Clone, Copy)]
pub enum Key {
    /// `Authorization: Bearer <key>`: Outline.
    Bearer,
    /// `Authorization: <key>`: Linear's personal API keys.
    Raw,
    /// `Authorization: token <key>`: Forgejo's access tokens.
    Token,
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

/// The one client every request goes through: its pool keeps the connections a source's many
/// requests reuse, instead of a handshake per request.
fn client() -> Result<reqwest::blocking::Client, ServiceError> {
    static CLIENT: std::sync::OnceLock<reqwest::blocking::Client> = std::sync::OnceLock::new();
    if let Some(client) = CLIENT.get() {
        return Ok(client.clone());
    }
    let client = tilo_net::http::blocking_client()
        .timeout(TIMEOUT)
        .build()
        .map_err(|error| ServiceError::Unavailable(error.to_string()))?;
    Ok(CLIENT.get_or_init(|| client).clone())
}

/// `build`ed and authorized, sent; a service asking to wait (`429`, `503` with `Retry-After`)
/// is waited for and asked again, up to [`RETRIES`] times.
fn send(
    build: &dyn Fn() -> Result<reqwest::blocking::RequestBuilder, ServiceError>,
    key: &str,
    how: Key,
) -> Result<reqwest::blocking::Response, ServiceError> {
    for attempt in 0..=RETRIES {
        let response = authorized(build()?, key, how)
            .send()
            .map_err(|error| ServiceError::Unavailable(without_url(&error)))?;
        if attempt == RETRIES {
            return Ok(response);
        }
        match response.status().as_u16() {
            429 | 503 => std::thread::sleep(retry_wait(&response)),
            _ => return Ok(response),
        }
    }
    Err(ServiceError::Unavailable("it kept asking to wait".into()))
}

/// How long a service asked to wait, by its `Retry-After`; a second when it names none.
fn retry_wait(response: &reqwest::blocking::Response) -> Duration {
    response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map_or(Duration::from_secs(1), |seconds| {
            Duration::from_secs(seconds).min(MAX_RETRY_WAIT)
        })
}

fn authorized(
    request: reqwest::blocking::RequestBuilder,
    key: &str,
    how: Key,
) -> reqwest::blocking::RequestBuilder {
    match how {
        Key::Bearer => request.bearer_auth(key),
        Key::Raw => request.header("authorization", key),
        Key::Token => request.header("authorization", format!("token {key}")),
    }
}

/// `GET url`, the answer as JSON; `None` when the service has no such thing (404).
pub fn get_json(url: &str, key: &str, how: Key) -> Result<Option<serde_json::Value>, ServiceError> {
    let response = send(
        &|| Ok(client()?.get(url).header("accept", "application/json")),
        key,
        how,
    )?;
    let status = response.status().as_u16();
    if matches!(status, 401 | 403) {
        return Err(ServiceError::Refused);
    }
    if status == 404 {
        return Ok(None);
    }
    if !(200..300).contains(&status) {
        return Err(ServiceError::Unavailable(format!(
            "{} answered {status}",
            without_query(url)
        )));
    }
    let bytes = bounded(response, MAX_ANSWER_BYTES)?;
    serde_json::from_slice(&bytes).map(Some).map_err(|_| {
        ServiceError::Unavailable(format!("{} answered something else", without_query(url)))
    })
}

fn without_query(url: &str) -> &str {
    url.split_once('?').map_or(url, |(path, _)| path)
}

/// `POST url` with `body` as JSON, the answer as JSON.
pub fn post_json(
    url: &str,
    body: &serde_json::Value,
    key: &str,
    how: Key,
) -> Result<serde_json::Value, ServiceError> {
    let response = send(
        &|| {
            Ok(client()?
                .post(url)
                .header("content-type", "application/json")
                .body(body.to_string()))
        },
        key,
        how,
    )?;
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

/// `work` of every item, at most [`IN_FLIGHT`] at a time, the answers in `items`' order.
pub fn each<T, R>(items: &[T], work: impl Fn(&T) -> R + Sync) -> Vec<R>
where
    T: Sync,
    R: Send,
{
    let mut answers: Vec<Option<R>> = (0..items.len()).map(|_| None).collect();
    let next = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        let workers: Vec<_> = (0..IN_FLIGHT.min(items.len()))
            .map(|_| {
                scope.spawn(|| {
                    let mut mine = Vec::new();
                    loop {
                        let at = next.fetch_add(1, Ordering::Relaxed);
                        let Some(item) = items.get(at) else {
                            break;
                        };
                        mine.push((at, work(item)));
                    }
                    mine
                })
            })
            .collect();
        for worker in workers {
            match worker.join() {
                Ok(mine) => {
                    for (at, answer) in mine {
                        if let Some(answer_at) = answers.get_mut(at) {
                            *answer_at = Some(answer);
                        }
                    }
                }
                Err(panicked) => std::panic::resume_unwind(panicked),
            }
        }
    });
    answers.into_iter().flatten().collect()
}
