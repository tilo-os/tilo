//! Discord's REST API: one call, with the bot token or a webhook's token,
//! inside the egress allowlist and the rate limits.

use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use tilo_connector::protocol::FromWorker;
use tilo_net::{Client, Method, NetError, Redirects};
use tokio::sync::mpsc;
use url::Url;

use crate::egress::{Egress, Purpose};
use crate::ratelimit::{RateLimits, route_key};
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
    limits: RateLimits,
    secrets: Arc<Secrets>,
    out: mpsc::UnboundedSender<FromWorker>,
}

impl Rest {
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
            limits: RateLimits::default(),
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
        self.limits.limited_total()
    }

    /// Whether a call to `path` would wait for its rate-limit bucket.
    #[must_use]
    pub fn busy(&self, method: &Method, path: &str) -> bool {
        self.limits.busy(&route_key(method.as_str(), path))
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
        let route = route_key(method.as_str(), path);
        let (content_type, bytes) = encode(&body);
        for _ in 0..MAX_ATTEMPTS {
            let mut held = self.limits.acquire(&route).await;
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
            self.limits.observe(&mut held, &headers);
            let raw = response
                .bytes_limited(MAX_RESPONSE_BYTES)
                .await
                .map_err(|e| unreachable_error(&e))?;
            let value: Value = if raw.is_empty() {
                Value::Null
            } else {
                serde_json::from_slice(&raw).unwrap_or(Value::Null)
            };
            match status {
                200..=299 => return Ok(value),
                429 => {
                    let wait = self.limits.retry_after(&mut held, &headers, &value);
                    if wait > MAX_INLINE_WAIT {
                        return Err(RestError::Unavailable(format!(
                            "Discord is rate limiting this route for {}s",
                            wait.as_secs()
                        )));
                    }
                    drop(held);
                }
                401 if auth == Auth::Bot => {
                    self.secrets.forget(BOT_TOKEN, TOKEN_ITEM);
                    let _ = self.out.send(FromWorker::Status {
                        healthy: false,
                        message: REJECTED_TOKEN.into(),
                    });
                    return Err(RestError::Unauthorized(REJECTED_TOKEN.into()));
                }
                400..=499 => {
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
