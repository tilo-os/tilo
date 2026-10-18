//! Per-task model access over the authenticated collaboration connection.
use agent_provider_config::protocol::{ModelRequest, ModelResponse, StreamEvent};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use ui::lock::MutexRecover as _;
use ui::time::Duration;

#[cfg(test)]
mod tests;

type Rpc = dyn Fn(u64, String, Value, Duration) -> Result<Value, String> + Send + Sync;

/// A cloneable task-bound sender, independent of the session's event receiver.
#[derive(Clone)]
pub struct Scope {
    lease: Arc<Mutex<String>>,
    run_id: u64,
    send: Arc<Rpc>,
}

/// Native inference uses the same task-bound capability in every turn thread.
pub type NativeTaskModel = Scope;

impl Scope {
    pub(crate) fn new(
        run_id: u64,
        lease: String,
        send: impl Fn(u64, String, Value, Duration) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            lease: Arc::new(Mutex::new(lease)),
            run_id,
            send: Arc::new(send),
        }
    }

    /// Invocation-bound scopes never accept a task lease from action context.
    pub(crate) fn for_invocation(
        send: impl Fn(Value, Duration) -> Result<Value, String> + Send + Sync + 'static,
    ) -> Self {
        Self::new(0, String::new(), move |_, _, request, timeout| {
            send(request, timeout)
        })
    }

    pub(crate) fn set_lease(&self, lease: String) {
        *self.lease.lock_recover() = lease;
    }

    /// Freeze before queuing a turn, not when its worker eventually runs.
    /// Only the sender is shared: subsequent steering cannot relabel this prompt.
    pub(crate) fn snapshot(&self) -> Self {
        Self {
            lease: Arc::new(Mutex::new(self.lease.lock_recover().clone())),
            run_id: self.run_id,
            send: Arc::clone(&self.send),
        }
    }

    /// Send a native gateway request. Each attempt claims fresh host custody.
    pub fn request(
        &self,
        request: tilo_model_gateway::Request,
        timeout: Duration,
    ) -> Result<Value, String> {
        let request = serde_json::to_value(request).map_err(|error| error.to_string())?;
        let lease = self.lease.lock_recover().clone();
        (self.send)(self.run_id, lease, request, timeout)
    }

    pub fn complete(
        &self,
        profile: &str,
        model: &str,
        request: ModelRequest,
        timeout: Duration,
        callback: &mut dyn FnMut(&StreamEvent),
    ) -> Result<ModelResponse, String> {
        // NativeComplete has already buffered the response on the host. Joining
        // adjacent prose fragments changes no live visibility and retains every
        // text/reasoning/tool boundary, including partial output on failure.
        let mut pending = None;
        let result = self.complete_inner(profile, model, request, timeout, &mut |event| {
            if let Some(event) = event {
                coalesce(&mut pending, event, callback);
            } else if let Some(event) = pending.take() {
                callback(&event);
            }
        });
        if let Some(event) = pending {
            callback(&event);
        }
        match &result {
            Ok(response) => callback(&StreamEvent::Finished {
                response: response.clone(),
            }),
            Err(_) => callback(&StreamEvent::Failed {
                error: agent_provider_config::protocol::ModelError::invalid(
                    "native model completion failed",
                ),
            }),
        }
        result
    }

    fn complete_inner(
        &self,
        profile: &str,
        model: &str,
        request: ModelRequest,
        timeout: Duration,
        callback: &mut dyn FnMut(Option<&StreamEvent>),
    ) -> Result<ModelResponse, String> {
        let reply = self.request(
            tilo_model_gateway::Request::NativeComplete {
                invocation: Value::Null,
                profile_id: profile.into(),
                model_id: model.into(),
                request,
                timeout_ms: millis(timeout),
                stream: true,
            },
            timeout,
        )?;
        // This channel adapter buffers gateway frames; replay only validated events.
        for frame in reply
            .get("events")
            .and_then(Value::as_array)
            .ok_or("missing native model events")?
        {
            let frame: tilo_model_gateway::Response =
                serde_json::from_value(frame.clone()).map_err(|error| error.to_string())?;
            if !frame.ok {
                return Err(frame.failure_error().to_string());
            }
            let payload = frame
                .output
                .as_deref()
                .ok_or("missing native model event payload")?;
            let event = match frame.event.as_deref() {
                Some("tool_call") => {
                    callback(None);
                    continue;
                }
                Some("text_delta") => StreamEvent::TextDelta {
                    text: serde_json::from_str(payload).map_err(|error| error.to_string())?,
                },
                Some("reasoning_delta") => StreamEvent::ReasoningDelta {
                    text: serde_json::from_str(payload).map_err(|error| error.to_string())?,
                },
                Some("tool_call_delta") => {
                    let value: Value =
                        serde_json::from_str(payload).map_err(|error| error.to_string())?;
                    #[derive(serde::Deserialize)]
                    struct Fragment {
                        index: u32,
                        id: Option<String>,
                        name: Option<String>,
                        delta: String,
                    }
                    let value: Fragment =
                        serde_json::from_value(value).map_err(|error| error.to_string())?;
                    StreamEvent::ToolCallDelta {
                        index: value.index,
                        id: value.id,
                        name: value.name,
                        arguments: value.delta,
                    }
                }
                _ => return Err("unknown native model event".into()),
            };
            callback(Some(&event));
        }
        let output = final_output(reply)?;
        let response: ModelResponse =
            serde_json::from_str(&output).map_err(|error| error.to_string())?;
        match response.tool_calls() {
            Err(error)
                if !matches!(
                    error.kind,
                    agent_provider_config::protocol::ErrorKind::Refused
                        | agent_provider_config::protocol::ErrorKind::Truncated
                ) =>
            {
                Err(error.to_string())
            }
            _ => Ok(response),
        }
    }

    pub fn transform(
        &self,
        profile: &str,
        model: &str,
        instructions: &str,
        input: &str,
        max_output_tokens: u32,
        timeout: Duration,
    ) -> Result<String, String> {
        final_output(self.request(
            tilo_model_gateway::Request::NativeTransform {
                invocation: json!(null),
                profile_id: profile.into(),
                model_id: model.into(),
                instructions: instructions.into(),
                input: input.into(),
                max_output_tokens,
                timeout_ms: millis(timeout),
            },
            timeout,
        )?)
    }
}

fn coalesce(
    pending: &mut Option<StreamEvent>,
    event: &StreamEvent,
    callback: &mut dyn FnMut(&StreamEvent),
) {
    match (pending.as_mut(), event) {
        (Some(StreamEvent::TextDelta { text }), StreamEvent::TextDelta { text: next })
        | (
            Some(StreamEvent::ReasoningDelta { text }),
            StreamEvent::ReasoningDelta { text: next },
        ) => {
            text.push_str(next);
            return;
        }
        _ => {}
    }
    if let Some(previous) = pending.take() {
        callback(&previous);
    }
    match event {
        StreamEvent::TextDelta { .. } | StreamEvent::ReasoningDelta { .. } => {
            *pending = Some(event.clone());
        }
        _ => callback(event),
    }
}

fn millis(timeout: Duration) -> u64 {
    u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX)
}

fn final_output(reply: Value) -> Result<String, String> {
    let response: tilo_model_gateway::Response = serde_json::from_value(
        reply
            .get("response")
            .cloned()
            .ok_or("missing native model response")?,
    )
    .map_err(|error| error.to_string())?;
    let response = response.into_result().map_err(|error| error.to_string())?;
    if response.event.is_some() {
        return Err("missing native model final response".into());
    }
    response
        .output
        .ok_or_else(|| "missing native model output".into())
}
