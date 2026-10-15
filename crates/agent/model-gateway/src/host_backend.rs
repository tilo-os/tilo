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

/// One host discovery turn, including bounded retries. All attempts share the
/// supplied idempotency key; it must identify this exact logical request.
///
/// # Errors
/// Returns bounded diagnostics for credential, provider, deadline or runtime
/// failures. No provider body, key or credential-file path is returned.
pub fn complete(
    endpoint: &str,
    source: &ApiKey,
    target: Target,
    model: &str,
    request: &neutral::ModelRequest,
    key: &str,
    deadline: Instant,
) -> Result<neutral::ModelResponse, String> {
    CompletionClient::new()?.complete(endpoint, source, target, model, request, key, deadline)
}

/// Reusable trusted-host transport for neutral completion turns.
///
/// Keeps its runtime and ordinary HTTP connection pool alive between turns.
/// Metered turns retain a dedicated client so their idempotency header cannot
/// leak into another logical request.
pub struct CompletionClient {
    runtime: Option<tokio::runtime::Runtime>,
    http: reqwest::Client,
}

impl CompletionClient {
    /// Construct a transport with redirects and automatic HTTP retries disabled.
    ///
    /// # Errors
    /// Returns a bounded diagnostic if the runtime or HTTP client cannot start.
    pub fn new() -> Result<Self, String> {
        let http = host_http(reqwest::header::HeaderMap::new())?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "no model runtime".to_string())?;
        Ok(Self {
            runtime: Some(runtime),
            http,
        })
    }

    /// Complete one neutral turn with the same custody and bounded retry policy
    /// as [`complete`]. Rotating credentials are resolved before every attempt.
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
    ) -> Result<neutral::ModelResponse, String> {
        let body = host_request(model, request)?;
        let runtime = self.runtime.as_ref().ok_or("no model runtime")?;
        let answer = std::thread::scope(|scope| {
            scope
                .spawn(|| {
                    runtime.block_on(complete_with_http(
                        endpoint,
                        source,
                        target,
                        &body,
                        key,
                        deadline,
                        &ServiceCredentials::open(),
                        &self.http,
                    ))
                })
                .join()
                .unwrap_or_else(|_| Err("the model call panicked".into()))
        })?;
        host_response(&answer)
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

/// Decode strictly: malformed arguments must not become an empty object.
fn host_response(answer: &serde_json::Value) -> Result<neutral::ModelResponse, String> {
    let message = answer
        .pointer("/choices/0/message")
        .filter(|message| message.is_object())
        .ok_or("the model's answer had no message")?;
    let stop_reason = match answer
        .pointer("/choices/0/finish_reason")
        .and_then(serde_json::Value::as_str)
    {
        Some("length") => neutral::StopReason::OutputLimit,
        Some("content_filter") => neutral::StopReason::Refusal,
        _ if message
            .get("refusal")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|text| !text.is_empty()) =>
        {
            neutral::StopReason::Refusal
        }
        Some("tool_calls") => neutral::StopReason::ToolCalls,
        _ => neutral::StopReason::EndTurn,
    };
    let mut content = Vec::new();
    if let Some(text) = message.get("content").and_then(serde_json::Value::as_str) {
        content.push(neutral::Content::Text { text: text.into() });
    }
    if let Some(calls) = message.get("tool_calls").filter(|calls| !calls.is_null()) {
        for call in calls.as_array().ok_or("invalid model tool calls")? {
            let id = call
                .get("id")
                .and_then(serde_json::Value::as_str)
                .ok_or("tool call had no id")?;
            let name = call
                .pointer("/function/name")
                .and_then(serde_json::Value::as_str)
                .ok_or("tool call had no name")?;
            let arguments = call
                .pointer("/function/arguments")
                .and_then(serde_json::Value::as_str)
                .ok_or("tool call had no arguments")?;
            let input =
                serde_json::from_str(arguments).map_err(|_| "invalid model tool arguments")?;
            content.push(neutral::Content::ToolCall(neutral::ToolCall {
                id: id.into(),
                name: name.into(),
                input,
            }));
        }
    }
    let tokens = |name| {
        answer
            .get("usage")
            .and_then(|usage| usage.get(name))
            .and_then(serde_json::Value::as_u64)
            .unwrap_or_default()
    };
    let response = neutral::ModelResponse {
        stop_reason,
        message: neutral::Message {
            role: neutral::Role::Assistant,
            content,
        },
        usage: neutral::Usage {
            input_tokens: tokens("prompt_tokens"),
            output_tokens: tokens("completion_tokens"),
            total_tokens: tokens("total_tokens"),
        },
    };
    response.tool_calls().map_err(|error| error.to_string())?;
    Ok(response)
}

/// Host compatibility encoding lives inside the gateway, never in a planner.
/// Unsupported multimodal history is rejected rather than silently dropped.
fn host_request(model: &str, request: &neutral::ModelRequest) -> Result<serde_json::Value, String> {
    use neutral::{Content, Role};
    let mut messages = Vec::new();
    for message in &request.messages {
        let mut text = String::new();
        let mut calls = Vec::new();
        for block in &message.content {
            match block {
                Content::Text { text: part } => text.push_str(part),
                Content::ToolCall(call) => {
                    call.validate().map_err(|error| error.to_string())?;
                    calls.push(serde_json::json!({
                        "id": call.id, "type": "function",
                        "function": {"name": call.name, "arguments": call.input.to_string()}
                    }));
                }
                Content::ToolResult(result) => messages.push(serde_json::json!({
                    "role": "tool", "tool_call_id": result.call_id, "content": result.content
                })),
                Content::Reasoning { .. } | Content::Image { .. } => {
                    return Err("host compatibility transport supports text and tools only".into());
                }
            }
        }
        if message.role != Role::Tool {
            let role = match message.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };
            messages.push(if calls.is_empty() {
                serde_json::json!({"role": role, "content": text})
            } else {
                serde_json::json!({"role": role, "content": text, "tool_calls": calls})
            });
        }
    }
    Ok(serde_json::json!({
        "model": model, "messages": messages,
        "tools": request.tools.iter().map(|tool| serde_json::json!({
            "type": "function", "function": {
                "name": tool.name, "description": tool.description, "parameters": tool.input_schema
            }
        })).collect::<Vec<_>>(),
        "tool_choice": match request.tool_choice {
            neutral::ToolChoice::Auto => "auto", neutral::ToolChoice::Required => "required",
            neutral::ToolChoice::None => "none",
        },
        "max_tokens": request.max_output_tokens,
    }))
}

fn host_http(headers: reqwest::header::HeaderMap) -> Result<reqwest::Client, String> {
    tilo_net::http::client()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .connect_timeout(CONNECT_TIMEOUT)
        .default_headers(headers)
        .build()
        .map_err(|_| "no model HTTP client".to_string())
}

#[cfg(test)]
async fn complete_async(
    endpoint: &str,
    source: &ApiKey,
    target: Target,
    body: &serde_json::Value,
    key: &str,
    deadline: Instant,
    credentials: &ServiceCredentials,
) -> Result<serde_json::Value, String> {
    let http = host_http(reqwest::header::HeaderMap::new())?;
    complete_with_http(
        endpoint,
        source,
        target,
        body,
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
    body: &serde_json::Value,
    key: &str,
    deadline: Instant,
    credentials: &ServiceCredentials,
    pooled: &reqwest::Client,
) -> Result<serde_json::Value, String> {
    let mut headers = reqwest::header::HeaderMap::new();
    if target.needs_idempotency_key() {
        headers.insert(
            reqwest::header::HeaderName::from_static("idempotency-key"),
            reqwest::header::HeaderValue::from_str(key)
                .map_err(|_| "invalid model idempotency key".to_string())?,
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
        let secret = source.resolve(endpoint, credentials)?;
        let budget = deadline
            .checked_duration_since(Instant::now())
            .filter(|left| !left.is_zero())
            .ok_or("the model call timed out")?;
        let failure = match rig_adapter::legacy(
            &http,
            &profile,
            secret.as_deref().map(String::as_str),
            target.shape(body.clone()),
            budget,
            None,
            false, // Host retries resolve custody anew and retain their idempotency key.
        )
        .await
        {
            Ok(text) => {
                return serde_json::from_str(&text).map_err(|_| "unreadable model answer".into());
            }
            Err(failure) => failure,
        };
        if failure.status == Some(402) || failure.why.contains("credits are exhausted") {
            return Err(format!(
                "{}; add credit or choose another model",
                openai::OUT_OF_CREDITS
            ));
        }
        let asked = match failure.again {
            Again::Soon(wait) => wait,
            Again::Never => None,
        };
        let wait = openai::backoff(made, asked);
        if matches!(failure.again, Again::Never)
            || made == openai::ATTEMPTS
            || Instant::now()
                .checked_add(wait)
                .is_none_or(|at| at >= deadline)
        {
            return Err(if made == 1 {
                failure.why
            } else {
                format!("{} (after {made} attempts)", failure.why)
            });
        }
        tokio::time::sleep(wait).await;
    }
    Err("the model exhausted its attempts".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn neutral_host_codec_preserves_order_types_and_rejects_partial_calls() {
        use neutral::{Content, Conversation, ToolResult};
        let answer = serde_json::json!({
            "choices": [{"finish_reason": "tool_calls", "message": {
                "role": "assistant", "tool_calls": [
                    {"id": "b", "type": "function", "function": {
                        "name": "test", "arguments": r#"{"enabled":false,"count":2}"#
                    }},
                    {"id": "a", "type": "function", "function": {
                        "name": "test", "arguments": "{}"
                    }}
                ]
            }}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 4, "total_tokens": 7}
        });
        let response = host_response(&answer).unwrap();
        assert_eq!(response.usage.total_tokens, 7);
        let calls = response.tool_calls().unwrap();
        assert_eq!(
            calls
                .iter()
                .map(|call| call.id.as_str())
                .collect::<Vec<_>>(),
            ["b", "a"]
        );
        assert_eq!(calls[0].input["enabled"], false);
        assert_eq!(calls[0].input["count"], 2);
        let mut history = Conversation::new("system", "user");
        history
            .push(
                response,
                ["b", "a"]
                    .into_iter()
                    .map(|id| ToolResult {
                        call_id: id.into(),
                        content: id.into(),
                        is_error: false,
                    })
                    .collect(),
            )
            .unwrap();
        let body = host_request("model", &history.request(Vec::new(), 123)).unwrap();
        assert_eq!(body["messages"][2]["tool_calls"][0]["id"], "b");
        assert_eq!(body["messages"][3]["tool_call_id"], "b");
        assert_eq!(body["messages"][4]["tool_call_id"], "a");
        assert_eq!(body["max_tokens"], 123);
        for reason in ["length", "content_filter"] {
            let mut partial = answer.clone();
            partial["choices"][0]["finish_reason"] = reason.into();
            assert!(host_response(&partial).is_err());
        }
        let mut malformed = answer;
        malformed["choices"][0]["message"]["tool_calls"][0]["function"]["arguments"] = "{".into();
        assert!(host_response(&malformed).is_err());
        let mut request = history.request(Vec::new(), 123);
        request.messages[0].content.push(Content::Image {
            media_type: "image/png".into(),
            base64: "data".into(),
        });
        assert!(host_request("model", &request).is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rig_retries_a_metered_turn_with_rotated_custody_and_one_key() {
        use std::io::{BufRead as _, Read as _, Write as _};
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
                    (
                        "429 Too Many Requests",
                        r#"{"error":{"code":"rate_limit_exceeded"}}"#,
                    )
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
        let body = serde_json::json!({"model":"model", "messages":[{"role":"user","content":"hello"}], "max_tokens":1024});
        let credentials = ServiceCredentials::open_in(&dir.path().join("vault"));
        let response = complete_async(
            &endpoint,
            &source,
            Target::CloudMetered,
            &body,
            "2cb77630-02ac-823f-af97-72c25a7e64b7",
            Instant::now() + Duration::from_secs(10),
            &credentials,
        )
        .await
        .unwrap();
        assert_eq!(response["choices"][0]["message"]["content"], "done");
        let requests = peer.join().unwrap();
        assert!(requests[0].0.contains("authorization: bearer first\r\n"));
        assert!(requests[1].0.contains("authorization: bearer second\r\n"));
        for (head, encoded) in &requests {
            assert!(head.contains("idempotency-key: 2cb77630-02ac-823f-af97-72c25a7e64b7\r\n"));
            let body: serde_json::Value = serde_json::from_slice(encoded).unwrap();
            assert_eq!(body["max_completion_tokens"], 1024);
            assert!(body.get("max_tokens").is_none());
        }
        assert_eq!(requests[0].1, requests[1].1);
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
