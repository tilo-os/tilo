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
/// The wire and output-limit field a trusted host's endpoint takes.
pub use agent_provider_config::{OpenAiApi, ProviderProtocol, TokenLimitParam};
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
    /// The wire the endpoint speaks: `OpenAI` chat completions unless the
    /// operator named Anthropic's Messages API.
    protocol: ProviderProtocol,
    /// The OpenAI-compatible endpoint's output-limit field, when it is not
    /// the one the request names (`max_tokens`).
    token_limit: Option<TokenLimitParam>,
    /// The `OpenAI` API an OpenAI-compatible endpoint's turns take, when the
    /// operator named one; otherwise as the endpoint suggests
    /// ([`agent_provider_config::transform::openai_api`]).
    openai_api: Option<OpenAiApi>,
    /// The config root whose vault adopts the key; the user's own unless
    /// the operator named one.
    credentials: Option<PathBuf>,
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
            protocol: ProviderProtocol::OpenAiCompatible,
            token_limit: None,
            openai_api: None,
            credentials: None,
        })
    }

    /// Adopt the key into the vault under the config root `root` rather
    /// than the user's: a host run that keeps no credential of its own past
    /// it (a benchmark's private state directory), or a session without the
    /// platform's secret store.
    #[must_use]
    pub fn with_credentials_in(mut self, root: impl Into<PathBuf>) -> Self {
        self.credentials = Some(root.into());
        self
    }

    /// Speak `protocol` to the endpoint: Anthropic's Messages API (its
    /// endpoint is the API's origin, `https://api.anthropic.com`) or `OpenAI`
    /// chat completions. Anthropic takes none of the request hints.
    #[must_use]
    pub const fn with_protocol(mut self, protocol: ProviderProtocol) -> Self {
        self.protocol = protocol;
        self
    }

    /// Name the output limit with `param`, as an OpenAI-compatible
    /// endpoint whose models refuse `max_tokens` wants it.
    #[must_use]
    pub const fn with_token_limit_param(mut self, param: TokenLimitParam) -> Self {
        self.token_limit = Some(param);
        self
    }

    /// Ask an OpenAI-compatible endpoint through `api`, whatever its
    /// address suggests: a proxy in front of `OpenAI` takes Responses.
    #[must_use]
    pub const fn with_openai_api(mut self, api: OpenAiApi) -> Self {
        self.openai_api = Some(api);
        self
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
        let profile = ProviderProfile {
            endpoint: endpoint.into(),
            protocol: self.protocol,
            token_limit_param: self.token_limit,
            openai_api: self.openai_api,
            ..ProviderProfile::default()
        };
        // Hints are OpenAI request fields: Anthropic refuses fields it does
        // not know.
        let no_hints = serde_json::json!({});
        let hints = match self.protocol {
            ProviderProtocol::OpenAiCompatible => &self.request_hints,
            ProviderProtocol::Anthropic => &no_hints,
        };
        std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    runtime.block_on(complete_with_http(
                        &profile,
                        source,
                        target,
                        (model, request, hints),
                        key,
                        deadline,
                        &self
                            .credentials
                            .as_deref()
                            .map_or_else(ServiceCredentials::open, ServiceCredentials::open_in),
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
    let profile = ProviderProfile {
        endpoint: endpoint.into(),
        ..ProviderProfile::default()
    };
    complete_with_http(
        &profile,
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
    profile: &ProviderProfile,
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
    let endpoint = profile.endpoint.as_str();
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
            profile,
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
                        .set_read_timeout(Some(tilo_test_env::hang_bound()))
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
                Instant::now() + tilo_test_env::hang_bound(),
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
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
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
                // client's setup too. The peer answers at once, and a retry
                // would be refused.
                Instant::now() + tilo_test_env::hang_bound(),
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
                    .set_read_timeout(Some(tilo_test_env::hang_bound()))
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
                Instant::now() + tilo_test_env::hang_bound(),
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

    /// A refused connection reached no provider: it is classified as
    /// unprocessed, asked again after the backoff, and an endpoint that is
    /// up by then answers.
    #[tokio::test(flavor = "current_thread")]
    async fn a_refused_connection_is_unprocessed_and_asked_again() {
        use std::io::{BufRead as _, Read as _, Write as _};
        let address = {
            let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            listener.local_addr().unwrap()
        };
        let endpoint = format!("http://{address}/v1");
        let refused = host_http(reqwest::header::HeaderMap::new())
            .unwrap()
            .post(&endpoint)
            .send()
            .await
            .unwrap_err();
        let failure = rig_adapter::send_failure(&refused);
        assert!(failure.unprocessed && !failure.outcome_unknown);
        assert_eq!(failure.again, Again::Soon(None));
        assert_eq!(failure.model_error.kind, neutral::ErrorKind::Transport);

        // The endpoint comes up within the first backoff (half a second).
        let peer = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(150));
            let listener = std::net::TcpListener::bind(address).unwrap();
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
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
            let body = r#"{"id":"reply","object":"chat.completion","created":1,"model":"model","choices":[{"index":0,"message":{"role":"assistant","content":"done"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#;
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let dir = tempfile::tempdir().unwrap();
        let request = neutral::Conversation::new("system", "hello").request(Vec::new(), 64);
        let response = complete_async(
            &endpoint,
            &ApiKey::None,
            Target::Standard,
            ("model", &request),
            "not-deduplicated",
            Instant::now() + tilo_test_env::hang_bound(),
            &ServiceCredentials::open_in(&dir.path().join("vault")),
        )
        .await
        .unwrap();
        assert_eq!(response.stop_reason, neutral::StopReason::EndTurn);
        peer.join().unwrap();
    }

    /// A peer answering one request with `answer` (JSON), handing back the
    /// request's head (lower-cased) and body.
    fn answer_once(
        answer: serde_json::Value,
    ) -> (String, std::thread::JoinHandle<(String, serde_json::Value)>) {
        use std::io::{BufRead as _, Read as _, Write as _};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let origin = format!("http://{}", listener.local_addr().unwrap());
        let peer = std::thread::spawn(move || {
            let (mut socket, _) = listener.accept().unwrap();
            socket
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            let mut reader = std::io::BufReader::new(&mut socket);
            let (mut head, mut length) = (String::new(), 0);
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
            let answer = answer.to_string();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{answer}", answer.len()).unwrap();
            (head, serde_json::from_slice(&body).unwrap())
        });
        (origin, peer)
    }

    /// One turn of `client` at `endpoint` with a fabricated key, adopted
    /// into a vault of its own.
    fn one_turn(
        client: CompletionClient,
        endpoint: &str,
    ) -> Result<neutral::ModelResponse, neutral::ModelError> {
        let vault = tempfile::tempdir().unwrap();
        let mut client = client.with_credentials_in(vault.path());
        let request = neutral::Conversation::new("system", "hello").request(Vec::new(), 512);
        client.complete(
            endpoint,
            &ApiKey::Value("fabricated-key".into()),
            Target::Standard,
            "model",
            &request,
            "unused-key",
            tilo_test_env::hang_deadline(),
        )
    }

    #[test]
    fn an_anthropic_endpoint_is_asked_on_its_messages_api_without_openai_hints() {
        let (origin, peer) = answer_once(serde_json::json!({
            "id": "msg", "type": "message", "role": "assistant", "model": "model",
            "content": [{"type": "text", "text": "done"}],
            "stop_reason": "end_turn", "stop_sequence": null,
            "usage": {"input_tokens": 40, "output_tokens": 9,
                      "cache_read_input_tokens": 900, "cache_creation_input_tokens": 60}
        }));
        let client = CompletionClient::new()
            .unwrap()
            .with_request_hints(
                &serde_json::json!({"service_tier": "flex", "reasoning_effort": "low"}),
            )
            .with_protocol(ProviderProtocol::Anthropic);
        let response = one_turn(client, &origin).unwrap();
        let (head, body) = peer.join().unwrap();
        assert!(head.starts_with("post /v1/messages "), "{head}");
        assert!(head.contains("x-api-key: fabricated-key\r\n"), "{head}");
        assert!(!head.contains("authorization:"), "{head}");
        assert_eq!(body["max_tokens"], 512);
        assert!(
            body.get("service_tier").is_none() && body.get("reasoning_effort").is_none(),
            "{body}"
        );
        assert_eq!(response.stop_reason, neutral::StopReason::EndTurn);
        // Every input token, those read from and written to the cache too.
        assert_eq!(response.usage.input_tokens, 1000);
        assert_eq!(response.usage.cached_input_tokens, 900);
        assert_eq!(response.usage.cache_write_tokens, 60);
        assert_eq!(response.usage.output_tokens, 9);
    }

    #[test]
    fn an_endpoint_told_its_output_limit_field_gets_that_field() {
        let (origin, peer) = answer_once(serde_json::json!({
            "id": "reply", "object": "chat.completion", "created": 1, "model": "model",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "done"},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 1000, "completion_tokens": 30, "total_tokens": 1030,
                      "prompt_tokens_details": {"cached_tokens": 768},
                      "completion_tokens_details": {"reasoning_tokens": 20}}
        }));
        let client = CompletionClient::new()
            .unwrap()
            .with_token_limit_param(TokenLimitParam::MaxCompletionTokens);
        let response = one_turn(client, &format!("{origin}/v1")).unwrap();
        let (head, body) = peer.join().unwrap();
        assert!(head.starts_with("post /v1/chat/completions "), "{head}");
        assert_eq!(body["max_completion_tokens"], 512);
        assert!(body.get("max_tokens").is_none(), "{body}");
        assert_eq!(response.usage.input_tokens, 1000);
        assert_eq!(response.usage.cached_input_tokens, 768);
        assert_eq!(response.usage.reasoning_tokens, 20);
    }

    /// A Responses reply: `output` items, its `status` and, when it is
    /// incomplete, why.
    fn responses_reply(
        output: &serde_json::Value,
        status: &str,
        incomplete: Option<&str>,
    ) -> serde_json::Value {
        serde_json::json!({
            "id": "resp_1", "object": "response", "created_at": 1, "model": "gpt-6-luna",
            "status": status,
            "incomplete_details": incomplete.map(|reason| serde_json::json!({"reason": reason})),
            "output": output,
            "usage": {"input_tokens": 1000, "input_tokens_details": {"cached_tokens": 768},
                      "output_tokens": 30, "output_tokens_details": {"reasoning_tokens": 20},
                      "total_tokens": 1030}
        })
    }

    fn plan_tool() -> neutral::ToolDefinition {
        neutral::ToolDefinition {
            name: "plan".into(),
            description: "Plan the steps".into(),
            input_schema: serde_json::json!({
                "type": "object", "properties": {"steps": {"type": "array"}}
            }),
        }
    }

    /// A client asking through Responses with the hints a preset sends.
    fn responses_client() -> CompletionClient {
        CompletionClient::new()
            .unwrap()
            .with_request_hints(
                &serde_json::json!({"service_tier": "priority", "reasoning_effort": "low"}),
            )
            .with_token_limit_param(TokenLimitParam::MaxCompletionTokens)
            .with_openai_api(OpenAiApi::Responses)
    }

    /// One turn of `request` through `client` at `endpoint`.
    fn turn(
        client: CompletionClient,
        endpoint: &str,
        request: &neutral::ModelRequest,
    ) -> Result<neutral::ModelResponse, neutral::ModelError> {
        let vault = tempfile::tempdir().unwrap();
        let mut client = client.with_credentials_in(vault.path());
        client.complete(
            endpoint,
            &ApiKey::Value("fabricated-key".into()),
            Target::Standard,
            "gpt-6-luna",
            request,
            "unused-key",
            tilo_test_env::hang_deadline(),
        )
    }

    #[test]
    fn openai_reasons_and_calls_tools_through_responses_and_carries_its_reasoning() {
        let reasoning = serde_json::json!({
            "type": "reasoning", "id": "rs_1", "summary": [], "encrypted_content": "Y2lwaGVy+/="
        });
        let call = serde_json::json!({
            "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "plan",
            "arguments": "{\"steps\":[]}", "status": "completed"
        });
        let (origin, peer) = answer_once(responses_reply(
            &serde_json::json!([reasoning, call]),
            "completed",
            None,
        ));
        let mut asked =
            neutral::Conversation::new("system", "hello").request(vec![plan_tool()], 512);
        asked.tool_choice = neutral::ToolChoice::Required;
        asked.messages.push(neutral::Message {
            role: neutral::Role::User,
            content: vec![neutral::Content::Image {
                media_type: "image/png".into(),
                base64: "iVBORw0K".into(),
            }],
        });
        // The endpoint as a host planner names it, chat completions' route.
        let endpoint = format!("{origin}/v1/chat/completions");
        let response = turn(responses_client(), &endpoint, &asked).unwrap();
        let (head, body) = peer.join().unwrap();
        assert!(head.starts_with("post /v1/responses "), "{head}");
        assert!(
            head.contains("authorization: bearer fabricated-key\r\n"),
            "{head}"
        );
        assert_eq!(body["model"], "gpt-6-luna");
        assert_eq!(body["reasoning"], serde_json::json!({"effort": "low"}));
        assert_eq!(body["service_tier"], "priority");
        assert_eq!(body["store"], false);
        assert_eq!(
            body["include"],
            serde_json::json!(["reasoning.encrypted_content"])
        );
        assert_eq!(body["max_output_tokens"], 512);
        for chat_only in [
            "reasoning_effort",
            "max_completion_tokens",
            "max_tokens",
            "messages",
        ] {
            assert!(body.get(chat_only).is_none(), "{chat_only}: {body}");
        }
        assert_eq!(body["tool_choice"], "required");
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["name"], "plan");
        assert_eq!(body["instructions"], "system");
        let image = body["input"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|item| item["content"].as_array().cloned().unwrap_or_default())
            .find(|part| part["type"] == "input_image")
            .unwrap_or_else(|| panic!("no picture in {body}"));
        assert_eq!(image["image_url"], "data:image/png;base64,iVBORw0K");

        assert_eq!(response.stop_reason, neutral::StopReason::ToolCalls);
        assert_eq!(response.usage.input_tokens, 1000);
        assert_eq!(response.usage.cached_input_tokens, 768);
        assert_eq!(response.usage.output_tokens, 30);
        assert_eq!(response.usage.reasoning_tokens, 20);
        let calls = response.tool_calls().unwrap();
        assert_eq!(
            (calls[0].id.as_str(), calls[0].name.as_str()),
            ("call_1", "plan")
        );
        assert_eq!(calls[0].input, serde_json::json!({"steps": []}));
        assert!(matches!(
            &response.message.content[0],
            neutral::Content::Reasoning { text, extra_content: Some(_) } if text.is_empty()
        ));

        // The next turn carries the reasoning back as the provider issued
        // it, ahead of the call it led to and that call's result.
        carries_reasoning_into(asked, response.message);
    }

    /// The turn after `asked`, whose answer was `answered`: one call of
    /// `plan` (`call_1`, item `fc_1`) after reasoning `rs_1`.
    fn carries_reasoning_into(asked: neutral::ModelRequest, answered: neutral::Message) {
        let mut next = asked;
        next.messages.push(answered);
        next.messages.push(neutral::Message {
            role: neutral::Role::Tool,
            content: vec![neutral::Content::ToolResult(neutral::ToolResult {
                call_id: "call_1".into(),
                content: "planned".into(),
                is_error: false,
            })],
        });
        let (origin, peer) = answer_once(responses_reply(
            &serde_json::json!([{"type": "message", "id": "msg_2", "role": "assistant",
                "status": "completed",
                "content": [{"type": "output_text", "text": "done", "annotations": []}]}]),
            "completed",
            None,
        ));
        let answer = turn(responses_client(), &format!("{origin}/v1"), &next).unwrap();
        assert_eq!(answer.stop_reason, neutral::StopReason::EndTurn);
        let (_, body) = peer.join().unwrap();
        let input = body["input"].as_array().unwrap();
        let at = |kind: &str| {
            input
                .iter()
                .position(|item| item["type"] == kind)
                .unwrap_or_else(|| panic!("no {kind} in {body}"))
        };
        let (thought, called, result) = (
            at("reasoning"),
            at("function_call"),
            at("function_call_output"),
        );
        assert!(thought < called && called < result, "{body}");
        assert_eq!(input[thought]["id"], "rs_1");
        assert_eq!(input[thought]["encrypted_content"], "Y2lwaGVy+/=");
        assert_eq!(input[called]["id"], "fc_1");
        assert_eq!(input[called]["call_id"], "call_1");
        assert_eq!(input[result]["call_id"], "call_1");
        assert_eq!(input[result]["output"], "planned");
    }

    #[test]
    fn a_responses_refusal_and_a_cut_off_answer_are_never_ordinary_answers() {
        let refusal = serde_json::json!([{"type": "message", "id": "msg_1", "role": "assistant",
            "status": "completed",
            "content": [{"type": "refusal", "refusal": "I can't help with that."}]}]);
        let cut = serde_json::json!([{"type": "message", "id": "msg_1", "role": "assistant",
            "status": "incomplete",
            "content": [{"type": "output_text", "text": "The pla", "annotations": []}]}]);
        for (output, status, reason, stop) in [
            (refusal, "completed", None, neutral::StopReason::Refusal),
            (
                cut,
                "incomplete",
                Some("max_output_tokens"),
                neutral::StopReason::OutputLimit,
            ),
        ] {
            let (origin, peer) = answer_once(responses_reply(&output, status, reason));
            let request = neutral::Conversation::new("system", "hello").request(Vec::new(), 64);
            let response = turn(responses_client(), &format!("{origin}/v1"), &request).unwrap();
            peer.join().unwrap();
            assert_eq!(response.stop_reason, stop);
            assert!(response.tool_calls().is_err());
        }
    }

    #[test]
    fn a_compatible_server_still_takes_chat_completions_with_its_hints() {
        let (origin, peer) = answer_once(serde_json::json!({
            "id": "reply", "object": "chat.completion", "created": 1, "model": "model",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": "done"},
                         "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13}
        }));
        let client = CompletionClient::new().unwrap().with_request_hints(
            &serde_json::json!({"service_tier": "priority", "reasoning_effort": "low"}),
        );
        one_turn(client, &format!("{origin}/v1")).unwrap();
        let (head, body) = peer.join().unwrap();
        assert!(head.starts_with("post /v1/chat/completions "), "{head}");
        assert_eq!(body["reasoning_effort"], "low");
        assert_eq!(body["service_tier"], "priority");
        for responses_only in ["store", "include", "reasoning", "input"] {
            assert!(
                body.get(responses_only).is_none(),
                "{responses_only}: {body}"
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
