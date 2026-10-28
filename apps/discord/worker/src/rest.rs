//! Discord's REST API: one call, with the bot token or a webhook's token,
//! inside the egress allowlist and the rate limits.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tilo_connector::protocol::FromWorker;
use tilo_net::{Client, HeaderMap, Method, NetError, Redirects};
use tokio::sync::mpsc;
use tokio::time::Instant;
use twilight_http_ratelimiting::{Endpoint, RateLimitHeaders, RateLimiter};
use url::Url;

use crate::egress::{Egress, Purpose};
use crate::secrets::{BOT_TOKEN, Secrets, TOKEN_ITEM};

/// Longest wait for a 429 the worker sits out inside one call; a longer
/// one fails the call as unavailable, so the host retries it later instead
/// of a perform outliving its step budget.
const MAX_INLINE_WAIT: Duration = Duration::from_secs(20);
const MAX_ATTEMPTS: u32 = 8;
const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
/// Discord asks bots to identify themselves this way.
const USER_AGENT: &str = "DiscordBot (https://tilo.so, 1)";
pub const REJECTED_TOKEN: &str = "Discord rejected the bot token";

#[derive(Debug)]
pub enum Body {
    Empty,
    Json(Value),
    /// `payload_json` and files, as webhook uploads take them.
    Multipart {
        payload: Value,
        files: Vec<(String, Vec<u8>)>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Auth {
    /// `Authorization: Bot <token>`.
    Bot,
    /// None: the path carries a webhook's or an interaction's token.
    InPath,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RestError {
    /// Discord refused the call (a 4xx other than 429): permanent.
    Refused {
        status: u16,
        code: u64,
        message: String,
    },
    /// The bot token is missing or Discord rejected it.
    Unauthorized(String),
    /// Discord could not be reached, failed, or is rate limiting for long.
    Unavailable(String),
}

impl RestError {
    /// Discord's JSON error code of a refusal.
    #[must_use]
    pub fn code(&self) -> Option<u64> {
        match self {
            Self::Refused { code, .. } => Some(*code),
            _ => None,
        }
    }

    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Refused { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// A 404 for something that is already gone.
    #[must_use]
    pub fn is_unknown(&self) -> bool {
        self.status() == Some(404)
    }
}

impl std::fmt::Display for RestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused {
                status,
                code,
                message,
            } => write!(f, "Discord refused ({status}): {message} ({code})"),
            Self::Unauthorized(message) | Self::Unavailable(message) => f.write_str(message),
        }
    }
}

pub struct Rest {
    http: Client,
    base: Url,
    egress: Egress,
    /// Calls in one bucket go one at a time and in the order they were
    /// made, which keeps calls to one channel in order, and wait for the
    /// refill when the bucket is empty.
    limits: RateLimiter,
    /// Until when a global 429 holds every call.
    global_until: Mutex<Option<Instant>>,
    /// Calls that got a 429 or waited for a bucket or the global limit.
    limited: AtomicU64,
    secrets: Arc<Secrets>,
    out: mpsc::UnboundedSender<FromWorker>,
}

impl Rest {
    /// Must be called inside a Tokio runtime, which runs the rate limiter.
    ///
    /// # Errors
    /// The HTTP client could not be built.
    pub fn new(
        base: Url,
        egress: Egress,
        secrets: Arc<Secrets>,
        out: mpsc::UnboundedSender<FromWorker>,
    ) -> Result<Self, String> {
        let http = Client::builder()
            .timeout(Duration::from_secs(30))
            .connect_timeout(Duration::from_secs(10))
            .user_agent(USER_AGENT)
            // A redirect could lead outside the allowlist.
            .redirects(Redirects::None)
            .build()
            .map_err(|error| error.to_string())?;
        Ok(Self {
            http,
            base,
            egress,
            limits: RateLimiter::default(),
            global_until: Mutex::new(None),
            limited: AtomicU64::new(0),
            secrets,
            out,
        })
    }

    #[must_use]
    pub fn http(&self) -> &Client {
        &self.http
    }

    /// How many calls a 429 or a rate-limit wait has held up so far.
    #[must_use]
    pub fn rate_limited_total(&self) -> u64 {
        self.limited.load(Ordering::Relaxed)
    }

    /// Whether a call to `path` would wait for its rate-limit bucket to
    /// refill.
    pub async fn busy(&self, method: &Method, path: &str) -> bool {
        let Some(endpoint) = endpoint(method, path) else {
            return false;
        };
        self.limits.bucket(endpoint).await.is_some_and(|bucket| {
            bucket.remaining == 0 && bucket.reset_at > std::time::Instant::now()
        })
    }

    /// Wait for the global limit and `endpoint`'s bucket to allow a call,
    /// counting the wait.
    async fn permit(
        &self,
        method: &Method,
        path: &str,
        endpoint: &Endpoint,
    ) -> twilight_http_ratelimiting::Permit {
        if self.wait_global().await | self.busy(method, path).await {
            self.limited.fetch_add(1, Ordering::Relaxed);
        }
        self.limits.acquire(endpoint.clone()).await
    }

    /// Wait out a global limit; whether there was one.
    async fn wait_global(&self) -> bool {
        let until = self.global_until.lock().ok().and_then(|until| *until);
        match until {
            Some(until) if until > Instant::now() => {
                tokio::time::sleep_until(until).await;
                true
            }
            _ => false,
        }
    }

    /// Call `method path` with `query` and `body`. `path` is below the API
    /// base, starts with `/` and has its segments percent-encoded.
    ///
    /// # Errors
    /// See [`RestError`].
    pub async fn call(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, String)],
        body: Body,
        auth: Auth,
        reason: Option<&str>,
    ) -> Result<Value, RestError> {
        let mut url = Url::parse(&format!(
            "{}{path}",
            self.base.as_str().trim_end_matches('/')
        ))
        .map_err(|error| RestError::Unavailable(format!("bad request URL: {error}")))?;
        if !query.is_empty() {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in query {
                pairs.append_pair(key, value);
            }
        }
        self.egress
            .check(&url, Purpose::Api)
            .map_err(RestError::Unavailable)?;
        let endpoint = endpoint(&method, path)
            .ok_or_else(|| RestError::Unavailable(format!("no rate limits for {method}")))?;
        let (content_type, bytes) = encode(&body);
        for _ in 0..MAX_ATTEMPTS {
            let permit = self.permit(&method, path, &endpoint).await;
            let mut request = self.http.request(method.clone(), url.as_str());
            if auth == Auth::Bot {
                let token = self
                    .secrets
                    .get(BOT_TOKEN, TOKEN_ITEM)
                    .await
                    .ok_or_else(|| {
                        RestError::Unauthorized("no bot token is set for the connector".into())
                    })?;
                request = request.header("authorization", format!("Bot {}", token.as_str()));
            }
            if let Some(reason) = reason {
                request = request.header(
                    "x-audit-log-reason",
                    percent_encoding::utf8_percent_encode(
                        reason,
                        percent_encoding::NON_ALPHANUMERIC,
                    )
                    .to_string(),
                );
            }
            if let Some(content_type) = &content_type {
                request = request
                    .header("content-type", content_type.as_str())
                    .body(bytes.clone());
            }
            let response = request.send().await.map_err(|e| unreachable_error(&e))?;
            let status = response.status().as_u16();
            let headers = response.headers().clone();
            let raw = response
                .bytes_limited(MAX_RESPONSE_BYTES)
                .await
                .map_err(|e| unreachable_error(&e))?;
            let value: Value = if raw.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&raw).unwrap_or(Value::Null)
            };
            match (status, self.settle(permit, status, &headers, &value)) {
                (200..=299, _) => return Ok(value),
                (_, Some(wait)) => {
                    if wait > MAX_INLINE_WAIT {
                        return Err(RestError::Unavailable(format!(
                            "Discord is rate limiting this route for {}s",
                            wait.as_secs()
                        )));
                    }
                }
                (401, _) if auth == Auth::Bot => {
                    self.secrets.forget(BOT_TOKEN, TOKEN_ITEM);
                    let _ = self.out.send(FromWorker::Status {
                        healthy: false,
                        message: REJECTED_TOKEN.into(),
                    });
                    return Err(RestError::Unauthorized(REJECTED_TOKEN.into()));
                }
                (400..=499, _) => {
                    return Err(RestError::Refused {
                        status,
                        code: value.get("code").and_then(Value::as_u64).unwrap_or(0),
                        message: value
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("refused")
                            .to_owned(),
                    });
                }
                _ => {
                    return Err(RestError::Unavailable(format!("Discord answered {status}")));
                }
            }
        }
        Err(RestError::Unavailable(
            "Discord kept rate limiting this route".into(),
        ))
    }
}

impl Rest {
    /// Tell the rate limiter what a response said about its bucket. For a
    /// 429, how long it says to wait, from its body's `retry_after` or the
    /// `Retry-After` header: the route's bucket stays empty that long, and a
    /// global 429 holds every other call too.
    fn settle(
        &self,
        permit: twilight_http_ratelimiting::Permit,
        status: u16,
        headers: &HeaderMap,
        body: &Value,
    ) -> Option<Duration> {
        if status != 429 {
            permit.complete(bucket_headers(headers));
            return None;
        }
        self.limited.fetch_add(1, Ordering::Relaxed);
        let wait = body
            .get("retry_after")
            .and_then(Value::as_f64)
            .and_then(|s| Duration::try_from_secs_f64(s).ok())
            .or_else(|| header(headers, "retry-after").and_then(seconds))
            .unwrap_or(Duration::from_secs(1));
        let global = body.get("global").and_then(Value::as_bool) == Some(true)
            || headers.contains_key("x-ratelimit-global");
        if global {
            if let Ok(mut until) = self.global_until.lock() {
                *until = Instant::now().checked_add(wait);
            }
            permit.complete(None);
        } else {
            let bucket = header(headers, RateLimitHeaders::BUCKET).unwrap_or_default();
            permit.complete(Some(RateLimitHeaders {
                bucket: bucket.as_bytes().to_vec(),
                limit: 0,
                remaining: 0,
                reset_at: std::time::Instant::now()
                    .checked_add(wait)
                    .unwrap_or_else(std::time::Instant::now),
            }));
        }
        Some(wait)
    }
}

/// The rate-limited endpoint of `method path`: the path with every id that
/// is not a major parameter (message ids, emoji, user ids) replaced, so the
/// limiter learns one bucket for all the calls Discord counts in one.
fn endpoint(method: &Method, path: &str) -> Option<Endpoint> {
    use twilight_http_ratelimiting::Method as Limited;
    let method = match *method {
        Method::GET => Limited::Get,
        Method::POST => Limited::Post,
        Method::PATCH => Limited::Patch,
        Method::PUT => Limited::Put,
        Method::DELETE => Limited::Delete,
        _ => return None,
    };
    let mut key = Vec::new();
    let mut previous = "";
    let mut before_previous = "";
    for segment in path.trim_start_matches('/').split('/') {
        let major = matches!(previous, "channels" | "guilds" | "webhooks")
            || (before_previous == "webhooks" && previous != "messages");
        let variable = segment.bytes().all(|b| b.is_ascii_digit()) || previous == "reactions";
        key.push(if major || !variable { segment } else { ":id" });
        before_previous = previous;
        previous = segment;
    }
    Some(Endpoint {
        method,
        path: key.join("/"),
    })
}

/// What a response's headers say about its bucket, when they say it all.
fn bucket_headers(headers: &HeaderMap) -> Option<RateLimitHeaders> {
    Some(RateLimitHeaders {
        bucket: header(headers, RateLimitHeaders::BUCKET)?
            .as_bytes()
            .to_vec(),
        limit: header(headers, RateLimitHeaders::LIMIT)?.parse().ok()?,
        remaining: header(headers, RateLimitHeaders::REMAINING)?.parse().ok()?,
        reset_at: std::time::Instant::now()
            .checked_add(header(headers, RateLimitHeaders::RESET_AFTER).and_then(seconds)?)?,
    })
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

fn seconds(text: &str) -> Option<Duration> {
    text.trim()
        .parse::<f64>()
        .ok()
        .and_then(|s| Duration::try_from_secs_f64(s).ok())
}

/// A network failure without the error's own text, which can name the URL
/// and with it a webhook's token.
fn unreachable_error(error: &NetError) -> RestError {
    RestError::Unavailable(match error {
        NetError::Timeout => "Discord did not answer in time".into(),
        NetError::Connect { .. } => "could not connect to Discord".into(),
        NetError::TooLarge { .. } => "Discord's answer was too large".into(),
        _ => "the connection to Discord failed".into(),
    })
}

/// Percent-encode one path segment (an emoji, a token).
#[must_use]
pub fn segment(text: &str) -> String {
    percent_encoding::utf8_percent_encode(text, percent_encoding::NON_ALPHANUMERIC).to_string()
}

fn encode(body: &Body) -> (Option<String>, Vec<u8>) {
    match body {
        Body::Empty => (None, Vec::new()),
        Body::Json(value) => (
            Some("application/json".into()),
            value.to_string().into_bytes(),
        ),
        Body::Multipart { payload, files } => {
            let mut boundary = String::from("tilo-discord-boundary-7f3c9a");
            while files.iter().any(|(_, bytes)| {
                bytes
                    .windows(boundary.len())
                    .any(|w| w == boundary.as_bytes())
            }) {
                boundary.push('x');
            }
            let mut out = Vec::new();
            out.extend_from_slice(
                format!(
                    "--{boundary}\r\nContent-Disposition: form-data; name=\"payload_json\"\r\nContent-Type: application/json\r\n\r\n{payload}\r\n"
                )
                .as_bytes(),
            );
            for (index, (name, bytes)) in files.iter().enumerate() {
                let name = name.replace(['"', '\r', '\n'], "_");
                out.extend_from_slice(
                    format!(
                        "--{boundary}\r\nContent-Disposition: form-data; name=\"files[{index}]\"; filename=\"{name}\"\r\nContent-Type: application/octet-stream\r\n\r\n"
                    )
                    .as_bytes(),
                );
                out.extend_from_slice(bytes);
                out.extend_from_slice(b"\r\n");
            }
            out.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
            (
                Some(format!("multipart/form-data; boundary={boundary}")),
                out,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints_keep_major_parameters_only() {
        let path = |method, path| endpoint(&method, path).map(|endpoint| endpoint.path);
        assert_eq!(
            path(Method::GET, "/channels/11/messages/22").as_deref(),
            Some("channels/11/messages/:id")
        );
        assert_eq!(
            path(
                Method::PUT,
                "/channels/11/messages/22/reactions/%F0%9F%91%8D/@me"
            )
            .as_deref(),
            Some("channels/11/messages/:id/reactions/:id/@me")
        );
        assert_eq!(
            path(Method::POST, "/webhooks/33/tok").as_deref(),
            Some("webhooks/33/tok")
        );
        assert_eq!(
            path(Method::PATCH, "/webhooks/33/tok/messages/44").as_deref(),
            Some("webhooks/33/tok/messages/:id")
        );
        assert_eq!(
            path(Method::PUT, "/guilds/1/members/2/roles/3").as_deref(),
            Some("guilds/1/members/:id/roles/:id")
        );
    }
}
