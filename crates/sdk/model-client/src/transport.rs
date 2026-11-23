//! Transport-neutral asynchronous RPC to the model gateway, over the descriptor
//! its launcher hands an app. Install this transport in the host's
//! `ModelBackend`, not a process global.
#[path = "administration_client.rs"]
mod administration;
#[cfg(test)]
#[path = "client_fixture_tests.rs"]
mod fixtures;
#[cfg(test)]
#[path = "transport_tests.rs"]
mod tests;
use crate::{Request, Response};
pub use administration::AdministrationClient;
use agent_provider_config::protocol::{ErrorKind, ModelError, ModelResponse, StreamEvent};
use std::{future::Future, pin::Pin, time::Duration};

pub type CallFuture<'a> = Pin<Box<dyn Future<Output = Result<Response, ModelError>> + Send + 'a>>;
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

/// Decode the gateway's public event wire once for synchronous and async clients.
/// Completed-call compatibility events are inert: only the final turn can dispatch.
pub fn decode_event(kind: &str, payload: &str) -> Result<Option<StreamEvent>, ModelError> {
    let event = match kind {
        crate::EVENT_TOOL_CALL => return Ok(None),
        "text_delta" | "reasoning_delta" => {
            let text = serde_json::from_str(payload)
                .map_err(|_| ModelError::invalid("invalid gateway text delta"))?;
            if kind == "text_delta" {
                StreamEvent::TextDelta { text }
            } else {
                StreamEvent::ReasoningDelta { text }
            }
        }
        crate::EVENT_TOOL_CALL_DELTA => {
            #[derive(serde::Deserialize)]
            struct Fragment {
                index: u32,
                id: Option<String>,
                name: Option<String>,
                delta: String,
            }
            let delta: Fragment = serde_json::from_str(payload)
                .map_err(|_| ModelError::invalid("invalid gateway tool delta"))?;
            StreamEvent::ToolCallDelta {
                index: delta.index,
                id: delta.id,
                name: delta.name,
                arguments: delta.delta,
            }
        }
        _ => return Err(ModelError::invalid("unknown gateway stream event")),
    };
    Ok(Some(event))
}

pub(crate) fn decode_completion(output: &str) -> Result<ModelResponse, ModelError> {
    let response: ModelResponse = serde_json::from_str(output)
        .map_err(|_| ModelError::invalid("invalid gateway completion"))?;
    // Refusal and output limits are valid terminal responses. StreamState keeps
    // their prose, but never exposes their calls. Malformed finals fail here.
    match response.tool_calls() {
        Err(error) if !matches!(error.kind, ErrorKind::Refused | ErrorKind::Truncated) => {
            Err(error)
        }
        _ => Ok(response),
    }
}

fn completion(response: &Response) -> Result<ModelResponse, ModelError> {
    if !response.ok {
        return Err(response.failure_error().model_error());
    }
    if response.event.is_some() {
        return Err(ModelError::invalid("missing gateway final response"));
    }
    decode_completion(response.output.as_deref().unwrap_or_default())
}

pub(crate) fn finish_stream(
    result: Result<ModelResponse, ModelError>,
    event: &mut dyn FnMut(&StreamEvent),
) -> Result<ModelResponse, ModelError> {
    match &result {
        Ok(response) => event(&StreamEvent::Finished {
            response: response.clone(),
        }),
        Err(error) => event(&StreamEvent::Failed {
            error: error.clone(),
        }),
    }
    result
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
    use_default: bool,
}
impl<T: Transport> ModelClient<T> {
    /// A Send-safe single completion over this client's selected route.
    pub async fn complete_once(
        &self,
        request: &agent_provider_config::protocol::ModelRequest,
    ) -> Result<ModelResponse, ModelError> {
        let response = self
            .transport
            .call(self.request(request, false), &mut |_| true)
            .await?;
        completion(&response)
    }

    /// Resolve the gateway's current deliberate default at each inference start.
    pub fn default_model(transport: T, timeout: Duration) -> Self {
        Self {
            transport,
            profile_id: String::new(),
            model_id: String::new(),
            timeout_ms: timeout.as_millis().min(120_000) as u64,
            use_default: true,
        }
    }

    fn request(
        &self,
        request: &agent_provider_config::protocol::ModelRequest,
        stream: bool,
    ) -> Request {
        if self.use_default {
            Request::CompleteDefault {
                request: request.clone(),
                timeout_ms: self.timeout_ms,
                stream,
            }
        } else {
            Request::Complete {
                profile_id: self.profile_id.clone(),
                model_id: self.model_id.clone(),
                request: request.clone(),
                timeout_ms: self.timeout_ms,
                stream,
            }
        }
    }
    /// Presentation deltas survive terminal failure in the caller's `StreamState`.
    /// Returning false cancels the request; partial tools never authorize actions.
    pub async fn complete_streaming(
        &self,
        request: &agent_provider_config::protocol::ModelRequest,
        event: &mut (dyn FnMut(&agent_provider_config::protocol::StreamEvent) -> bool + Send),
    ) -> Result<
        agent_provider_config::protocol::ModelResponse,
        agent_provider_config::protocol::ModelError,
    > {
        let mut invalid = false;
        let response = self
            .transport
            .call(self.request(request, true), &mut |item| {
                if invalid {
                    return false;
                }
                if !item.ok {
                    invalid = true;
                    return false;
                }
                match decode_event(
                    item.event.as_deref().unwrap_or_default(),
                    item.output.as_deref().unwrap_or_default(),
                ) {
                    Ok(Some(parsed)) => event(&parsed),
                    Ok(None) => true,
                    Err(_) => {
                        invalid = true;
                        false
                    }
                }
            })
            .await;
        let result = match response {
            _ if invalid => Err(ModelError::invalid("invalid gateway stream event")),
            Err(error) => Err(error),
            Ok(response) => completion(&response),
        };
        finish_stream(result, &mut |item| {
            event(item);
        })
    }

    pub fn new(transport: T, profile_id: String, model_id: String, timeout: Duration) -> Self {
        Self {
            transport,
            profile_id,
            model_id,
            timeout_ms: timeout.as_millis().min(120_000) as u64,
            use_default: false,
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
            finish_stream(result, &mut |event| sink(event.clone()))
        })
    }

    fn complete<'a>(
        &'a self,
        request: &'a agent_provider_config::protocol::ModelRequest,
    ) -> agent_provider_config::protocol::CompletionFuture<'a> {
        Box::pin(self.complete_once(request))
    }
}
