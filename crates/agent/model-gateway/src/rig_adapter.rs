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
#[path = "rig_stream_tests.rs"]
mod stream_tests;
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
    retry_refusals: bool,
    stream_end: StreamEnd,
    authenticated: bool,
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
        let body = self
            .body
            .clone()
            .map_or_else(|| serde_json::from_slice(&bytes), Ok)
            .map_err(|_| {
                http::Error::instance(std::io::Error::other("invalid request encoding"))
            })?;
        let body = outbound_body(&self.profile, &self.model, body, self.streaming)
            .map_err(|_| http::Error::instance(std::io::Error::other("invalid request body")))?;
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
            let again = again_after(
                status.as_u16(),
                response
                    .headers()
                    .get(reqwest::header::RETRY_AFTER)
                    .and_then(|value| value.to_str().ok()),
            );
            let retry_after = match again {
                Again::Soon(Some(wait)) if !wait.is_zero() && wait <= Duration::from_secs(3600) => {
                    format!(" retry_after_s={}", wait.as_secs_f32().ceil())
                }
                _ => String::new(),
            };
            let detail = error_body(response).await;
            let unprocessed = refused_unprocessed(status.as_u16(), &detail);
            return Err(ProviderFailure {
                why: format!(
                    "provider returned HTTP {}{}{retry_after}",
                    status.as_u16(),
                    provider_error_advice(status.as_u16(), &detail)
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
                // Historical unary endpoints omitted terminal metadata. Only
                // the legacy RPC accepts that shorthand: neutral requests and
                // streams still require an authoritative provider finish.
                // Keep the original bytes above for the legacy caller.
                if this.body.is_some() {
                    body = legacy_unary_body(this.profile.protocol, body)
                        .map_err(http::Error::instance)?;
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
        tail.push(&chunk);
        // A finished Rig decoder drops its receiver; the bounded remainder
        // still has to pass framing checks. StreamDrain aborts on cancellation.
        let _ = send.send(chunk).await;
    }
    if tail.incomplete()
        || (protocol == ProviderProtocol::Anthropic && tail.lifecycle != MessageLifecycle::Stopped)
    {
        return Err("provider stream was truncated or had invalid envelope ordering".into());
    }
    Ok(())
}

fn legacy_unary_body(protocol: ProviderProtocol, body: Vec<u8>) -> serde_json::Result<Vec<u8>> {
    let Ok(serde_json::Value::Object(mut object)) = serde_json::from_slice(&body) else {
        return Ok(body);
    };
    match protocol {
        ProviderProtocol::OpenAiCompatible => {
            if let Some(choices) = object
                .get_mut("choices")
                .and_then(serde_json::Value::as_array_mut)
            {
                for choice in choices {
                    let Some(choice) = choice.as_object_mut() else {
                        continue;
                    };
                    let Some(message) =
                        choice.get("message").and_then(serde_json::Value::as_object)
                    else {
                        continue;
                    };
                    let reason = if message
                        .get("tool_calls")
                        .and_then(serde_json::Value::as_array)
                        .is_some_and(|calls| !calls.is_empty())
                    {
                        "tool_calls"
                    } else {
                        "stop"
                    };
                    choice
                        .entry("finish_reason")
                        .or_insert_with(|| reason.into());
                }
            }
        }
        ProviderProtocol::Anthropic
            if object
                .get("content")
                .is_some_and(serde_json::Value::is_array) =>
        {
            object.entry("type").or_insert_with(|| "message".into());
            let reason = if object
                .get("content")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|blocks| blocks.iter().any(|block| block["type"] == "tool_use"))
            {
                "tool_use"
            } else {
                "end_turn"
            };
            object.entry("stop_reason").or_insert_with(|| reason.into());
        }
        ProviderProtocol::Anthropic => {}
    }
    serde_json::to_vec(&object)
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
        retry_refusals,
        stream_end: Arc::default(),
        authenticated: secret.is_some(),
        budget,
        body: Some(body),
        failure: Arc::default(),
        status: Arc::default(),
        unary_body: Arc::default(),
    };
    let original = transport.unary_body.clone();
    execute(transport, profile, secret, model, request, streaming)
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
    transport: GatewayHttp,
    profile: &ProviderProfile,
    secret: Option<&str>,
    model: String,
    request: CompletionRequest,
    streaming: Option<(Emit<'_>, &mut PlanTiming, bool)>,
) -> Result<CompletionResponse, ProviderFailure> {
    let budget = transport.budget;
    let result = async {
        match profile.protocol {
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
        Ok(Ok(response)) if response.aborted.is_some() => Err(stream_failure(
            "provider generation was cancelled; partial output was retained; no tools were authorized",
        )),
        Ok(Ok(response)) if response.error.is_some() => Err(stream_failure(
            "provider generation failed; partial output was retained; no tools were authorized",
        )),
        Ok(Ok(response)) => Ok(response),
        other => {
            let failure = transport
                .failure
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            if let Some(failure) = failure {
                return Err(failure);
            }
            let mut failure = stream_failure(match &other {
                Err(_) => {
                    "provider request timed out; partial output was retained; try a shorter request"
                }
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
            if matches!(other, Ok(Err(rig_core::error::ProviderError::Request(_)))) {
                failure.outcome_unknown = false;
                failure.why = "provider transport failed".into();
            }
            if let Ok(Err(error)) = &other
                && let Some(body) = error.provider_response_body()
            {
                failure.why = format!(
                    "provider stream failed{}",
                    provider_error_advice(0, body.as_bytes())
                );
            }
            failure.status = *transport
                .status
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Err(failure)
        }
    }
}

use agent_provider_config::protocol as neutral;
use rig_core::message;

fn neutral_request(request: neutral::ModelRequest) -> Result<CompletionRequest, String> {
    let mut result = CompletionRequest::new("gateway request");
    result.chat_history.clear();
    result.max_tokens = Some(u64::from(request.max_output_tokens));
    result.tool_choice = Some(match request.tool_choice {
        neutral::ToolChoice::Auto => message::ToolChoice::Auto,
        neutral::ToolChoice::Required => message::ToolChoice::Required,
        neutral::ToolChoice::None => message::ToolChoice::None,
    });
    let name =
        |name: String| message::ToolName::new(name).map_err(|_| "empty tool name".to_string());
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
                        _ => return Err("unsupported image media type".into()),
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
                        .map_err(|_| "invalid tool call".to_string())?;
                    let tool_name = name(call.name)?;
                    if calls.insert(call.id.clone(), tool_name.clone()).is_some() {
                        return Err("duplicate tool call id".into());
                    }
                    assistant.push(message::AssistantContent::tool_call(
                        call.id, tool_name, call.input,
                    ));
                }
                (neutral::Role::Tool, neutral::Content::ToolResult(tool)) => {
                    let tool_name = calls
                        .remove(&tool.call_id)
                        .ok_or_else(|| "tool result has no matching call".to_string())?;
                    user.push(message::UserContent::ToolResult(message::ToolResult {
                        call: message::CallId::from_wire(tool.call_id),
                        name: tool_name,
                        content: vec![message::ToolResultContent::Text(message::Text::new(
                            tool.content,
                        ))],
                        is_error: tool.is_error,
                    }));
                }
                _ => return Err("content is not valid for its message role".into()),
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
        .map_err(|_| "invalid model conversation".to_string())?;
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

fn neutral_response(response: CompletionResponse) -> Result<neutral::ModelResponse, String> {
    use rig_core::completion::FinishReason;
    let stop_reason = if refused(&response) {
        neutral::StopReason::Refusal
    } else {
        match response.finish_reason() {
            Some(FinishReason::Length) => neutral::StopReason::OutputLimit,
            Some(FinishReason::ContentFilter) => neutral::StopReason::Refusal,
            Some(FinishReason::ToolCalls) => neutral::StopReason::ToolCalls,
            Some(FinishReason::Stop) => neutral::StopReason::EndTurn,
            _ => return Err("provider returned an unsupported finish reason".into()),
        }
    };
    if response.error.is_some() || response.aborted.is_some() {
        return Err("provider did not finish the turn".into());
    }
    let mut content = Vec::new();
    for block in response.choice {
        match block {
            message::AssistantContent::Text(text) => {
                content.push(neutral::Content::Text { text: text.text });
            }
            message::AssistantContent::ToolCall(call) => {
                if call.function.invalid_arguments.is_some() {
                    return Err("provider returned malformed tool arguments".into());
                }
                content.push(neutral::Content::ToolCall(neutral::ToolCall {
                    id: call.id.to_string(),
                    name: call.function.name.to_string(),
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
            _ => return Err("provider returned unsupported assistant content".into()),
        }
    }
    if content.is_empty() && stop_reason == neutral::StopReason::EndTurn {
        return Err("provider returned an empty assistant response".into());
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
            .map_err(|_| "provider returned invalid tool calls".to_string())?;
    }
    Ok(response)
}

pub(super) async fn complete(
    client: &reqwest::Client,
    call: &PlanCall<'_>,
    request: neutral::ModelRequest,
    emit: Emit<'_>,
) -> Result<String, String> {
    let mut timing = PlanTiming::start();
    timing.stream = call.stream;
    let request = neutral_request(request)?;
    let (profile_id, model_id) = (call.profile_id.to_owned(), call.model_id.to_owned());
    let (profile, mut secret) = blocking(move || plan_profile(&profile_id, &model_id)).await?;
    let ceiling = profile
        .models
        .iter()
        .find(|model| model.id == call.model_id)
        .and_then(|model| model.request_limits.as_ref())
        .and_then(|limits| limits.plan_timeout_ms)
        .map_or(MAX_PROVIDER_TIMEOUT, |milliseconds| {
            Duration::from_millis(u64::from(milliseconds))
        });
    let timeout = call.timeout.min(ceiling);
    timing.timeout_ms = Some(timeout.as_millis());
    if let Some(model) = profile
        .models
        .iter()
        .find(|model| model.id == call.model_id)
        && let Some(limits) = &model.request_limits
    {
        timing.output_allowance = Some(
            request
                .max_tokens
                .unwrap_or_else(|| u64::from(limits.max_output_tokens))
                .min(u64::from(limits.max_output_tokens)),
        );
        timing.runtime_context = limits.runtime_context_tokens;
        timing.source_context = model.context_window;
    }
    let budget = timeout
        .checked_sub(timing.started.elapsed())
        .filter(|budget| !budget.is_zero())
        .ok_or_else(|| "provider request timed out".to_string())?;
    let transport = GatewayHttp {
        client: client.clone(),
        endpoint: provider_url(&profile),
        profile: profile.clone(),
        model: call.model_id.into(),
        streaming: call.stream,
        authenticated: secret.is_some(),
        budget,
        body: None,
        retry_refusals: false,
        stream_end: Arc::default(),
        failure: Arc::default(),
        status: Arc::default(),
        unary_body: Arc::default(),
    };
    let result = execute(
        transport,
        &profile,
        secret.as_ref().map(|secret| secret.as_str()),
        call.model_id.into(),
        request,
        call.stream.then_some((emit, &mut timing, true)),
    )
    .await
    .map_err(|failure| {
        if failure.outcome_unknown {
            format!(
                "provider outcome is unknown; request was not retried ({})",
                failure.why
            )
        } else {
            failure.why
        }
    })
    .and_then(|response| {
        timing.status = Some(200);
        timing.observe_body(&response.raw.to_string());
        let response = neutral_response(response)?;
        serde_json::to_string(&response).map_err(|_| "invalid model response".to_string())
    });
    secret.zeroize();
    timing.log(call, result.as_ref().map(|_| ()));
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
        event: Some(kind.into()),
    };
    if emit(&event) {
        Ok(())
    } else {
        Err(DeltaFailure::ClientLeft)
    }
}

fn finalize_call(assembly: &mut StreamAssembly, index: usize, call: &message::ToolCall) {
    if let Some(slot) = assembly.calls.get_mut(index) {
        slot.id = call.id.to_string();
        slot.name = call.function.name.to_string();
        if slot.arguments.is_empty() {
            slot.arguments = serde_json::to_string(&call.function.arguments).unwrap_or_default();
        }
    }
}

fn append_arguments(
    assembly: &mut StreamAssembly,
    parts: &std::collections::BTreeMap<usize, usize>,
    part: usize,
    json: &str,
    coalescer: Option<&mut DeltaCoalescer>,
    emit: Emit<'_>,
) -> Result<(), ProviderFailure> {
    if let Some(call) = parts
        .get(&part)
        .and_then(|index| assembly.calls.get_mut(*index))
    {
        call.arguments.push_str(json);
    }
    // Rig exposes a call's id at End. Small calls wait for it;
    // long arguments still deliver partial deltas (with no id).
    if let Some(coalescer) = coalescer {
        for (index, call) in assembly.calls.iter().enumerate() {
            if !call.id.is_empty() || call.arguments.len() >= DELTA_FLUSH_BYTES {
                coalescer.flush(&assembly.calls, index, Instant::now(), false, emit)?;
            }
        }
    }
    Ok(())
}

fn start_call(
    assembly: &mut StreamAssembly,
    parts: &mut std::collections::BTreeMap<usize, usize>,
    part: usize,
    name: String,
) -> Result<(), &'static str> {
    if assembly.calls.len() >= MAX_STREAM_EVENTS {
        return Err("too many tool calls");
    }
    parts.insert(part, assembly.calls.len());
    assembly.calls.push(StreamCall {
        name,
        ..Default::default()
    });
    Ok(())
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
    fn event(
        &mut self,
        event: StreamEvent,
        timing: &mut PlanTiming,
        emit: Emit<'_>,
        neutral: bool,
    ) -> Result<(), Box<ProviderError>> {
        match event {
            StreamEvent::Start {
                part,
                kind: PartKind::ToolCall,
                name,
            } => {
                timing
                    .first_token
                    .get_or_insert_with(|| timing.started.elapsed());
                start_call(
                    &mut self.assembly,
                    &mut self.parts,
                    part.index(),
                    name.map(|name| name.to_string()).unwrap_or_default(),
                )
                .map_err(|error| ProviderError::Response(error.into()))?;
            }
            StreamEvent::Arguments { part, json } => {
                timing
                    .first_token
                    .get_or_insert_with(|| timing.started.elapsed());
                append_arguments(
                    &mut self.assembly,
                    &self.parts,
                    part.index(),
                    &json,
                    self.coalescer.as_mut(),
                    emit,
                )
                .map_err(|_| ProviderError::Response("client left".into()))?;
            }
            StreamEvent::End {
                part,
                content: rig_core::completion::AssistantContent::ToolCall(call),
            } => {
                if let Some(index) = self.parts.get(&part.index()).copied() {
                    finalize_call(&mut self.assembly, index, &call);
                    self.completed.insert(index);
                    // A closed block is not a successful turn. Only deltas
                    // may escape before the authoritative provider finish.
                    if let Some(coalescer) = &mut self.coalescer {
                        coalescer
                            .flush(&self.assembly.calls, index, Instant::now(), true, emit)
                            .map_err(|_| ProviderError::Response("client left".into()))?;
                    }
                }
            }
            StreamEvent::Text { text, .. } => {
                timing
                    .first_token
                    .get_or_insert_with(|| timing.started.elapsed());
                if neutral {
                    text_delta(
                        "text_delta",
                        text,
                        &mut self.text_frames,
                        self.coalescer.as_mut(),
                        emit,
                    )
                    .map_err(ProviderError::from)?;
                }
            }
            StreamEvent::Reasoning { text, .. } => {
                self.assembly.reasoning_chunks = self.assembly.reasoning_chunks.saturating_add(1);
                self.assembly.reasoning_bytes = self
                    .assembly
                    .reasoning_bytes
                    .saturating_add(text.len() as u64);
                timing
                    .first_reasoning
                    .get_or_insert_with(|| timing.started.elapsed());
                if neutral {
                    text_delta(
                        "reasoning_delta",
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
    let Some((emit, timing, deltas)) = streaming else {
        return model.call(request).await;
    };
    let mut stream = model.stream(request)?;
    let mut progress = StreamProgress {
        coalescer: deltas.then(DeltaCoalescer::default),
        ..Default::default()
    };
    while let Some(item) = stream.next().await {
        let Item::Event(event) = item? else { continue };
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
        let mut ids = std::collections::BTreeSet::new();
        for content in &response.choice {
            if let message::AssistantContent::ToolCall(call) = content {
                let checked = neutral::ToolCall {
                    id: call.id.to_string(),
                    name: call.function.name.to_string(),
                    input: serde_json::Value::Object(call.function.arguments.clone()),
                };
                if call.function.invalid_arguments.is_some()
                    || checked.validate().is_err()
                    || !ids.insert(checked.id)
                {
                    return Err(Box::new(ProviderError::Response(
                        "invalid or malformed tool calls".into(),
                    )));
                }
            }
        }
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
