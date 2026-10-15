//! Transport-neutral asynchronous RPC and the browser/native fetch adapter.
//! Install this transport in the host's `ModelBackend`, not a process global.
//! Authentication is a host session token, never a provider API key or app ID.
#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
use crate::{Request, Response};
use std::{future::Future, pin::Pin, time::Duration};

pub type CallFuture<'a> = Pin<Box<dyn Future<Output = Result<Response, String>> + Send + 'a>>;
pub trait Transport: Send + Sync {
    /// Intermediate events go to `event`; false cancels the operation. Dropping
    /// this future cancels its response. Transports synchronize cancellation or
    /// invalidate the connection so unread frames cannot contaminate another call.
    fn call<'a>(
        &'a self,
        request: Request,
        event: &'a mut (dyn FnMut(Response) -> bool + Send),
    ) -> CallFuture<'a>;
}

#[cfg(unix)]
#[path = "transport_fd.rs"]
mod fd;
#[cfg(unix)]
pub use fd::FdTransport;

/// A same-origin host should issue a short-lived, audience-bound session token.
/// This client has no provider endpoint override or provider-key read method.
pub struct FetchTransport {
    client: tilo_net::Client,
    endpoint: String,
    session_token: zeroize::Zeroizing<String>,
}
impl FetchTransport {
    /// # Errors
    /// Rejects unsafe URLs or an unavailable network client.
    pub fn new(endpoint: &str, session_token: String) -> Result<Self, String> {
        let url = tilo_net::Url::parse(endpoint).map_err(|_| "invalid model service URL")?;
        if url.scheme() != "https"
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/model/rpc"
        {
            return Err(
                "model service requires an HTTPS /model/rpc URL without credentials".into(),
            );
        }
        let client = tilo_net::Client::builder()
            .redirects(tilo_net::Redirects::None)
            .timeout(Duration::from_secs(125))
            .build()
            .map_err(|_| "model transport unavailable")?;
        Ok(Self {
            client,
            endpoint: endpoint.into(),
            session_token: zeroize::Zeroizing::new(session_token),
        })
    }
}
impl Transport for FetchTransport {
    fn call<'a>(
        &'a self,
        request: Request,
        event: &'a mut (dyn FnMut(Response) -> bool + Send),
    ) -> CallFuture<'a> {
        Box::pin(async move {
            let body = serde_json::to_vec(&crate::Frame {
                version: crate::RPC_VERSION,
                value: request,
            })
            .map_err(|_| "cannot encode model request")?;
            if body.len() > 2 * 1024 * 1024 {
                return Err("model request too large".into());
            }
            let mut response = self
                .client
                .post(&self.endpoint)
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {}", *self.session_token))
                .body(body)
                .send()
                .await
                .map_err(|_| "model service unavailable")?;
            if !response.status().is_success() {
                return Err("model service rejected request".into());
            }
            let mut pending = Vec::new();
            let mut events = 0usize;
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| "model stream interrupted")?
            {
                // Do not append a whole network chunk: validate one byte/line
                // at a time so a peer cannot defeat the allocation bound.
                for byte in chunk {
                    if byte == b'\n' {
                        let item: Response = serde_json::from_slice(&pending)
                            .map_err(|_| "invalid model response")?;
                        pending.clear();
                        if item.event.is_none() {
                            return Ok(item);
                        }
                        events = events.saturating_add(1);
                        if events > crate::MAX_STREAM_EVENTS + crate::MAX_DELTA_EVENTS {
                            return Err("too many model events".into());
                        }
                        if !event(item) {
                            return Err("model request cancelled".into());
                        }
                    } else {
                        if pending.len() >= 2 * 1024 * 1024 {
                            return Err("model response too large".into());
                        }
                        pending.push(byte);
                    }
                }
            }
            Err("model stream ended without a final response".into())
        })
    }
}

fn transport_error(detail: String) -> agent_provider_config::protocol::ModelError {
    if detail.contains("unavailable") {
        agent_provider_config::protocol::ModelError {
            kind: agent_provider_config::protocol::ErrorKind::Unavailable,
            message: detail,
        }
    } else {
        crate::ClientError::Rejected(detail).model_error()
    }
}

/// A selected-model backend suitable for host-scoped installation. Selection
/// travels as public profile/model identifiers; provider URLs and keys do not.
///
/// `FdTransport` negotiates cancellation before starting work and preserves its
/// launcher connection after a synchronized Stop. Older gateways fail closed;
/// corrupt or unresponsive connections require relaunch, never a socket fallback.
pub struct ModelClient<T> {
    transport: T,
    profile_id: String,
    model_id: String,
    timeout_ms: u64,
}
impl<T: Transport> ModelClient<T> {
    /// Presentation deltas survive terminal failure in the caller's `StreamState`.
    /// Returning false cancels the request; partial tools never authorize actions.
    ///
    /// # Errors
    /// Returns transport, cancellation, provider or malformed-response errors.
    pub async fn complete_streaming(
        &self,
        request: &agent_provider_config::protocol::ModelRequest,
        event: &mut (dyn FnMut(&agent_provider_config::protocol::StreamEvent) -> bool + Send),
    ) -> Result<
        agent_provider_config::protocol::ModelResponse,
        agent_provider_config::protocol::ModelError,
    > {
        use agent_provider_config::protocol::{ModelError, StreamEvent};
        let mut invalid = false;
        let response = self
            .transport
            .call(
                Request::Complete {
                    profile_id: self.profile_id.clone(),
                    model_id: self.model_id.clone(),
                    request: request.clone(),
                    timeout_ms: self.timeout_ms,
                    stream: true,
                },
                &mut |item| {
                    let payload = item.output.as_deref().unwrap_or_default();
                    let parsed = match item.event.as_deref() {
                        Some("text_delta" | "reasoning_delta") => {
                            serde_json::from_str::<String>(payload).ok().map(|text| {
                                if item.event.as_deref() == Some("reasoning_delta") {
                                    StreamEvent::ReasoningDelta { text }
                                } else {
                                    StreamEvent::TextDelta { text }
                                }
                            })
                        }
                        Some(crate::EVENT_TOOL_CALL_DELTA) => {
                            #[derive(serde::Deserialize)]
                            struct ToolFragment {
                                index: u32,
                                id: Option<String>,
                                name: Option<String>,
                                delta: String,
                            }
                            serde_json::from_str::<ToolFragment>(payload)
                                .ok()
                                .map(|delta| StreamEvent::ToolCallDelta {
                                    index: delta.index,
                                    id: delta.id,
                                    name: delta.name,
                                    arguments: delta.delta,
                                })
                        }
                        Some(crate::EVENT_TOOL_CALL) => return true,
                        _ => None,
                    };
                    if let Some(parsed) = parsed {
                        event(&parsed)
                    } else {
                        invalid = true;
                        false
                    }
                },
            )
            .await;
        let result = match response {
            _ if invalid => Err(ModelError::invalid("invalid gateway stream event")),
            Err(error) => Err(transport_error(error)),
            Ok(response) if !response.ok => Err(crate::ClientError::Rejected(
                response
                    .error
                    .unwrap_or_else(|| "provider request failed".into()),
            )
            .model_error()),
            Ok(response) => serde_json::from_str::<agent_provider_config::protocol::ModelResponse>(
                response.output.as_deref().unwrap_or_default(),
            )
            .map_err(|_| ModelError::invalid("invalid gateway completion")),
        };
        match &result {
            Ok(response) => {
                event(&StreamEvent::Finished {
                    response: response.clone(),
                });
            }
            Err(error) => {
                event(&StreamEvent::Failed {
                    error: error.clone(),
                });
            }
        }
        result
    }

    pub fn new(transport: T, profile_id: String, model_id: String, timeout: Duration) -> Self {
        Self {
            transport,
            profile_id,
            model_id,
            timeout_ms: timeout.as_millis().min(120_000) as u64,
        }
    }
}
impl<T: Transport> agent_provider_config::protocol::ModelBackend for ModelClient<T> {
    fn stream<'a>(
        &'a self,
        request: &'a agent_provider_config::protocol::ModelRequest,
        mut sink: agent_provider_config::protocol::StreamSink<'a>,
    ) -> agent_provider_config::protocol::CompletionFuture<'a> {
        use agent_provider_config::protocol::{ErrorKind, ModelError, StreamEvent};
        Box::pin(async move {
            // The transport callback must be Send, but the host sink may own Rc
            // or UI state. Poll both halves here, without spawning or marking
            // the local sink Send. Bound a synchronous burst by the protocol's
            // event budget; a misbehaving transport fails rather than losing a
            // delta or blocking this same-thread consumer on a full queue.
            let (sender, receiver) =
                std::sync::mpsc::sync_channel(crate::MAX_STREAM_EVENTS + crate::MAX_DELTA_EVENTS);
            let mut overflow = false;
            let mut forward = |event: &StreamEvent| {
                // complete_streaming also reports its result as an event. Emit
                // the terminal locally, after all queued presentation deltas.
                if matches!(
                    event,
                    StreamEvent::Finished { .. } | StreamEvent::Failed { .. }
                ) {
                    return true;
                }
                if sender.try_send(event.clone()).is_err() {
                    overflow = true;
                    return false;
                }
                true
            };
            let result = {
                let mut operation = Box::pin(self.complete_streaming(request, &mut forward));
                std::future::poll_fn(|cx| {
                    let result = operation.as_mut().poll(cx);
                    while let Ok(event) = receiver.try_recv() {
                        sink(event);
                    }
                    result
                })
                .await
            };
            let result = if overflow {
                Err(ModelError {
                    kind: ErrorKind::Transport,
                    message: "model stream exceeded the presentation queue capacity".into(),
                })
            } else {
                result
            };
            match &result {
                Ok(response) => sink(StreamEvent::Finished {
                    response: response.clone(),
                }),
                Err(error) => sink(StreamEvent::Failed {
                    error: error.clone(),
                }),
            }
            result
        })
    }

    fn complete<'a>(
        &'a self,
        request: &'a agent_provider_config::protocol::ModelRequest,
    ) -> agent_provider_config::protocol::CompletionFuture<'a> {
        use agent_provider_config::protocol::{ErrorKind, ModelError};
        Box::pin(async move {
            let error = |kind, message: &str| ModelError {
                kind,
                message: message.into(),
            };
            let response = self
                .transport
                .call(
                    Request::Complete {
                        profile_id: self.profile_id.clone(),
                        model_id: self.model_id.clone(),
                        request: request.clone(),
                        timeout_ms: self.timeout_ms,
                        stream: false,
                    },
                    &mut |_| true,
                )
                .await
                .map_err(transport_error)?;
            if !response.ok {
                return Err(crate::ClientError::Rejected(
                    response
                        .error
                        .unwrap_or_else(|| "provider request failed".into()),
                )
                .model_error());
            }
            serde_json::from_str(
                response
                    .output
                    .as_deref()
                    .ok_or_else(|| error(ErrorKind::InvalidResponse, "missing model response"))?,
            )
            .map_err(|_| error(ErrorKind::InvalidResponse, "invalid model response"))
        })
    }
}
