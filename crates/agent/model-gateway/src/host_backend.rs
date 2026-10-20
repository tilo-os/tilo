//! Explicit, trusted-host compatibility entry point for relay operators.
//!
//! This is not an RPC and must never be wired to sandbox-controlled endpoints,
//! file paths or keys. Sandboxed apps use `Complete` over their launcher-issued
//! capability. Host CLI credentials are adopted into the shared, origin/purpose
//! bound credential store, then resolved only here; rotating files are adopted
//! again before each attempt. Provider encoding/decoding is the gateway's Rig
//! adapter, not a second HTTP implementation.
//!
//! This module is a child of `service` so both entry points share its private
//! transport policy. Only this deliberately host-only facade is re-exported.
use super::*;
use crate::openai::{self, Target};
pub use agent_provider_config::protocol;
use protocol as neutral;

/// Compatibility source accepted only from a trusted host operator.
#[derive(Clone, PartialEq, Eq)]
pub enum ApiKey {
    None,
    Value(String),
    File(PathBuf),
}

impl std::fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::None => "None",
            Self::Value(_) => "Value([redacted])",
            Self::File(_) => "File([host credential source])",
        })
    }
}

impl ApiKey {
    /// Prefer a nonempty value, then a nonempty rotating-file path.
    #[must_use]
    pub fn from_env(value: Option<String>, file: Option<String>) -> Self {
        match (
            value.filter(|value| !value.is_empty()),
            file.filter(|file| !file.is_empty()),
        ) {
            (Some(value), _) => Self::Value(value),
            (None, Some(file)) => Self::File(file.into()),
            (None, None) => Self::None,
        }
    }

    fn resolve(
        &self,
        endpoint: &str,
        credentials: &ServiceCredentials,
    ) -> Result<Option<Zeroizing<String>>, String> {
        let value = match self {
            Self::None => return Ok(None),
            Self::Value(value) => Zeroizing::new(value.clone()),
            Self::File(path) => {
                use std::io::Read as _;
                let file = fs::File::open(path)
                    .map_err(|_| "the model key file is unreadable".to_string())?;
                let mut text = Zeroizing::new(String::new());
                file.take(16 * 1024 + 1)
                    .read_to_string(&mut text)
                    .map_err(|_| "the model key file is unreadable".to_string())?;
                if text.len() > 16 * 1024 || text.trim().is_empty() {
                    return Err("the model key file is empty or too large".into());
                }
                Zeroizing::new(text.trim().to_owned())
            }
        };
        // Bind paths as well as origin: two metered machines on one host must
        // never share a credential reference. No endpoint or source crosses RPC.
        let purpose = format!(
            "host-model-{}",
            openai::idempotency_key(&[endpoint.as_bytes()])
        );
        let authorization = ServiceAuthorization::new(SERVICE_NAME, &purpose, endpoint)
            .map_err(|error| credential_error(&error))?;
        let reference = authorization
            .credential_ref("operator")
            .map_err(|error| credential_error(&error))?;
        let _serial = VAULT_WRITES
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        credentials
            .store(&reference, &authorization, &value)
            .map_err(|error| credential_error(&error))?;
        credentials
            .resolve(&reference, &authorization)
            .map_err(|error| credential_error(&error))
    }
}

/// Reusable trusted-host transport for neutral completion turns.
///
/// Keeps its runtime and ordinary HTTP connection pool alive between turns.
/// Metered turns retain a dedicated client so their idempotency header cannot
/// leak into another logical request.
pub struct CompletionClient {
    runtime: Option<tokio::runtime::Runtime>,
    http: reqwest::Client,
    request_hints: serde_json::Value,
}

impl CompletionClient {
    /// Construct a transport with redirects and automatic HTTP retries disabled.
    ///
    /// # Errors
    /// Returns a bounded diagnostic if the runtime or HTTP client cannot start.
    pub fn new() -> Result<Self, neutral::ModelError> {
        let http = host_http(reqwest::header::HeaderMap::new())?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| neutral::ModelError::new(neutral::ErrorKind::Unavailable))?;
        Ok(Self {
            runtime: Some(runtime),
            http,
            request_hints: serde_json::json!({}),
        })
    }

    /// Set trusted-host provider preferences. Only service tier and reasoning
    /// effort are accepted; these never alter the neutral protocol or authority.
    #[must_use]
    pub fn with_request_hints(mut self, hints: &serde_json::Value) -> Self {
        let Some(request_hints) = self.request_hints.as_object_mut() else {
            return self;
        };
        for (name, allowed) in [
            ("service_tier", &["auto", "default", "flex", "priority"][..]),
            (
                "reasoning_effort",
                &["none", "minimal", "low", "medium", "high", "xhigh"][..],
            ),
        ] {
            if let Some(value) = hints.get(name).and_then(serde_json::Value::as_str)
                && allowed.contains(&value)
            {
                request_hints.insert(name.to_owned(), value.into());
            }
        }
        self
    }

    /// Complete one neutral turn with the same custody and bounded retry policy
    /// as the gateway. Rotating credentials are resolved before every attempt.
    ///
    /// # Errors
    /// Returns bounded credential, provider, deadline or runtime diagnostics.
    pub fn complete(
        &mut self,
        endpoint: &str,
        source: &ApiKey,
        target: Target,
        model: &str,
        request: &neutral::ModelRequest,
        key: &str,
        deadline: Instant,
    ) -> Result<neutral::ModelResponse, neutral::ModelError> {
        let runtime = self
            .runtime
            .as_ref()
            .ok_or_else(|| neutral::ModelError::new(neutral::ErrorKind::Unavailable))?;
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    runtime.block_on(complete_with_http(
                        endpoint,
                        source,
                        target,
                        (model, request, &self.request_hints),
                        key,
                        deadline,
                        &ServiceCredentials::open(),
                        &self.http,
                    ))
                })
                .join()
                .unwrap_or_else(|_| Err(neutral::ModelError::new(neutral::ErrorKind::Provider)))
        })
    }
}

impl Drop for CompletionClient {
    fn drop(&mut self) {
        // Safe even when a synchronous host facade is dropped inside Tokio.
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_background();
        }
    }
}

fn host_http(headers: reqwest::header::HeaderMap) -> Result<reqwest::Client, neutral::ModelError> {
    tilo_net::http::client()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(CONNECT_TIMEOUT)
        .default_headers(headers)
        .build()
        .map_err(|_| neutral::ModelError::new(neutral::ErrorKind::Unavailable))
}

#[cfg(test)]
async fn complete_async(
    endpoint: &str,
    source: &ApiKey,
    target: Target,
    turn: (&str, &neutral::ModelRequest),
    key: &str,
    deadline: Instant,
    credentials: &ServiceCredentials,
) -> Result<neutral::ModelResponse, neutral::ModelError> {
    let http = host_http(reqwest::header::HeaderMap::new())?;
    complete_with_http(
        endpoint,
        source,
        target,
        (turn.0, turn.1, &serde_json::json!({})),
        key,
        deadline,
        credentials,
        &http,
    )
    .await
}

async fn complete_with_http(
    endpoint: &str,
    source: &ApiKey,
    target: Target,
    turn: (&str, &neutral::ModelRequest, &serde_json::Value),
    key: &str,
    deadline: Instant,
    credentials: &ServiceCredentials,
    pooled: &reqwest::Client,
) -> Result<neutral::ModelResponse, neutral::ModelError> {
    let mut request = rig_adapter::neutral_request(turn.1.clone())?;
    request.additional_params = Some(turn.2.clone());
    let mut headers = reqwest::header::HeaderMap::new();
    if target.needs_idempotency_key() {
        if key.is_empty() {
            return Err(neutral::ModelError::new(neutral::ErrorKind::InvalidRequest));
        }
        headers.insert(
            reqwest::header::HeaderName::from_static("idempotency-key"),
            reqwest::header::HeaderValue::from_str(key)
                .map_err(|_| neutral::ModelError::new(neutral::ErrorKind::InvalidRequest))?,
        );
    }
    let http = if target.needs_idempotency_key() {
        host_http(headers)?
    } else {
        pooled.clone()
    };
    let profile = ProviderProfile {
        endpoint: endpoint.into(),
        ..ProviderProfile::default()
    };
    for made in 1..=openai::ATTEMPTS {
        let secret = source
            .resolve(endpoint, credentials)
            .map_err(|_| neutral::ModelError::new(neutral::ErrorKind::Authentication))?;
        let budget = deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or_else(|| neutral::ModelError::new(neutral::ErrorKind::TimedOut))?;
        let failure = match rig_adapter::host_complete(
            &http,
            &profile,
            secret.as_deref().map(String::as_str),
            turn.0,
            request.clone(),
            budget,
            target, // Host retries resolve custody anew and retain their idempotency key.
        )
        .await
        {
            Ok(response) => return rig_adapter::neutral_response(response),
            Err(failure) => failure,
        };
        let asked = match failure.again {
            Again::Soon(wait) => wait,
            Again::Never => None,
        };
        let wait = openai::backoff(made, asked);
        let replay_safe = failure.unprocessed
            || (failure.outcome_unknown && target.needs_idempotency_key() && !key.is_empty());
        if !replay_safe
            || matches!(failure.again, Again::Never)
            || made == openai::ATTEMPTS
            || Instant::now()
                .checked_add(wait)
                .is_none_or(|at| at >= deadline)
        {
            return Err(
                if failure.outcome_unknown && matches!(failure.again, Again::Soon(_)) {
                    neutral::ModelError {
                        kind: neutral::ErrorKind::Provider,
                        message:
                            "Provider outcome is unknown; request was not replayed automatically"
                                .into(),
                    }
                } else {
                    failure.model_error
                },
            );
        }
        tokio::time::sleep(wait).await;
    }
    Err(neutral::ModelError::new(neutral::ErrorKind::Provider))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "current_thread")]
    async fn rig_retries_a_metered_turn_with_rotated_custody_and_one_key() {
        use std::io::{BufRead as _, Read as _, Write as _};
        for (target, refusal) in [
            (Target::Standard, "429 Too Many Requests"),
            (Target::CloudMetered, "429 Too Many Requests"),
            (Target::CloudMetered, "500 Internal Server Error"),
            (Target::CloudMetered, "504 Gateway Timeout"),
        ] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("token");
            fs::write(&path, "first").unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
            let source = ApiKey::File(path.clone());
            let peer = std::thread::spawn(move || {
                let mut requests = Vec::new();
                for attempt in 0..2 {
                    let (mut socket, _) = listener.accept().unwrap();
                    socket
                        .set_read_timeout(Some(Duration::from_secs(5)))
                        .unwrap();
                    let mut reader = std::io::BufReader::new(&mut socket);
                    let mut head = String::new();
                    let mut length = 0;
                    loop {
                        let mut line = String::new();
                        assert!(reader.read_line(&mut line).unwrap() > 0);
                        if line == "\r\n" {
                            break;
                        }
                        let line = line.to_ascii_lowercase();
                        if let Some(value) = line.strip_prefix("content-length:") {
                            length = value.trim().parse().unwrap();
                        }
                        head.push_str(&line);
                    }
                    let mut body = vec![0; length];
                    reader.read_exact(&mut body).unwrap();
                    requests.push((head, body));
                    let (status, body) = if attempt == 0 {
                        fs::write(&path, "second\n").unwrap();
                        (refusal, r#"{"error":{"code":"rate_limit_exceeded"}}"#)
                    } else {
                        (
                            "200 OK",
                            r#"{"id":"reply","object":"chat.completion","created":1,"model":"model","choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
                        )
                    };
                    write!(socket, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                }
                requests
            });
            let request = neutral::Conversation::new("system", "hello").request(Vec::new(), 1024);
            let credentials = ServiceCredentials::open_in(&dir.path().join("vault"));
            let response = complete_async(
                &endpoint,
                &source,
                target,
                ("model", &request),
                "2cb77630-02ac-823f-af97-72c25a7e64b7",
                // Room for the backoff and a fresh client per attempt on a
                // loaded machine; the peer answers at once.
                Instant::now() + Duration::from_secs(60),
                &credentials,
            )
            .await
            .unwrap();
            assert_eq!(response.stop_reason, neutral::StopReason::EndTurn);
            let requests = peer.join().unwrap();
            assert!(requests[0].0.contains("authorization: bearer first\r\n"));
            assert!(requests[1].0.contains("authorization: bearer second\r\n"));
            for (head, encoded) in &requests {
                assert_eq!(
                    head.contains("idempotency-key: 2cb77630-02ac-823f-af97-72c25a7e64b7\r\n"),
                    target.needs_idempotency_key()
                );
                let body: serde_json::Value = serde_json::from_slice(encoded).unwrap();
                if target.needs_idempotency_key() {
                    assert_eq!(body["max_completion_tokens"], 1024);
                    assert!(body.get("max_tokens").is_none());
                } else {
                    assert_eq!(body["max_tokens"], 1024);
                    assert!(body.get("max_completion_tokens").is_none());
                }
            }
            assert_eq!(requests[0].1, requests[1].1);
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn billing_429_preserves_classification_without_retrying() {
        use std::io::{BufRead as _, Read as _, Write as _};
        for code in [
            "insufficient_quota",
            "credit_balance_too_low",
            "billing_hard_limit_reached",
        ] {
            let dir = tempfile::tempdir().unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
            let peer = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(60)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(&mut socket);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                reader.read_exact(&mut vec![0; length]).unwrap();
                let body = serde_json::json!({"error":{"code":code,"message":"secret-never-echo"}})
                    .to_string();
                write!(socket, "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
                // A retry would hit a refused connection and replace the billing error.
            });
            let request = neutral::Conversation::new("system", "hello").request(Vec::new(), 64);
            let error = complete_async(
                &endpoint,
                &ApiKey::Value("fabricated-key".into()),
                Target::Standard,
                ("model", &request),
                "unused-key",
                // The deadline is not what this checks, and it covers the
                // client's setup too: room for that on a loaded machine
                // (5 s ran out there and turned the 429 into a timeout).
                // The peer answers at once, and a retry would be refused.
                Instant::now() + Duration::from_secs(60),
                &ServiceCredentials::open_in(&dir.path().join("vault")),
            )
            .await
            .unwrap_err();
            assert_eq!(
                error,
                neutral::ModelError::new(neutral::ErrorKind::CreditsExhausted),
                "{code}"
            );
            assert!(!error.message.contains("secret-never-echo"));
            peer.join().unwrap();
        }
    }

    #[tokio::test]
    async fn standard_accepted_errors_are_unknown_and_never_replayed() {
        use std::io::{BufRead as _, Read as _, Write as _};
        for status in [500, 504] {
            let dir = tempfile::tempdir().unwrap();
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            let endpoint = format!("http://{}/v1", listener.local_addr().unwrap());
            let peer = std::thread::spawn(move || {
                let (mut socket, _) = listener.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = std::io::BufReader::new(&mut socket);
                let mut length = 0;
                loop {
                    let mut line = String::new();
                    assert!(reader.read_line(&mut line).unwrap() > 0);
                    if line == "\r\n" {
                        break;
                    }
                    if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                        length = value.trim().parse().unwrap();
                    }
                }
                let mut body = vec![0; length];
                reader.read_exact(&mut body).unwrap();
                let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
                assert_eq!(body["model"], "model");
                write!(
                    socket,
                    "HTTP/1.1 {status} Error\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{{}}"
                )
                .unwrap();
                listener
            });
            let request = neutral::Conversation::new("system", "hello").request(Vec::new(), 64);
            let error = complete_async(
                &endpoint,
                &ApiKey::None,
                Target::Standard,
                ("model", &request),
                "not-deduplicated",
                Instant::now() + Duration::from_secs(3),
                &ServiceCredentials::open_in(&dir.path().join("vault")),
            )
            .await
            .unwrap_err();
            assert_eq!(error.kind, neutral::ErrorKind::Provider);
            assert!(error.message.contains("outcome is unknown"));
            let listener = peer.join().unwrap();
            listener.set_nonblocking(true).unwrap();
            assert_eq!(
                listener.accept().unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock
            );
        }
    }

    #[test]
    fn rotating_file_is_re_adopted_and_debug_is_redacted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("token");
        let credentials = ServiceCredentials::open_in(&dir.path().join("vault"));
        let source = ApiKey::File(path.clone());
        fs::write(&path, "first\n").unwrap();
        assert_eq!(
            source
                .resolve("https://example.com/v1", &credentials)
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("first")
        );
        fs::write(&path, "second").unwrap();
        assert_eq!(
            source
                .resolve("https://example.com/v1", &credentials)
                .unwrap()
                .as_deref()
                .map(String::as_str),
            Some("second")
        );
        fs::write(&path, " ").unwrap();
        assert!(
            source
                .resolve("https://example.com/v1", &credentials)
                .is_err()
        );
        fs::write(&path, "x".repeat(16 * 1024 + 1)).unwrap();
        assert!(
            source
                .resolve("https://example.com/v1", &credentials)
                .is_err()
        );
        assert!(!format!("{:?}", ApiKey::Value("secret".into())).contains("secret"));
    }
}
