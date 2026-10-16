//! Rig owns the provider wires and decoders. This transport owns the trust
//! boundary: no ambient clients, redirects, automatic retries or unbounded
//! response buffers. Legacy RPC bodies are a compatibility input, not a second
//! provider implementation.
use super::*;
use bytes::Bytes;
use futures_util::StreamExt as _;
use rig_core::completion::{CompletionRequest, CompletionResponse};
use rig_core::error::ProviderError;
use rig_core::http_client::{self as http, HttpClientExt, LazyBody, MultipartForm};
use rig_core::providers::{anthropic, openai};
use rig_core::streaming::{Item, PartKind, StreamEvent};

#[cfg(test)]
#[path = "google_rig_tests.rs"]
mod google_tests;
#[cfg(test)]
#[path = "provider_conformance_tests.rs"]
mod provider_conformance_tests;
#[cfg(test)]
#[path = "rig_stream_tests.rs"]
pub(super) mod stream_tests;
#[cfg(test)]
#[path = "rig_tests.rs"]
mod tests;

/// Framing-only guard: Rig intentionally discards an undelimited SSE tail.
/// Do not let a prior finish reason turn a cut-off final event into success.
#[derive(Default)]
struct SseTail {
    line_bytes: usize,
    comment: bool,
    pending: bool,
    cr: bool,
    prefix: Vec<u8>,
    lifecycle: MessageLifecycle,
}
#[derive(Default, PartialEq, Eq)]
enum MessageLifecycle {
    #[default]
    Initial,
    Started,
    Stopped,
    Invalid,
}
impl SseTail {
    fn push(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if byte == b'\n' && self.cr {
                self.cr = false;
                continue;
            }
            self.cr = byte == b'\r';
            if matches!(byte, b'\r' | b'\n') {
                // Check envelope ordering only, never decode provider JSON.
                // Rig accepts partial message metadata; Messages requires one
                // opening envelope and a closing message_stop before EOF.
                if let Some(event) = self.prefix.strip_prefix(b"event:") {
                    let event = event.trim_ascii();
                    self.lifecycle = match (event, &self.lifecycle) {
                        (b"message_start", MessageLifecycle::Initial) => MessageLifecycle::Started,
                        (b"message_stop", MessageLifecycle::Started) => MessageLifecycle::Stopped,
                        (b"message_start" | b"message_stop", _) => MessageLifecycle::Invalid,
                        (_, state)
                            if event.starts_with(b"content_block_")
                                || event == b"message_delta" =>
                        {
                            if *state == MessageLifecycle::Started {
                                MessageLifecycle::Started
                            } else {
                                MessageLifecycle::Invalid
                            }
                        }
                        _ => std::mem::take(&mut self.lifecycle),
                    };
                }
                self.prefix.clear();
                if self.line_bytes == 0 {
                    self.pending = false;
                } else if !self.comment {
                    self.pending = true;
                }
                self.line_bytes = 0;
                self.comment = false;
            } else {
                if self.line_bytes == 0 {
                    self.comment = byte == b':';
                }
                if self.prefix.len() < 64 {
                    self.prefix.push(byte);
                }
                self.line_bytes = self.line_bytes.saturating_add(1);
            }
        }
    }
    fn incomplete(&self) -> bool {
        self.pending || (self.line_bytes > 0 && !self.comment)
    }
}

type StreamEnd = Arc<tokio::sync::Mutex<Option<StreamDrain>>>;

struct StreamDrain {
    receiver: tokio::sync::oneshot::Receiver<Result<(), String>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for StreamDrain {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Clone)]
struct GatewayHttp {
    client: reqwest::Client,
    endpoint: String,
    profile: ProviderProfile,
    model: String,
    streaming: bool,
    target: crate::openai::Target,
    retry_refusals: bool,
    stream_end: StreamEnd,
    authenticated: bool,
    profile_headers: Vec<(&'static str, reqwest::header::HeaderValue)>,
    budget: Duration,
    /// Exact legacy body, after gateway policy. Neutral requests use Rig's
    /// encoding instead; both take the same provider decoder and transport.
    body: Option<serde_json::Value>,
    failure: Arc<Mutex<Option<ProviderFailure>>>,
    status: Arc<Mutex<Option<u16>>>,
    unary_body: Arc<Mutex<Option<String>>>,
}

impl GatewayHttp {
    async fn dispatch(&self, request: http::Request<Bytes>) -> http::Result<reqwest::Response> {
        let (mut head, bytes) = request.into_parts();
        if !self.authenticated {
            head.headers.remove(reqwest::header::AUTHORIZATION);
        }
        for (name, value) in &self.profile_headers {
            head.headers.insert(*name, value.clone());
        }
        let body = self
            .body
            .clone()
            .map_or_else(|| serde_json::from_slice(&bytes), Ok)
            .map_err(|_| {
                http::Error::instance(std::io::Error::other("invalid request encoding"))
            })?;
        let body = outbound_body(&self.profile, &self.model, body, self.streaming)
            .map_err(|_| http::Error::instance(std::io::Error::other("invalid request body")))?;
        let body = self.target.shape(body);
        if !self.retry_refusals {
            return self
                .dispatch_once(&head.headers, &body, self.budget)
                .await
                .map_err(|failure| self.fail(failure));
        }
        let started = Instant::now();
        plan_send(
            || self.budget.checked_sub(started.elapsed()),
            tokio::time::sleep,
            &|| false, // No output can precede the HTTP response headers.
            |budget| self.dispatch_once(&head.headers, &body, budget),
        )
        .await
        .map_err(|failure| self.fail(failure))
    }

    async fn dispatch_once(
        &self,
        headers: &reqwest::header::HeaderMap,
        body: &serde_json::Value,
        budget: Duration,
    ) -> Result<reqwest::Response, ProviderFailure> {
        let outgoing = self
            .client
            .post(&self.endpoint)
            .timeout(budget)
            .headers(headers.clone())
            .json(body);
        let response = outgoing.send().await.map_err(|error| ProviderFailure {
            model_error: crate::openai::Failure::transport(error.is_timeout(), error.is_connect())
                .model_error(),
            why: if error.is_timeout() {
                "provider request timed out"
            } else {
                "provider transport failed"
            }
            .into(),
            again: if error.is_timeout() || error.is_connect() {
                Again::Soon(None)
            } else {
                Again::Never
            },
            outcome_unknown: !error.is_builder() && !error.is_connect(),
            unprocessed: error.is_connect(),
            status: None,
        })?;
        let status = response.status();
        *self
            .status
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(status.as_u16());
        if !status.is_success() {
            let retry_header = response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned);
            let detail = error_body(response).await;
            let classified = crate::openai::Failure::from_http(
                status.as_u16(),
                &String::from_utf8_lossy(&detail),
                retry_header.as_deref(),
            );
            let again = if classified.retryable() {
                Again::Soon(classified.retry_after)
            } else {
                Again::Never
            };
            let retry_after = match again {
                Again::Soon(Some(wait)) if !wait.is_zero() && wait <= Duration::from_secs(3600) => {
                    format!(" retry_after_s={}", wait.as_secs_f32().ceil())
                }
                _ => String::new(),
            };
            let unprocessed = classified.refused_unprocessed(status.as_u16());
            return Err(ProviderFailure {
                model_error: classified.model_error(),
                why: format!(
                    "provider returned HTTP {}{}{retry_after}",
                    status.as_u16(),
                    classified_error_advice(&classified)
                ),
                again,
                outcome_unknown: !unprocessed,
                unprocessed,
                status: Some(status.as_u16()),
            });
        }
        Ok(response)
    }

    fn fail(&self, failure: ProviderFailure) -> http::Error {
        *self
            .failure
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(failure);
        http::Error::instance(std::io::Error::other("provider transport failed"))
    }
}

impl HttpClientExt for GatewayHttp {
    fn send<T, U>(
        &self,
        request: http::Request<T>,
    ) -> impl Future<Output = http::Result<http::Response<LazyBody<U>>>> + Send + 'static
    where
        T: Into<Bytes> + Send,
        U: From<Bytes> + Send + 'static,
    {
        let this = self.clone();
        let request = request.map(Into::into);
        async move {
            let mut response = this.dispatch(request).await?;
            let mut head = http::Response::builder().status(response.status());
            if let Some(headers) = head.headers_mut() {
                *headers = response.headers().clone();
            }
            let body: LazyBody<U> = Box::pin(async move {
                let mut body = Vec::new();
                while let Some(chunk) = response.chunk().await.map_err(http::Error::instance)? {
                    if body.len().saturating_add(chunk.len()) > MAX_STREAM_BYTES {
                        return Err(http::Error::instance(std::io::Error::other(
                            "provider response exceeds limit",
                        )));
                    }
                    body.extend_from_slice(&chunk);
                }
                if this.body.is_some() {
                    *this
                        .unary_body
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner) =
                        Some(String::from_utf8_lossy(&body).into_owned());
                }
                Ok(U::from(Bytes::from(body)))
            });
            Ok(head.body(body)?)
        }
    }

    fn send_multipart<U>(
        &self,
        _request: http::Request<MultipartForm>,
    ) -> impl Future<Output = http::Result<http::Response<LazyBody<U>>>> + Send + 'static
    where
        U: From<Bytes> + Send + 'static,
    {
        std::future::ready(Err(http::Error::instance(std::io::Error::other(
            "multipart inference is not authorized",
        ))))
    }

    fn send_streaming<T>(
        &self,
        request: http::Request<T>,
    ) -> impl Future<Output = http::Result<http::StreamingResponse>> + Send
    where
        T: Into<Bytes> + Send,
    {
        let request = request.map(Into::into);
        async move {
            let response = self.dispatch(request).await?;
            let mut head = http::Response::builder().status(response.status());
            if let Some(headers) = head.headers_mut() {
                *headers = response.headers().clone();
            }
            // Rig may finish at message_delta before consuming message_stop.
            // Drain and validate the bounded envelope before authorizing tools,
            // while forwarding chunks promptly through a bounded queue.
            let (send, receive) = tokio::sync::mpsc::channel(2);
            let (done, receiver) = tokio::sync::oneshot::channel();
            let protocol = self.profile.protocol;
            let task = tokio::spawn(async move {
                let result = drain_stream(response, protocol, send).await;
                let _ = done.send(result);
            });
            *self.stream_end.lock().await = Some(StreamDrain { receiver, task });
            let body = futures_util::stream::unfold(receive, |mut receive| async move {
                receive.recv().await.map(|chunk| (Ok(chunk), receive))
            });
            let body: http::BoxedStream = Box::pin(body);
            Ok(head.body(body)?)
        }
    }
}

async fn drain_stream(
    mut response: reqwest::Response,
    protocol: ProviderProtocol,
    send: tokio::sync::mpsc::Sender<Bytes>,
) -> Result<(), String> {
    let mut tail = SseTail::default();
    let mut utf8 = Vec::new();
    let mut read = 0usize;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| "provider stream failed")?
    {
        read = read.saturating_add(chunk.len());
        if read > MAX_STREAM_BYTES {
            return Err("provider response exceeds limit".into());
        }
        // Rig's SSE layer replaces invalid UTF-8. Validate transport text
        // before forwarding it, retaining at most three split codepoint bytes.
        utf8.extend_from_slice(&chunk);
        match std::str::from_utf8(&utf8) {
            Ok(_) => utf8.clear(),
            Err(error) if error.error_len().is_some() => {
                return Err("provider stream was not UTF-8".into());
            }
            Err(error) => {
                utf8.drain(..error.valid_up_to());
            }
        }
        tail.push(&chunk);
        // A finished Rig decoder drops its receiver; the bounded remainder
        // still has to pass framing checks. StreamDrain aborts on cancellation.
        let _ = send.send(chunk).await;
    }
    if !utf8.is_empty()
        || tail.incomplete()
        || (protocol == ProviderProtocol::Anthropic && tail.lifecycle != MessageLifecycle::Stopped)
    {
        return Err("provider stream was truncated or had invalid envelope ordering".into());
    }
    Ok(())
}

/// One provider operation. No provider error's Display reaches a caller: Rig
/// errors can include response content, headers or URLs.
pub(super) async fn legacy(
    client: &reqwest::Client,
    profile: &ProviderProfile,
    secret: Option<&str>,
    body: serde_json::Value,
    budget: Duration,
    streaming: Option<(Emit<'_>, &mut PlanTiming, bool)>,
    retry_refusals: bool,
) -> Result<String, ProviderFailure> {
    let model = body
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut request = CompletionRequest::new("legacy gateway request");
    if let Some(tools) = body.get("tools") {
        for tool in tools
            .as_array()
            .ok_or_else(|| stream_failure("invalid tool declarations"))?
        {
            let tool = if profile.protocol == ProviderProtocol::OpenAiCompatible {
                tool.get("function").filter(|_| tool["type"] == "function")
            } else {
                Some(tool)
            }
            .ok_or_else(|| stream_failure("invalid tool declaration"))?;
            let name = tool
                .get("name")
                .and_then(serde_json::Value::as_str)
                .and_then(|name| message::ToolName::new(name).ok())
                .ok_or_else(|| stream_failure("invalid tool name"))?;
            request
                .tools
                .push(rig_core::completion::ToolDefinition::new(
                    name,
                    tool.get("description")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default(),
                    tool.get("parameters")
                        .or_else(|| tool.get("input_schema"))
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!({})),
                ));
        }
    }
    let choice = body.get("tool_choice");
    if choice.is_some_and(|choice| {
        choice == "none" || choice.get("type").is_some_and(|kind| kind == "none")
    }) {
        request.tool_choice = Some(message::ToolChoice::None);
    }
    // Rig's encoder establishes protocol headers. The compatibility transport
    // supplies the already policy-checked legacy body verbatim.
    request.max_tokens = Some(
        body.get("max_tokens")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(4096),
    );
    let transport = GatewayHttp {
        client: client.clone(),
        endpoint: provider_url(profile),
        profile: profile.clone(),
        model: model.clone(),
        streaming: streaming.is_some(),
        target: crate::openai::Target::Standard,
        retry_refusals,
        stream_end: Arc::default(),
        authenticated: secret.is_some(),
        budget,
        body: Some(body),
        failure: Arc::default(),
        status: Arc::default(),
        unary_body: Arc::default(),
        profile_headers: Vec::new(),
    };
    let original = transport.unary_body.clone();
    execute(transport, secret, request, streaming)
        .await
        .map(|mut response| {
            if profile.protocol == ProviderProtocol::OpenAiCompatible
                && let Some(object) = response.raw.as_object_mut()
            {
                object.insert("object".into(), "chat.completion".into());
            }
            original
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take()
                .unwrap_or_else(|| response.raw.to_string())
        })
}

async fn execute(
    mut transport: GatewayHttp,
    secret: Option<&str>,
    mut request: CompletionRequest,
    streaming: Option<(Emit<'_>, &mut PlanTiming, bool)>,
) -> Result<CompletionResponse, ProviderFailure> {
    let invalid_headers = || {
        let mut failure = stream_failure("provider transport failed");
        failure.model_error = ModelError::new(neutral::ErrorKind::InvalidRequest);
        failure.outcome_unknown = false;
        failure
    };
    let headers = provider_transform::profile_headers(&transport.profile, secret)
        .map_err(|_| invalid_headers())?;
    transport.profile_headers = headers
        .into_iter()
        .map(|header| {
            let mut value = reqwest::header::HeaderValue::from_str(header.value.as_str())
                .map_err(|_| invalid_headers())?;
            value.set_sensitive(header.secret);
            Ok((header.name, value))
        })
        .collect::<Result<_, ProviderFailure>>()?;
    let budget = transport.budget;
    // Neutral history keeps the opaque extension, not Rig's provider origin.
    // Bind only these native call items to the selected Chat codec, which
    // rebuilds ids, names and arguments while replaying the extra fields.
    if transport.profile.protocol == ProviderProtocol::OpenAiCompatible {
        for turn in &mut request.chat_history {
            if let message::Message::Assistant(turn) = turn
                && turn.content.iter().any(|block| {
                    matches!(block, message::AssistantContent::ToolCall(_))
                        && block
                            .native_item()
                            .and_then(|item| item.get("extra_content"))
                            .is_some()
                })
            {
                turn.origin = Some(message::Origin::new(
                    "openai.chat",
                    openai::PROVIDER_NAME,
                    transport.model.clone(),
                ));
            }
        }
    }
    let result = async {
        let model = transport.model.clone();
        match transport.profile.protocol {
            ProviderProtocol::OpenAiCompatible => {
                let provider = openai::OpenAIConfig::new(secret.unwrap_or_default());
                run(
                    provider.connect(transport.clone()).chat(model),
                    request,
                    streaming,
                    transport.body.is_none(),
                    &transport.stream_end,
                )
                .await
            }
            ProviderProtocol::Anthropic => {
                if secret.is_none() {
                    return Err(rig_core::error::ProviderError::request(
                        "credential required",
                    ));
                }
                let provider = anthropic::AnthropicConfig::new(secret.unwrap_or_default());
                run(
                    provider.connect(transport.clone()).completion(model),
                    request,
                    streaming,
                    transport.body.is_none(),
                    &transport.stream_end,
                )
                .await
            }
        }
    };
    let result = tokio::time::timeout(budget, result).await;
    match result {
        Ok(Ok(response)) if response.aborted.is_some() => {
            let mut failure = stream_failure(
                "provider generation was cancelled; partial output was retained; no tools were authorized",
            );
            failure.model_error = ModelError::new(neutral::ErrorKind::Cancelled);
            Err(failure)
        }
        Ok(Ok(response)) if response.error.is_some() => {
            let mut failure = stream_failure(
                "provider generation failed; partial output was retained; no tools were authorized",
            );
            failure.model_error = ModelError::new(neutral::ErrorKind::Provider);
            Err(failure)
        }
        Ok(Ok(response)) => Ok(response),
        other => Err(execution_failure(&other, &transport)),
    }
}

fn execution_failure(
    other: &Result<Result<CompletionResponse, ProviderError>, tokio::time::error::Elapsed>,
    transport: &GatewayHttp,
) -> ProviderFailure {
    let failure = transport
        .failure
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(failure) = failure {
        return failure;
    }
    let mut failure = stream_failure(match other {
        Err(_) => "provider request timed out; partial output was retained; try a shorter request",
        Ok(Err(ProviderError::Truncated)) => {
            "provider stream was truncated; partial output was retained; no tools were authorized"
        }
        Ok(Err(ProviderError::Http(_))) => {
            "provider network connection failed; check connectivity; partial output was retained"
        }
        _ => {
            "provider response was malformed or unsupported; partial output was retained; no tools were authorized"
        }
    });
    failure.model_error = ModelError::new(match other {
        Err(_) => neutral::ErrorKind::TimedOut,
        Ok(Err(ProviderError::Truncated)) => neutral::ErrorKind::Truncated,
        Ok(Err(ProviderError::Http(_))) => neutral::ErrorKind::Transport,
        Ok(Err(ProviderError::Request(_))) => neutral::ErrorKind::InvalidRequest,
        _ => neutral::ErrorKind::InvalidResponse,
    });
    if matches!(other, Ok(Err(rig_core::error::ProviderError::Request(_)))) {
        failure.outcome_unknown = false;
        failure.why = "provider transport failed".into();
    }
    if let Ok(Err(error)) = other
        && let Some(body) = error.provider_response_body()
    {
        let classified = crate::openai::Failure::from_http(
            error
                .provider_response_status()
                .map_or(0, |status| status.as_u16()),
            body,
            None,
        );
        failure.model_error = classified.model_error();
        failure.why = format!(
            "provider stream failed{}",
            classified_error_advice(&classified)
        );
    }
    failure.status = *transport
        .status
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    failure
}

use agent_provider_config::protocol::{self as neutral, ModelError};
use rig_core::message;

/// Host custody uses the same neutral codec and provider execution as Complete.
pub(super) async fn host_complete(
    client: &reqwest::Client,
    profile: &ProviderProfile,
    secret: Option<&str>,
    model: &str,
    request: CompletionRequest,
    budget: Duration,
    target: crate::openai::Target,
) -> Result<CompletionResponse, ProviderFailure> {
    execute(
        GatewayHttp {
            client: client.clone(),
            endpoint: provider_url(profile),
            profile: profile.clone(),
            model: model.into(),
            streaming: false,
            target,
            retry_refusals: false,
            stream_end: Arc::default(),
            authenticated: secret.is_some(),
            budget,
            body: None,
            failure: Arc::default(),
            status: Arc::default(),
            unary_body: Arc::default(),
            profile_headers: Vec::new(),
        },
        secret,
        request,
        None,
    )
    .await
}

pub(super) fn neutral_request(
    request: neutral::ModelRequest,
) -> Result<CompletionRequest, ModelError> {
    let mut result = CompletionRequest::new("gateway request");
    result.chat_history.clear();
    result.max_tokens = Some(u64::from(request.max_output_tokens));
    result.tool_choice = Some(match request.tool_choice {
        neutral::ToolChoice::Auto => message::ToolChoice::Auto,
        neutral::ToolChoice::Required => message::ToolChoice::Required,
        neutral::ToolChoice::None => message::ToolChoice::None,
    });
    let name = |name: String| {
        message::ToolName::new(name)
            .map_err(|_| ModelError::new(neutral::ErrorKind::InvalidRequest))
    };
    let mut calls = std::collections::BTreeMap::new();
    for item in request.messages {
        let mut user = Vec::new();
        let mut assistant = Vec::new();
        let mut system = String::new();
        for content in item.content {
            match (item.role, content) {
                (neutral::Role::System, neutral::Content::Text { text }) => {
                    if !system.is_empty() {
                        system.push('\n');
                    }
                    system.push_str(&text);
                }
                (neutral::Role::User, neutral::Content::Text { text }) => {
                    user.push(message::UserContent::text(text));
                }
                (neutral::Role::Assistant, neutral::Content::Text { text }) => {
                    assistant.push(message::AssistantContent::text(text));
                }
                (neutral::Role::User, neutral::Content::Image { media_type, base64 }) => {
                    let media_type = match media_type.as_str() {
                        "image/png" => message::ImageMediaType::PNG,
                        "image/jpeg" => message::ImageMediaType::JPEG,
                        "image/gif" => message::ImageMediaType::GIF,
                        "image/webp" => message::ImageMediaType::WEBP,
                        _ => return Err(ModelError::new(neutral::ErrorKind::InvalidRequest)),
                    };
                    user.push(message::UserContent::Image(message::Image {
                        data: message::DocumentSourceKind::Base64(base64),
                        media_type: Some(media_type),
                        ..Default::default()
                    }));
                }
                (neutral::Role::Assistant, neutral::Content::Reasoning { text }) => {
                    assistant.push(message::AssistantContent::Reasoning(
                        message::Reasoning::new(text),
                    ));
                }
                (neutral::Role::Assistant, neutral::Content::ToolCall(call)) => {
                    call.validate()
                        .map_err(|_| ModelError::new(neutral::ErrorKind::InvalidRequest))?;
                    let tool_name = name(call.name)?;
                    if calls.insert(call.id.clone(), tool_name.clone()).is_some() {
                        return Err(ModelError::new(neutral::ErrorKind::InvalidRequest));
                    }
                    let mut block =
                        message::AssistantContent::tool_call(call.id, tool_name, call.input);
                    if let Some(extra) = call.extra_content {
                        block = block.with_native(serde_json::json!({"extra_content": extra}));
                    }
                    assistant.push(block);
                }
                (neutral::Role::Tool, neutral::Content::ToolResult(tool)) => {
                    let tool_name = calls
                        .remove(&tool.call_id)
                        .ok_or_else(|| ModelError::new(neutral::ErrorKind::InvalidRequest))?;
                    user.push(message::UserContent::ToolResult(message::ToolResult {
                        call: message::CallId::from_wire(tool.call_id),
                        name: tool_name,
                        content: vec![message::ToolResultContent::Text(message::Text::new(
                            tool.content,
                        ))],
                        is_error: tool.is_error,
                    }));
                }
                _ => return Err(ModelError::new(neutral::ErrorKind::InvalidRequest)),
            }
        }
        result.chat_history.push(match item.role {
            neutral::Role::System => message::Message::System { content: system },
            neutral::Role::Assistant => {
                message::Message::Assistant(message::AssistantMessage::new(assistant))
            }
            neutral::Role::User | neutral::Role::Tool => message::Message::User { content: user },
        });
    }
    for tool in request.tools {
        result.tools.push(rig_core::completion::ToolDefinition::new(
            name(tool.name)?,
            tool.description,
            tool.input_schema,
        ));
    }
    result
        .validate_message_content()
        .map_err(|_| ModelError::new(neutral::ErrorKind::InvalidRequest))?;
    Ok(result)
}

fn refused(response: &CompletionResponse) -> bool {
    response
        .raw
        .pointer("/choices/0/message/refusal")
        .is_some_and(|value| !value.is_null())
        || response
            .raw
            .pointer("/choices/0/message/content")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|parts| {
                parts.iter().any(|part| {
                    part.get("type").and_then(serde_json::Value::as_str) == Some("refusal")
                })
            })
}

pub(super) fn neutral_response(
    response: CompletionResponse,
) -> Result<neutral::ModelResponse, ModelError> {
    use rig_core::completion::FinishReason;
    let stop_reason = match response.finish_reason() {
        Some(FinishReason::Length) => neutral::StopReason::OutputLimit,
        Some(FinishReason::ContentFilter) => neutral::StopReason::Refusal,
        Some(FinishReason::ToolCalls) => neutral::StopReason::ToolCalls,
        Some(FinishReason::Stop) => neutral::StopReason::EndTurn,
        _ => {
            return Err(ModelError::new(neutral::ErrorKind::InvalidResponse));
        }
    };
    let stop_reason = if refused(&response) {
        neutral::StopReason::Refusal
    } else {
        stop_reason
    };
    if response.error.is_some() || response.aborted.is_some() {
        return Err(ModelError::new(neutral::ErrorKind::InvalidResponse));
    }
    let mut content = Vec::new();
    for block in response.choice {
        match block {
            message::AssistantContent::Text(text) => {
                content.push(neutral::Content::Text { text: text.text });
            }
            message::AssistantContent::ToolCall(call) => {
                if call.function.invalid_arguments.is_some() {
                    return Err(ModelError::new(neutral::ErrorKind::InvalidResponse));
                }
                content.push(neutral::Content::ToolCall(neutral::ToolCall {
                    id: call.id.to_string(),
                    name: call.function.name.to_string(),
                    extra_content: call
                        .native
                        .as_ref()
                        .and_then(|native| native.item.get("extra_content"))
                        .filter(|extra| extra.is_object())
                        .cloned(),
                    input: serde_json::Value::Object(call.function.arguments),
                }));
            }
            message::AssistantContent::Reasoning(reasoning) => {
                if !reasoning.text.is_empty() {
                    content.push(neutral::Content::Reasoning {
                        text: reasoning.text,
                    });
                }
            }
            _ => {
                return Err(ModelError::new(neutral::ErrorKind::InvalidResponse));
            }
        }
    }
    if content.is_empty() && stop_reason == neutral::StopReason::EndTurn {
        return Err(ModelError::new(neutral::ErrorKind::InvalidResponse));
    }
    let response = neutral::ModelResponse {
        message: neutral::Message {
            role: neutral::Role::Assistant,
            content,
        },
        stop_reason,
        usage: neutral::Usage {
            input_tokens: response.usage.input_tokens.unwrap_or_default(),
            output_tokens: response.usage.output_tokens.unwrap_or_default(),
            total_tokens: response.usage.total_tokens.unwrap_or_default(),
        },
    };
    // Refusal and truncation remain explicit, non-dispatchable responses.
    if matches!(
        stop_reason,
        neutral::StopReason::EndTurn | neutral::StopReason::ToolCalls
    ) {
        response
            .tool_calls()
            .map_err(|_| ModelError::new(neutral::ErrorKind::InvalidResponse))?;
    }
    Ok(response)
}

pub(super) async fn complete(
    client: &reqwest::Client,
    call: &PlanCall<'_>,
    request: neutral::ModelRequest,
    emit: Emit<'_>,
) -> Result<String, ModelError> {
    let mut timing = PlanTiming::start();
    timing.stream = call.stream;
    let request = neutral_request(request)?;
    let (profile_id, model_id) = (call.profile_id.to_owned(), call.model_id.to_owned());
    let (profile, mut secret) =
        blocking_model(move || plan_profile(&profile_id, &model_id)).await?;
    let budget = timing
        .budget(call, &profile, request.max_tokens)
        .map_err(|_| ModelError::new(neutral::ErrorKind::TimedOut))?;
    let transport = GatewayHttp {
        client: client.clone(),
        endpoint: provider_url(&profile),
        profile: profile.clone(),
        model: call.model_id.into(),
        streaming: call.stream,
        target: crate::openai::Target::Standard,
        authenticated: secret.is_some(),
        budget,
        body: None,
        retry_refusals: false,
        stream_end: Arc::default(),
        failure: Arc::default(),
        status: Arc::default(),
        unary_body: Arc::default(),
        profile_headers: Vec::new(),
    };
    let result = execute(
        transport,
        secret.as_ref().map(|secret| secret.as_str()),
        request,
        call.stream.then_some((emit, &mut timing, true)),
    )
    .await
    .map_err(|failure| failure.model_error)
    .and_then(|response| {
        timing.status = Some(200);
        timing.observe_body(&response.raw.to_string());
        let response = neutral_response(response)?;
        serde_json::to_string(&response)
            .map_err(|_| ModelError::new(neutral::ErrorKind::InvalidResponse))
    });
    secret.zeroize();
    timing.log(
        call,
        result.as_ref().map(|_| ()).map_err(|error| &error.message),
    );
    result
}

enum DeltaFailure {
    Truncated,
    ClientLeft,
}

impl From<DeltaFailure> for ProviderError {
    fn from(failure: DeltaFailure) -> Self {
        match failure {
            DeltaFailure::Truncated => Self::Truncated,
            DeltaFailure::ClientLeft => Self::Response("client left".into()),
        }
    }
}

fn text_delta(
    kind: &str,
    text: String,
    frames: &mut usize,
    coalescer: Option<&mut DeltaCoalescer>,
    emit: Emit<'_>,
) -> Result<(), DeltaFailure> {
    if coalescer
        .as_ref()
        .map_or(*frames, |coalescer| coalescer.frames)
        >= MAX_DELTA_EVENTS
    {
        // Presentation must not silently lose bytes while the turn is reported
        // as complete. Keep the already emitted prefix and fail closed.
        return Err(DeltaFailure::Truncated);
    }
    *frames = frames.saturating_add(1);
    if let Some(coalescer) = coalescer {
        coalescer.frames = coalescer.frames.saturating_add(1);
    }
    let event = Response {
        ok: true,
        output: Some(serde_json::Value::String(text).to_string()),
        configured: None,
        error: None,
        model_error: None,
        event: Some(kind.into()),
    };
    if emit(&event) {
        Ok(())
    } else {
        Err(DeltaFailure::ClientLeft)
    }
}

#[derive(Default)]
struct StreamProgress {
    assembly: StreamAssembly,
    parts: std::collections::BTreeMap<usize, usize>,
    coalescer: Option<DeltaCoalescer>,
    completed: std::collections::BTreeSet<usize>,
    text_frames: usize,
}

impl StreamProgress {
    fn end(
        &mut self,
        part: usize,
        call: &message::ToolCall,
        emit: Emit<'_>,
    ) -> Result<(), Box<ProviderError>> {
        let index = self
            .parts
            .get(&part)
            .copied()
            .ok_or_else(|| ProviderError::Response("tool end before start".into()))?;
        let slot = self
            .assembly
            .calls
            .get_mut(index)
            .ok_or_else(|| ProviderError::Response("invalid tool index".into()))?;
        // Rig assembles streamed name fragments into the final name.
        // Never authorize a silently renamed operation.
        if (!slot.name.is_empty() && slot.name != call.function.name.as_str())
            || !self.completed.insert(index)
        {
            return Err(Box::new(ProviderError::Response(
                "provider changed or repeated a tool".into(),
            )));
        }
        slot.id = call.id.to_string();
        slot.name = call.function.name.to_string();
        if slot.arguments.is_empty() {
            slot.arguments = serde_json::to_string(&call.function.arguments)
                .map_err(|_| ProviderError::Response("invalid tool arguments".into()))?;
        }
        // A closed block is not a successful turn. Only presentation
        // deltas escape before the authoritative provider finish.
        if let Some(coalescer) = &mut self.coalescer {
            coalescer
                .flush(&self.assembly.calls, index, Instant::now(), true, emit)
                .map_err(|_| ProviderError::Response("client left".into()))?;
        }
        Ok(())
    }

    fn event(
        &mut self,
        event: StreamEvent,
        timing: &mut PlanTiming,
        emit: Emit<'_>,
        neutral: bool,
    ) -> Result<(), Box<ProviderError>> {
        let reasoning = matches!(&event, StreamEvent::Reasoning { .. });
        match event {
            StreamEvent::Start {
                part,
                kind: PartKind::ToolCall,
                name,
            } => {
                timing
                    .first_token
                    .get_or_insert_with(|| timing.started.elapsed());
                if self.assembly.calls.len() >= MAX_STREAM_EVENTS
                    || self
                        .parts
                        .insert(part.index(), self.assembly.calls.len())
                        .is_some()
                {
                    return Err(Box::new(ProviderError::Response(
                        "invalid or excessive tool parts".into(),
                    )));
                }
                self.assembly.calls.push(StreamCall {
                    name: name.map(|name| name.to_string()).unwrap_or_default(),
                    ..Default::default()
                });
            }
            StreamEvent::Arguments { part, json } => {
                timing
                    .first_token
                    .get_or_insert_with(|| timing.started.elapsed());
                let index =
                    self.parts.get(&part.index()).copied().ok_or_else(|| {
                        ProviderError::Response("arguments before tool start".into())
                    })?;
                self.assembly
                    .calls
                    .get_mut(index)
                    .ok_or_else(|| ProviderError::Response("invalid tool index".into()))?
                    .arguments
                    .push_str(&json);
                // Rig exposes the id at End. Small calls wait for it; long
                // arguments still deliver partial presentation deltas.
                if let Some(coalescer) = &mut self.coalescer {
                    for (index, call) in self.assembly.calls.iter().enumerate() {
                        if !call.id.is_empty() || call.arguments.len() >= DELTA_FLUSH_BYTES {
                            coalescer
                                .flush(&self.assembly.calls, index, Instant::now(), false, emit)
                                .map_err(|_| ProviderError::Response("client left".into()))?;
                        }
                    }
                }
            }
            StreamEvent::End {
                part,
                content: message::AssistantContent::ToolCall(call),
            } => {
                self.end(part.index(), &call, emit)?;
            }
            StreamEvent::Text { text, .. } | StreamEvent::Reasoning { text, .. } => {
                if reasoning {
                    self.assembly.reasoning_chunks =
                        self.assembly.reasoning_chunks.saturating_add(1);
                    self.assembly.reasoning_bytes = self
                        .assembly
                        .reasoning_bytes
                        .saturating_add(text.len() as u64);
                    timing
                        .first_reasoning
                        .get_or_insert_with(|| timing.started.elapsed());
                } else {
                    timing
                        .first_token
                        .get_or_insert_with(|| timing.started.elapsed());
                }
                if neutral {
                    text_delta(
                        if reasoning {
                            "reasoning_delta"
                        } else {
                            "text_delta"
                        },
                        text,
                        &mut self.text_frames,
                        self.coalescer.as_mut(),
                        emit,
                    )
                    .map_err(ProviderError::from)?;
                }
            }
            _ => {}
        }
        timing.observe_stream(
            self.assembly.reasoning_chunks,
            self.assembly.reasoning_bytes,
        );
        Ok(())
    }
}

async fn run<W>(
    model: rig_core::Model<W>,
    request: CompletionRequest,
    streaming: Option<(Emit<'_>, &mut PlanTiming, bool)>,
    neutral: bool,
    stream_end: &StreamEnd,
) -> Result<CompletionResponse, rig_core::error::ProviderError>
where
    W: rig_core::wire::Wire<
            Op = rig_core::operation::Completion,
            Payload = rig_core::wire::Encoded,
            Frame = rig_core::wire::WireFrame,
        >,
{
    let forbidden = matches!(request.tool_choice, Some(message::ToolChoice::None));
    let tools = request
        .tools
        .iter()
        .filter(|_| !forbidden)
        .map(|tool| tool.name.to_string())
        .collect::<std::collections::BTreeSet<_>>();
    let Some((emit, timing, deltas)) = streaming else {
        let response = model.call(request).await?;
        validate_history(&response, &tools)
            .map_err(|error| ProviderError::Response(error.to_string()))?;
        if neutral && response.error.is_none() && response.aborted.is_none() {
            neutral_response(response.clone())
                .map_err(|error| ProviderError::Response(error.to_string()))?;
        }
        return Ok(response);
    };
    let mut stream = model.stream(request)?;
    let mut progress = StreamProgress {
        coalescer: deltas.then(DeltaCoalescer::default),
        ..Default::default()
    };
    while let Some(item) = stream.next().await {
        let event = match item? {
            Item::Event(event) => event,
            Item::Unknown(payload) if payload.value().get("error").is_some() => {
                return Err(ProviderError::from_provider_body(
                    payload.value().to_string(),
                ));
            }
            Item::Unknown(_) => continue,
        };
        timing.status = Some(200);
        timing
            .first_byte
            .get_or_insert_with(|| timing.started.elapsed());
        progress
            .event(event, timing, emit, neutral)
            .map_err(|error| *error)?;
    }
    let response = stream.finish().await?;
    let end = stream_end.lock().await.take();
    if let Some(mut end) = end {
        (&mut end.receiver)
            .await
            .map_err(|_| ProviderError::Truncated)?
            .map_err(|_| ProviderError::Truncated)?;
    }
    validate_history(&response, &tools)
        .map_err(|error| ProviderError::Response(error.to_string()))?;
    authorize_completed_calls(
        &response,
        &progress.assembly,
        progress.completed.len(),
        emit,
        timing,
        progress.coalescer.as_mut(),
    )
    .map_err(|error| *error)?;
    Ok(response)
}

/// Rig retains unknown fields in raw history. Validate replay-sensitive
/// fields before any completed-call event can authorize execution.
fn validate_history(
    response: &CompletionResponse,
    tools: &std::collections::BTreeSet<String>,
) -> Result<(), ModelError> {
    if response.choice.iter().any(|content| {
        matches!(content,
        message::AssistantContent::ToolCall(call) if !tools.contains(call.function.name.as_str()))
    }) {
        return Err(ModelError::invalid("provider returned an undeclared tool"));
    }
    if response
        .raw
        .pointer("/choices/0/message/reasoning_content")
        .is_some_and(|value| !value.is_null() && !value.is_string())
    {
        return Err(ModelError::invalid(
            "provider reasoning_content was not a string",
        ));
    }
    Ok(())
}

fn authorize_completed_calls(
    response: &CompletionResponse,
    assembly: &StreamAssembly,
    completed: usize,
    emit: Emit<'_>,
    timing: &mut PlanTiming,
    coalescer: Option<&mut DeltaCoalescer>,
) -> Result<(), Box<ProviderError>> {
    // Rig retains malformed argument strings rather than rejecting the turn.
    // Validate before legacy completed-call events can authorize execution.
    if response.error.is_none()
        && response.aborted.is_none()
        && !refused(response)
        && matches!(
            response.finish_reason(),
            Some(
                rig_core::completion::FinishReason::Stop
                    | rig_core::completion::FinishReason::ToolCalls
            )
        )
    {
        neutral_response(response.clone()).map_err(|_| {
            ProviderError::Response("invalid or malformed provider response".into())
        })?;
        if completed != assembly.calls.len() {
            return Err(Box::new(ProviderError::Truncated));
        }
        emit_complete(
            &assembly.calls,
            0,
            assembly.calls.len(),
            emit,
            timing,
            coalescer,
        )
        .map_err(|_| ProviderError::Response("client left".into()))?;
    }
    Ok(())
}
