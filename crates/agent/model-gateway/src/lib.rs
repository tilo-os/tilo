//! Tilo model gateway protocol.
//!
//! The API key is accepted only by the per-user gateway and is never part of
//! Settings' CRDT/settings payload or an agent process. Sandboxed callers use
//! one launcher-passed connected Unix descriptor (`TILO_SVC_MODEL-GATEWAY_FD`).
//!
//! Every connection starts with a [`Principal`] handoff that fixes who it acts
//! for. The launcher writes [`Principal::App`] before a sandboxed app holds its
//! descriptor; the desktop session connects to the service socket itself,
//! which is outside every sandbox, and writes [`Principal::Session`]
//! ([`session`]).

#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::fd::{FromRawFd, RawFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::time::Duration;

use agent_provider_config::assign::{Layer, Resolution};
use agent_provider_config::{EndpointLocation, ModelDescriptor, ModelOptions, ProviderProtocol};
use serde::{Deserialize, Serialize};
use thiserror::Error;
#[cfg(unix)]
use zeroize::Zeroizing;

pub const SERVICE_NAME: &str = "model-gateway";
// Launcher and runtime-portal uppercase the service name without replacing '-'.
pub const SERVICE_FD_ENV: &str = "TILO_SVC_MODEL-GATEWAY_FD";
pub const RPC_VERSION: u16 = 5;
/// Version of the [`Principal`] handoff frame.
pub const HANDOFF_PROTOCOL: u16 = 2;
/// The handoff: a little-endian `u32` length, then JSON.
pub const HANDOFF_FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(4 * 1024);
/// Every gateway request and response: a little-endian `u32` length, then
/// JSON.
pub const FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(2 * 1024 * 1024);
/// Largest instruction text one [`Request::Transform`] may carry.
pub const MAX_TRANSFORM_INSTRUCTIONS_BYTES: usize = 16 * 1024;
/// Largest input text one [`Request::Transform`] may carry.
pub const MAX_TRANSFORM_INPUT_BYTES: usize = 256 * 1024;
/// The most output tokens one [`Request::Transform`] may ask a model for.
pub const MAX_TRANSFORM_OUTPUT_TOKENS: u32 = 4096;
/// The longest one [`Request::Transform`] may wait on a provider, retries
/// included.
pub const MAX_TRANSFORM_TIMEOUT: Duration = Duration::from_secs(30);
/// The largest model answer a [`Request::Transform`] returns.
pub const MAX_TRANSFORM_OUTPUT_BYTES: usize = 256 * 1024;

/// The only encoded-preview media types accepted by the local vision RPC.
/// Keeping this vocabulary closed means a caller cannot ask the trusted
/// gateway to fetch a URL or interpret an arbitrary document as an image.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct VisionImage {
    /// `image/jpeg`, `image/png`, or `image/webp`.
    pub media_type: String,
    /// Standard base64 of a caller-decoded, downscaled preview.
    pub data_base64: String,
    /// SHA-256 of the original photo bytes, not of `data_base64`.
    pub source_content_hash: String,
}

/// One deliberately small photograph reference supplied to the dedicated
/// local face runtime.  `photo_id` is an app address, not a pathname; the
/// runtime never receives an identity label or an instruction from Photos.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct FaceImage {
    pub photo_id: String,
    pub media_type: String,
    pub data_base64: String,
    pub source_content_hash: String,
}

/// Who a gateway connection acts for, fixed by its first frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Principal {
    /// A sandboxed app, named by the launcher that connected on its behalf.
    App { app_id: String },
    /// The desktop session itself (the shell).
    Session,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct HandoffFrame {
    protocol: u16,
    principal: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    app_id: Option<String>,
}

impl Principal {
    /// The handoff body for this principal: `{"protocol":2,"principal":"app",
    /// "app_id":…}` or `{"protocol":2,"principal":"session"}`. The launcher
    /// writes the same bytes for apps.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Protocol`] if the frame cannot be encoded.
    pub fn handoff(&self) -> Result<Vec<u8>, ClientError> {
        let frame = match self {
            Self::App { app_id } => HandoffFrame {
                protocol: HANDOFF_PROTOCOL,
                principal: "app".into(),
                app_id: Some(app_id.clone()),
            },
            Self::Session => HandoffFrame {
                protocol: HANDOFF_PROTOCOL,
                principal: "session".into(),
                app_id: None,
            },
        };
        serde_json::to_vec(&frame).map_err(|error| ClientError::Protocol(error.to_string()))
    }

    /// Decode a handoff body.
    ///
    /// # Errors
    ///
    /// Returns [`ClientError::Protocol`] for a malformed frame, another
    /// protocol version or an unknown principal.
    pub fn from_handoff(body: &[u8]) -> Result<Self, ClientError> {
        let invalid = |why: &str| ClientError::Protocol(why.to_owned());
        let frame: HandoffFrame =
            serde_json::from_slice(body).map_err(|_| invalid("malformed gateway handoff"))?;
        if frame.protocol != HANDOFF_PROTOCOL {
            return Err(invalid("unsupported gateway handoff protocol"));
        }
        match (frame.principal.as_str(), frame.app_id) {
            ("app", Some(app_id)) if !app_id.is_empty() => Ok(Self::App { app_id }),
            ("session", None) => Ok(Self::Session),
            _ => Err(invalid("gateway handoff names an unknown principal")),
        }
    }

    /// The app id, for an app principal.
    #[must_use]
    pub fn app_id(&self) -> Option<&str> {
        match self {
            Self::App { app_id } => Some(app_id),
            Self::Session => None,
        }
    }
}

impl From<&Principal> for agent_provider_config::tasks::Consumer {
    fn from(principal: &Principal) -> Self {
        match principal {
            Principal::App { app_id } => Self::App {
                app_id: app_id.clone(),
            },
            Principal::Session => Self::Session,
        }
    }
}

/// A request that can cross the sandbox boundary. Inference verbs name a
/// registered task ([`agent_provider_config::tasks`]), never a provider
/// profile or a model: the gateway resolves the task through the person's
/// assignments and enforces its constraints. `SetApiKey` is only accepted
/// from Settings; a server also authenticates the launcher-provided app
/// preamble.
#[derive(Deserialize, Serialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum Request {
    /// Which model would serve `task` for this caller (acting as `agent`).
    /// Returns a [`Route`]; carries no secret.
    Resolve {
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
    },
    /// Read-only check, for a host that launches a managed agent bound to one
    /// provider model, of that selection's local Plan prerequisites: the
    /// profile is configured and enabled, lists the model, speaks `protocol`
    /// and holds its credential. A positive `configured` response does not
    /// establish provider reachability, accepted credentials, or live
    /// local-model availability, and dispatches nothing.
    SelectionConfigured {
        profile_id: String,
        model_id: String,
        protocol: agent_provider_config::ProviderProtocol,
    },
    /// A provider-dialect request for `task`. `protocol` is the dialect the
    /// caller wrote `body` in, taken from a prior [`Request::Resolve`]; the
    /// gateway re-resolves, refuses a body written for another dialect, and
    /// sets the model and its typed options itself.
    Plan {
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        protocol: ProviderProtocol,
        body: serde_json::Value,
        timeout_ms: u64,
        /// Ask for streaming frames: zero or more [`Response`]s with an
        /// `event` (each a completed tool call), then one final response
        /// without `event` carrying the assembled, non-streaming-shaped
        /// provider body. Older gateways ignore the field and send only the
        /// final response, which a streaming client reads the same way.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        stream: bool,
        /// With `stream`, also ask for coalesced [`EVENT_TOOL_CALL_DELTA`]
        /// frames carrying the argument text of the call in progress. Older
        /// gateways ignore it; the client then sees only `tool_call` events.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        deltas: bool,
        /// Opaque, content-free correlation for the gateway's timing log.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trace: Option<PlanTrace>,
    },
    /// A content-free timing mark from the agent (whose own stderr a managed
    /// launch discards), written to the gateway's timing log. Older RPC3
    /// servers reject this additive verb; callers ignore the refusal.
    Trace {
        trace: PlanTrace,
        event: TraceEvent,
        /// When the mark happened (Unix ms), if the caller had to hold it
        /// while a streamed Plan owned the channel. Absent: now.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at_unix_ms: Option<u64>,
    },
    /// One bounded, single-shot text transformation for tilo-agent
    /// (`agent.transform`): a system instruction and one input, answered
    /// with the model's text. No tools, no conversation, no provider
    /// envelope: the gateway resolves the task, builds the provider request
    /// itself, caps its size, output and deadline, and returns only the
    /// answer text.
    Transform {
        task: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent: Option<String>,
        instructions: String,
        input: String,
        max_output_tokens: u32,
        timeout_ms: u64,
    },
    /// Fixed-shape visual analysis for Photos (`photos.vision`). The gateway
    /// owns the prompt; a sandboxed caller never obtains network access.
    AnalyzeImage {
        image: VisionImage,
    },
    /// Fixed, consent-gated face detection and clustering
    /// (`photos.faces`). It returns opaque cluster/photo membership; no
    /// embedding, crop, face rectangle, identity name, or raw path can cross
    /// this boundary.
    ClusterFaces {
        images: Vec<FaceImage>,
    },
    /// Fixed-shape, device-local review of a feedback recording: redaction
    /// proposals and step titles. The gateway owns the prompt and the output
    /// schema, validates the model's answer against `trace`, and refuses
    /// hosted, unpinned or undeclared profiles. Only the desktop session may
    /// ask, because review runs there. The caller names no profile or model:
    /// the gateway resolves the `feedback.review` task.
    /// The answer is a JSON [`ReviewAnswer`] holding model proposals only;
    /// the caller merges them over its own deterministic floor.
    ReviewTrace {
        trace: tilo_feedback_review::ReviewTraceRequest,
    },
    SecretStatus {
        profile_id: String,
    },
    SetApiKey {
        profile_id: String,
        api_key: String,
    },
    RemoveApiKey {
        profile_id: String,
    },
    /// Settings asks whether a provider's credential is present and whether
    /// its endpoint answers with it. Returns [`ProviderHealth`].
    Probe {
        profile_id: String,
    },
}

/// Where a task is served, as a caller may know it: no endpoint, no
/// credential.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct Route {
    pub task: String,
    pub profile_id: String,
    pub profile_label: String,
    pub protocol: ProviderProtocol,
    pub location: EndpointLocation,
    pub model: ModelDescriptor,
    pub options: ModelOptions,
    pub layer: Layer,
}

impl Route {
    #[must_use]
    pub fn from_resolution(resolution: &Resolution) -> Self {
        Self {
            task: resolution.task.id.to_string(),
            profile_id: resolution.profile.id.clone(),
            profile_label: resolution.profile.display_label().to_string(),
            protocol: resolution.profile.protocol,
            location: resolution.profile.location,
            model: resolution.model.clone(),
            options: resolution.options,
            layer: resolution.layer,
        }
    }
}

/// Whether a provider can be used from this device.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialState {
    /// The endpoint needs no credential (this device's runtime).
    NotNeeded,
    /// No key is stored on this device, or the account is not signed in.
    Missing,
    /// A key or a signed-in account token is present.
    Present,
}

/// Whether the endpoint answered a model listing with the credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Reachability {
    Reachable,
    /// The endpoint answered but refused the credential.
    Unauthorized,
    /// The endpoint answered with another failure status.
    Failed {
        status: u16,
    },
    /// Nothing answered.
    Unreachable,
    /// Not checked because no credential is present.
    NotChecked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct ProviderHealth {
    pub credential: CredentialState,
    pub reachability: Reachability,
}

/// Correlates the Plan requests (model turns) of one agent decision.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct PlanTrace {
    /// Random per decision; carries no content.
    pub id: u64,
    /// Zero-based model turn within the decision, or a step index for marks.
    pub turn: u32,
}

/// The closed vocabulary of agent timing marks.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TraceEvent {
    /// The model's final plan was read and accepted by the agent.
    PlanReady,
    /// The decision failed (fallback, timeout, invalid answer).
    PlanFailed,
    /// The agent sent one planned action to the host.
    ActionSubmitted,
    /// The host settled that action.
    ActionSettled,
}

/// The only streaming event kind: one completed provider tool call, as
/// `{"id":..,"name":..,"arguments":<JSON string>}` in [`Response::output`].
pub const EVENT_TOOL_CALL: &str = "tool_call";

/// An intermediate streaming event requested with `deltas`: raw argument text
/// appended to the tool call in progress, as
/// `{"index":u32,"id":str|null,"name":str,"delta":str}` in
/// [`Response::output`]. Every delta of a call precedes its `tool_call`.
pub const EVENT_TOOL_CALL_DELTA: &str = "tool_call_delta";

#[derive(Debug, Deserialize, Serialize)]
pub struct Frame<T> {
    pub version: u16,
    #[serde(flatten)]
    pub value: T,
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub route: Option<Route>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<ProviderHealth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Set only on intermediate streaming frames (see [`EVENT_TOOL_CALL`]).
    /// A response without an event is the final frame of a request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
}

#[derive(Debug, Error)]
pub enum ClientError {
    #[error("Tilo model gateway is unavailable")]
    Unavailable,
    #[error("model gateway protocol error: {0}")]
    Protocol(String),
    #[error("model gateway refused request: {0}")]
    Rejected(String),
}

/// Whether this process has the launcher-issued service descriptor. This does
/// not probe or connect to a pathname socket, so the sandbox cannot substitute
/// a different same-UID service.
#[cfg(unix)]
pub fn available() -> bool {
    std::env::var(SERVICE_FD_ENV)
        .ok()
        .and_then(|value| value.parse::<RawFd>().ok())
        .is_some_and(|fd| fd >= 3 && unsafe { libc::fcntl(fd, libc::F_GETFD) } != -1)
}

/// No launcher-issued gateway transport is implemented on this platform.
/// Environment variables cannot manufacture a supported capability.
#[cfg(not(unix))]
pub fn available() -> bool {
    false
}

/// Send a request over the launcher-provided service fd. The fd is duplicated
/// for each RPC so a short-lived helper cannot close the parent app's channel.
///
/// # Errors
///
/// Returns [`ClientError::Unavailable`] when the launcher service descriptor or
/// gateway connection is unavailable, [`ClientError::Protocol`] for malformed
/// frames, and [`ClientError::Rejected`] when the gateway refuses the request.
#[cfg(unix)]
pub fn request(request: Request) -> Result<Response, ClientError> {
    let mut stream = service_stream()?;
    write_frame(
        &mut stream,
        &Frame {
            version: RPC_VERSION,
            value: request,
        },
    )?;
    let response: Response = read_frame(&mut stream)?;
    if response.ok {
        Ok(response)
    } else {
        Err(ClientError::Rejected(
            response
                .error
                .unwrap_or_else(|| "request rejected".to_string()),
        ))
    }
}

/// Duplicate the launcher-issued service descriptor for one RPC.
#[cfg(unix)]
fn service_stream() -> Result<UnixStream, ClientError> {
    let fd = std::env::var(SERVICE_FD_ENV)
        .ok()
        .and_then(|value| value.parse::<RawFd>().ok())
        .filter(|fd| *fd >= 3 && unsafe { libc::fcntl(*fd, libc::F_GETFD) } != -1)
        .ok_or(ClientError::Unavailable)?;
    // SAFETY: F_DUPFD_CLOEXEC duplicates a checked open descriptor; ownership
    // of the duplicate transfers immediately to UnixStream.
    let duplicate = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
    if duplicate < 0 {
        return Err(ClientError::Unavailable);
    }
    Ok(unsafe { UnixStream::from_raw_fd(duplicate) })
}

/// Refuse gateway operations when the launcher capability transport is absent.
///
/// # Errors
///
/// Always returns [`ClientError::Unavailable`]. There is no direct-network or
/// pathname-socket fallback for an unsupported host.
#[cfg(not(unix))]
pub fn request(_request: Request) -> Result<Response, ClientError> {
    Err(ClientError::Unavailable)
}

/// Which model serves `task` for this caller, acting as `agent` when given.
///
/// # Errors
///
/// Returns [`ClientError`] when the gateway is unavailable or no model can
/// serve the task within its constraints.
pub fn resolve(task: &str, agent: Option<&str>) -> Result<Route, ClientError> {
    request(Request::Resolve {
        task: task.to_string(),
        agent: agent.map(str::to_string),
    })?
    .route
    .ok_or_else(|| ClientError::Protocol("gateway returned no route".into()))
}

/// Send one provider-dialect request for `task`. `protocol` is the dialect
/// `body` is written in, from the [`Route`] the caller resolved.
///
/// # Errors
///
/// Returns [`ClientError`] when the gateway is unavailable, rejects the
/// request, returns a malformed response, or the provider request fails.
pub fn plan(
    task: &str,
    agent: Option<&str>,
    protocol: ProviderProtocol,
    body: serde_json::Value,
    timeout: Duration,
) -> Result<String, ClientError> {
    let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    let response = request(Request::Plan {
        task: task.to_string(),
        agent: agent.map(str::to_string),
        protocol,
        body,
        timeout_ms,
        stream: false,
        deltas: false,
        trace: None,
    })?;
    response
        .output
        .ok_or_else(|| ClientError::Protocol("gateway returned no provider response".into()))
}

/// Streaming Plan: `on_event` sees each completed tool call as the provider
/// streams it; the return value is the final assembled provider body, in the
/// same shape [`plan`] returns. Every frame of the request is read before
/// this returns, so the shared service channel stays in step.
///
/// # Errors
///
/// As [`plan`].
pub fn plan_streaming(
    task: &str,
    agent: Option<&str>,
    protocol: ProviderProtocol,
    body: serde_json::Value,
    timeout: Duration,
    trace: Option<PlanTrace>,
    on_event: &mut dyn FnMut(&str, &str),
) -> Result<String, ClientError> {
    let target = PlanTarget {
        task,
        agent,
        protocol,
    };
    stream_plan(&target, body, timeout, trace, false, on_event)
}

/// [`plan_streaming`] that also asks for [`EVENT_TOOL_CALL_DELTA`] frames:
/// `on_event` sees both `tool_call_delta` and `tool_call` events. A gateway
/// that predates deltas sends only `tool_call` events.
///
/// # Errors
///
/// As [`plan`].
pub fn plan_streaming_deltas(
    task: &str,
    agent: Option<&str>,
    protocol: ProviderProtocol,
    body: serde_json::Value,
    timeout: Duration,
    trace: Option<PlanTrace>,
    on_event: &mut dyn FnMut(&str, &str),
) -> Result<String, ClientError> {
    let target = PlanTarget {
        task,
        agent,
        protocol,
    };
    stream_plan(&target, body, timeout, trace, true, on_event)
}

/// What one streaming Plan names: its task, the agent it acts as, and the
/// dialect its body is written in.
struct PlanTarget<'a> {
    task: &'a str,
    agent: Option<&'a str>,
    protocol: ProviderProtocol,
}

#[cfg(unix)]
fn stream_plan(
    target: &PlanTarget<'_>,
    body: serde_json::Value,
    timeout: Duration,
    trace: Option<PlanTrace>,
    deltas: bool,
    on_event: &mut dyn FnMut(&str, &str),
) -> Result<String, ClientError> {
    let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    let mut stream = service_stream()?;
    write_frame(
        &mut stream,
        &Frame {
            version: RPC_VERSION,
            value: Request::Plan {
                task: target.task.to_string(),
                agent: target.agent.map(str::to_string),
                protocol: target.protocol,
                body,
                timeout_ms,
                stream: true,
                deltas,
                trace,
            },
        },
    )?;
    // A bounded number of intermediate frames: one per completed tool call,
    // and (only when asked for) a separately bounded number of deltas.
    let mut calls = 0_usize;
    let mut delta_frames = 0_usize;
    loop {
        let response: Response = read_frame(&mut stream)?;
        if !response.ok {
            return Err(ClientError::Rejected(
                response
                    .error
                    .unwrap_or_else(|| "request rejected".to_string()),
            ));
        }
        match response.event {
            Some(event) => {
                let count = if deltas && event == EVENT_TOOL_CALL_DELTA {
                    delta_frames = delta_frames.saturating_add(1);
                    delta_frames > MAX_DELTA_EVENTS
                } else {
                    calls = calls.saturating_add(1);
                    calls > MAX_STREAM_EVENTS
                };
                if count {
                    return Err(ClientError::Protocol(
                        "gateway sent too many streaming events".into(),
                    ));
                }
                on_event(&event, response.output.as_deref().unwrap_or(""));
            }
            None => {
                return response.output.ok_or_else(|| {
                    ClientError::Protocol("gateway returned no provider response".into())
                });
            }
        }
    }
}

/// No launcher-issued gateway transport exists on this platform.
///
/// # Errors
///
/// Always [`ClientError::Unavailable`].
#[cfg(not(unix))]
fn stream_plan(
    _target: &PlanTarget<'_>,
    _body: serde_json::Value,
    _timeout: Duration,
    _trace: Option<PlanTrace>,
    _deltas: bool,
    _on_event: &mut dyn FnMut(&str, &str),
) -> Result<String, ClientError> {
    Err(ClientError::Unavailable)
}

/// The most intermediate frames one streaming Plan may carry.
pub const MAX_STREAM_EVENTS: usize = 64;

/// The most `tool_call_delta` frames one streaming Plan may carry; bounded
/// separately from [`MAX_STREAM_EVENTS`]. A gateway stops sending deltas at
/// this count (the completed `tool_call` events still follow).
pub const MAX_DELTA_EVENTS: usize = 4096;

/// Send a content-free timing mark. Best effort: a gateway that predates the
/// verb refuses it, and that refusal is ignored by callers.
///
/// # Errors
///
/// As [`request`].
pub fn trace(trace: PlanTrace, event: TraceEvent) -> Result<(), ClientError> {
    trace_at(trace, event, None)
}

/// [`trace`] for a mark that happened at `at_unix_ms` and was held back while
/// a streamed Plan used the channel; the gateway logs that time.
///
/// # Errors
///
/// As [`trace`].
pub fn trace_at(
    trace: PlanTrace,
    event: TraceEvent,
    at_unix_ms: Option<u64>,
) -> Result<(), ClientError> {
    request(Request::Trace {
        trace,
        event,
        at_unix_ms,
    })
    .map(|_| ())
}

/// Run one bounded single-shot transformation for `task` through the
/// isolated model gateway and return the model's answer text. The person's
/// assignment, not the caller, picks the model. The gateway enforces the
/// `MAX_TRANSFORM_*` bounds; a request past them is refused, not truncated.
///
/// # Errors
///
/// Returns [`ClientError`] when the gateway is unavailable, refuses the
/// request (including for exceeding a bound), or the provider call fails.
pub fn transform(
    task: &str,
    agent: Option<&str>,
    instructions: &str,
    input: &str,
    max_output_tokens: u32,
    timeout: Duration,
) -> Result<String, ClientError> {
    let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    let response = request(Request::Transform {
        task: task.to_string(),
        agent: agent.map(str::to_string),
        instructions: instructions.to_string(),
        input: input.to_string(),
        max_output_tokens,
        timeout_ms,
    })?;
    response
        .output
        .ok_or_else(|| ClientError::Protocol("gateway returned no transform answer".into()))
}

/// Request a fixed-schema visual analysis through the launcher-issued gateway
/// descriptor. Only Photos may ask; the gateway resolves `photos.vision` to a
/// model on this device. Callers cannot provide a URL, prompt, or timeout.
///
/// # Errors
///
/// Returns [`ClientError`] if the capability is not passed by the launcher,
/// the gateway rejects the request, or the provider is unavailable.
pub fn analyze_image(image: VisionImage) -> Result<String, ClientError> {
    let response = request(Request::AnalyzeImage { image })?;
    response
        .output
        .ok_or_else(|| ClientError::Protocol("gateway returned no image analysis".into()))
}

/// Run the dedicated local face model (`photos.faces`) against one bounded
/// batch. This is not a generic multimodal request and cannot be redirected
/// to a hosted model.
///
/// # Errors
///
/// Returns [`ClientError`] if the launcher gateway is absent, the local model
/// refuses the fixed protocol, or its response is unavailable.
pub fn cluster_faces(images: Vec<FaceImage>) -> Result<String, ClientError> {
    let response = request(Request::ClusterFaces { images })?;
    response
        .output
        .ok_or_else(|| ClientError::Protocol("gateway returned no face clusters".into()))
}

/// Whether a provider's credential is present on this device and its
/// endpoint answers with it. Settings only.
///
/// # Errors
///
/// Returns [`ClientError`] when the gateway cannot be reached or refuses.
pub fn probe(profile_id: &str) -> Result<ProviderHealth, ClientError> {
    request(Request::Probe {
        profile_id: profile_id.to_string(),
    })?
    .health
    .ok_or_else(|| ClientError::Protocol("gateway returned no provider health".into()))
}

/// Query whether the profile has a secret stored by the isolated gateway.
///
/// # Errors
///
/// Returns [`ClientError`] when the gateway cannot be reached or rejects or
/// malforms the response.
pub fn secret_configured(profile_id: &str) -> Result<bool, ClientError> {
    Ok(request(Request::SecretStatus {
        profile_id: profile_id.to_string(),
    })?
    .configured
    .unwrap_or(false))
}

/// Store a profile API key in the isolated gateway.
///
/// # Errors
///
/// Returns [`ClientError`] when the gateway cannot be reached, rejects the
/// update, or returns a malformed response.
pub fn set_api_key(profile_id: &str, api_key: String) -> Result<(), ClientError> {
    request(Request::SetApiKey {
        profile_id: profile_id.to_string(),
        api_key,
    })?;
    Ok(())
}

/// Remove a profile API key from the isolated gateway.
///
/// # Errors
///
/// Returns [`ClientError`] when the gateway cannot be reached, rejects the
/// update, or returns a malformed response.
pub fn remove_api_key(profile_id: &str) -> Result<(), ClientError> {
    request(Request::RemoveApiKey {
        profile_id: profile_id.to_string(),
    })?;
    Ok(())
}

/// Write one size-bounded JSON frame to a gateway stream.
///
/// # Errors
///
/// Returns [`ClientError::Protocol`] when serialization fails or the frame is
/// too large, and [`ClientError::Unavailable`] when writing fails.
#[cfg(unix)]
pub fn write_frame<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<(), ClientError> {
    let body = Zeroizing::new(
        serde_json::to_vec(value).map_err(|error| ClientError::Protocol(error.to_string()))?,
    );
    FRAMING.write(stream, &body).map_err(|error| match error {
        wire_frame::FrameError::Io(_) => ClientError::Unavailable,
        _ => ClientError::Protocol("request exceeds gateway frame limit".into()),
    })?;
    stream.flush().map_err(|_| ClientError::Unavailable)
}

/// Read and decode one size-bounded JSON frame from a gateway stream.
///
/// # Errors
///
/// Returns [`ClientError::Unavailable`] when reading fails and
/// [`ClientError::Protocol`] for an invalid length or payload.
#[cfg(unix)]
pub fn read_frame<T: for<'a> Deserialize<'a>>(stream: &mut UnixStream) -> Result<T, ClientError> {
    let mut body = Zeroizing::new(Vec::new());
    match FRAMING.read_into(stream, &mut body) {
        Ok(true) => {}
        Err(wire_frame::FrameError::TooLarge(_)) => {
            return Err(ClientError::Protocol("gateway frame exceeds limit".into()));
        }
        Ok(false) | Err(_) => return Err(ClientError::Unavailable),
    }
    serde_json::from_slice(&body)
        .map_err(|_| ClientError::Protocol("invalid gateway response".into()))
}

/// The model that answered a review.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewModel {
    pub profile_id: String,
    pub model_id: String,
    /// Absent for a model with no attested revision (one off this device).
    pub revision: Option<String>,
}

/// The gateway's answer to [`Request::ReviewTrace`].
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewAnswer {
    /// The model that answered; `None` when no device-local model declares
    /// trace review, in which case `proposals` is empty.
    pub model: Option<ReviewModel>,
    /// The model's proposals only, checked against the request.
    pub proposals: tilo_feedback_review::ReviewProposals,
}

/// The desktop session's own gateway calls.
///
/// The shell is not sandboxed and is not launched through the launcher, so it
/// has no launcher-issued descriptor. It connects to the gateway's service
/// socket itself (`launcher::services::socket_for("model-gateway")`, the path
/// the systemd socket unit listens on) and names itself with
/// [`Principal::Session`]. The socket is outside every sandbox and `0600`, so
/// only an unsandboxed process of the same user can present that principal:
/// the same trust root the launcher's app handoff already rests on.
#[cfg(unix)]
pub mod session {
    use std::path::Path;

    use tilo_feedback_review::{ContractError, ReviewProposals, ReviewTraceRequest, floor, merge};

    use super::{
        ClientError, Frame, HANDOFF_FRAMING, Principal, RPC_VERSION, Request, Response,
        ReviewAnswer, ReviewModel, read_frame, write_frame,
    };

    /// How long a session call may wait for the gateway's answer. Review on a
    /// laptop GPU is the slowest verb the session uses.
    const ANSWER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

    /// A session connection to the gateway.
    pub struct Connection(std::os::unix::net::UnixStream);

    impl Connection {
        /// Connect to the gateway at `socket` and present the session handoff.
        ///
        /// # Errors
        ///
        /// Returns [`ClientError::Unavailable`] when the gateway cannot be
        /// reached.
        pub fn open(socket: &Path) -> Result<Self, ClientError> {
            let mut stream = std::os::unix::net::UnixStream::connect(socket)
                .map_err(|_| ClientError::Unavailable)?;
            stream
                .set_read_timeout(Some(ANSWER_TIMEOUT))
                .map_err(|_| ClientError::Unavailable)?;
            HANDOFF_FRAMING
                .write(&mut stream, &Principal::Session.handoff()?)
                .map_err(|_| ClientError::Unavailable)?;
            Ok(Self(stream))
        }

        /// Send one request and read its answer.
        ///
        /// # Errors
        ///
        /// Returns [`ClientError`] when the gateway is unreachable, answers
        /// malformed, or refuses.
        pub fn request(&mut self, request: Request) -> Result<Response, ClientError> {
            write_frame(
                &mut self.0,
                &Frame {
                    version: RPC_VERSION,
                    value: request,
                },
            )?;
            let response: Response = read_frame(&mut self.0)?;
            if response.ok {
                Ok(response)
            } else {
                Err(ClientError::Rejected(
                    response.error.unwrap_or_else(|| "request rejected".into()),
                ))
            }
        }

        /// Which model serves `task` for the session, acting as `agent`
        /// when given.
        ///
        /// # Errors
        ///
        /// Returns [`ClientError`] when the gateway is unreachable or no model
        /// can serve the task within its constraints.
        pub fn resolve(
            &mut self,
            task: &str,
            agent: Option<&str>,
        ) -> Result<super::Route, ClientError> {
            self.request(Request::Resolve {
                task: task.to_string(),
                agent: agent.map(str::to_string),
            })?
            .route
            .ok_or_else(|| ClientError::Protocol("gateway returned no route".into()))
        }

        /// Ask the device-local review model about `trace`. The proposals
        /// were checked by the gateway and are checked again here against the
        /// same request.
        ///
        /// # Errors
        ///
        /// Returns [`ClientError`] when the gateway refuses (malformed model
        /// output, unreachable runtime) or is unreachable.
        pub fn review_trace(
            &mut self,
            trace: &ReviewTraceRequest,
        ) -> Result<ReviewAnswer, ClientError> {
            let response = self.request(Request::ReviewTrace {
                trace: trace.clone(),
            })?;
            let output = response
                .output
                .ok_or_else(|| ClientError::Protocol("gateway returned no review".into()))?;
            let answer: ReviewAnswer = serde_json::from_str(&output)
                .map_err(|_| ClientError::Protocol("gateway returned a malformed review".into()))?;
            if answer.model.is_none() && answer.proposals != ReviewProposals::default() {
                return Err(ClientError::Protocol(
                    "gateway reviewed without a model".into(),
                ));
            }
            if answer
                .proposals
                .titles
                .iter()
                .any(|title| !matches!(title.title, tilo_feedback_review::TitleText::Model { .. }))
            {
                return Err(ClientError::Protocol(
                    "a model title must be marked as model text".into(),
                ));
            }
            answer
                .proposals
                .validate_against(trace)
                .map_err(|error| ClientError::Protocol(error.to_string()))?;
            Ok(answer)
        }
    }

    /// What the local model contributed to a review.
    #[derive(Debug)]
    pub enum ModelContribution {
        /// Its proposals were merged over the floor.
        Merged(ReviewModel),
        /// No device-local model declares trace review. The caller may offer
        /// to install one; review is complete without it.
        NoLocalModel,
        /// The gateway or the model did not answer usably; the floor stands.
        Failed(ClientError),
    }

    /// The result of reviewing one request.
    #[derive(Debug)]
    pub struct ReviewOutcome {
        pub proposals: ReviewProposals,
        pub model: ModelContribution,
    }

    /// Review `trace`: always the deterministic floor, with the local model's
    /// proposals merged on top when the gateway has a declared model and it
    /// answers. Blocking; run it off the compositor's event loop.
    ///
    /// # Errors
    ///
    /// Returns [`ContractError`] when `trace` breaks the review contract's
    /// bounds; split long recordings into several requests.
    pub fn review(
        socket: &Path,
        trace: &ReviewTraceRequest,
    ) -> Result<ReviewOutcome, ContractError> {
        trace.validate()?;
        let floor = floor(trace);
        let answer = Connection::open(socket).and_then(|mut gateway| gateway.review_trace(trace));
        Ok(match answer {
            Ok(ReviewAnswer {
                model: Some(model),
                proposals,
            }) => ReviewOutcome {
                proposals: merge(trace, floor, Some(proposals)),
                model: ModelContribution::Merged(model),
            },
            Ok(ReviewAnswer { model: None, .. }) => ReviewOutcome {
                proposals: merge(trace, floor, None),
                model: ModelContribution::NoLocalModel,
            },
            Err(error) => ReviewOutcome {
                proposals: merge(trace, floor, None),
                model: ModelContribution::Failed(error),
            },
        })
    }
}

#[cfg(all(test, unix))]
mod wire_tests {
    use super::*;

    /// The gateway socket's bytes: a little-endian `u32` length, then JSON.
    #[test]
    fn gateway_wire_bytes_are_pinned() {
        use std::io::Read as _;
        let (mut client, mut gateway) = UnixStream::pair().unwrap();
        write_frame(
            &mut client,
            &Response {
                ok: true,
                output: Some("x".into()),
                ..Response::default()
            },
        )
        .unwrap();
        let mut sent = [0_u8; 28];
        gateway.read_exact(&mut sent).unwrap();
        assert_eq!(sent[..4], [24, 0, 0, 0]);
        assert_eq!(&sent[4..], br#"{"ok":true,"output":"x"}"#);

        gateway.write_all(&[12, 0, 0, 0]).unwrap();
        gateway.write_all(br#"{"ok":false}"#).unwrap();
        let response: Response = read_frame(&mut client).unwrap();
        assert!(!response.ok);
    }
}

#[cfg(test)]
#[cfg(unix)]
mod session_tests {
    use tilo_feedback_review::{
        Confidence, DeclaredSensitivity, RedactionBasis, RedactionCategory, RedactionProposal,
        ReviewProposals, ReviewStep, ReviewText, ReviewTraceRequest, Span, StepKind, StepOutcome,
        StepTitle, TextOrigin, TitleText,
    };

    use super::session::{ModelContribution, review};
    use super::*;

    fn trace() -> ReviewTraceRequest {
        ReviewTraceRequest {
            steps: vec![ReviewStep {
                kind: StepKind::Edited,
                outcome: StepOutcome::Done,
                app: "Mail".into(),
                action: "Message".into(),
                resource_kind: "mail.draft".into(),
                texts: vec![ReviewText {
                    node: "r:body".into(),
                    sensitivity: DeclaredSensitivity::Public,
                    kind: None,
                    origin: TextOrigin::Semantic,
                    text: "Hi Petr, mail petr@example.cz".into(),
                }],
            }],
        }
    }

    fn reviewer() -> ReviewModel {
        ReviewModel {
            profile_id: "local".into(),
            model_id: "reviewer".into(),
            revision: Some("a".repeat(40)),
        }
    }

    /// A gateway stand-in that checks the session handoff and answers one
    /// review with `answer`.
    fn gateway(
        answer: ReviewAnswer,
    ) -> (
        tempfile::TempDir,
        std::path::PathBuf,
        std::thread::JoinHandle<()>,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("model-gateway.sock");
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let serving = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let handoff = HANDOFF_FRAMING.read(&mut stream).unwrap().unwrap();
            assert_eq!(
                Principal::from_handoff(&handoff).unwrap(),
                Principal::Session
            );
            let frame: Frame<Request> = read_frame(&mut stream).unwrap();
            assert_eq!(frame.version, RPC_VERSION);
            assert!(matches!(frame.value, Request::ReviewTrace { .. }));
            write_frame(
                &mut stream,
                &Response {
                    ok: true,
                    output: Some(serde_json::to_string(&answer).unwrap()),
                    ..Response::default()
                },
            )
            .unwrap();
        });
        (dir, path, serving)
    }

    fn petr(span: Span) -> ReviewAnswer {
        ReviewAnswer {
            model: Some(reviewer()),
            proposals: ReviewProposals {
                redactions: vec![RedactionProposal {
                    step: 0,
                    node: "r:body".into(),
                    span,
                    category: RedactionCategory::Name,
                    confidence: Confidence::new(80).unwrap(),
                    basis: RedactionBasis::Model,
                }],
                titles: vec![StepTitle {
                    step: 0,
                    title: TitleText::Model {
                        text: "You wrote to Petr".into(),
                    },
                }],
            },
        }
    }

    #[test]
    fn the_model_is_merged_over_the_floor() {
        let (_dir, socket, serving) = gateway(petr(Span { start: 3, end: 7 }));
        let outcome = review(&socket, &trace()).unwrap();
        serving.join().unwrap();
        assert!(matches!(outcome.model, ModelContribution::Merged(_)));
        let bases: Vec<RedactionBasis> = outcome
            .proposals
            .redactions
            .iter()
            .map(|p| p.basis)
            .collect();
        assert_eq!(bases, [RedactionBasis::Model, RedactionBasis::Pattern]);
        assert_eq!(
            outcome.proposals.titles[0].title,
            TitleText::Model {
                text: "You wrote to \u{25AE}\u{25AE}\u{25AE}\u{25AE}".into()
            }
        );
    }

    #[test]
    fn an_answer_that_misaddresses_the_request_leaves_only_the_floor() {
        let (_dir, socket, serving) = gateway(petr(Span { start: 3, end: 400 }));
        let outcome = review(&socket, &trace()).unwrap();
        serving.join().unwrap();
        assert!(matches!(
            outcome.model,
            ModelContribution::Failed(ClientError::Protocol(_))
        ));
        assert_eq!(outcome.proposals.redactions.len(), 1);
        assert_eq!(
            outcome.proposals.redactions[0].basis,
            RedactionBasis::Pattern
        );
        assert!(matches!(
            outcome.proposals.titles[0].title,
            TitleText::Label(_)
        ));
    }

    #[test]
    fn without_a_local_model_review_is_the_floor() {
        let (_dir, socket, serving) = gateway(ReviewAnswer {
            model: None,
            proposals: ReviewProposals::default(),
        });
        let outcome = review(&socket, &trace()).unwrap();
        serving.join().unwrap();
        assert!(matches!(outcome.model, ModelContribution::NoLocalModel));
        assert_eq!(outcome.proposals.redactions.len(), 1);

        let unreachable = review(
            std::path::Path::new("/nonexistent/model-gateway.sock"),
            &trace(),
        )
        .unwrap();
        assert!(matches!(
            unreachable.model,
            ModelContribution::Failed(ClientError::Unavailable)
        ));
        assert_eq!(unreachable.proposals.redactions.len(), 1);
    }
}

#[cfg(all(test, not(unix)))]
mod unsupported_platform_tests {
    use super::*;

    #[test]
    fn photos_and_secret_operations_require_a_supported_gateway_capability() {
        assert!(!available());
        assert!(matches!(
            analyze_image(VisionImage {
                media_type: "image/png".into(),
                data_base64: String::new(),
                source_content_hash: String::new(),
            }),
            Err(ClientError::Unavailable)
        ));
        assert!(matches!(
            cluster_faces(Vec::new()),
            Err(ClientError::Unavailable)
        ));
        assert!(matches!(
            plan(
                "agent.planning",
                None,
                ProviderProtocol::Anthropic,
                serde_json::Value::Null,
                Duration::from_secs(1)
            ),
            Err(ClientError::Unavailable)
        ));
        assert!(matches!(
            resolve("agent.planning", None),
            Err(ClientError::Unavailable)
        ));
        assert!(matches!(
            secret_configured("hosted"),
            Err(ClientError::Unavailable)
        ));
        assert!(matches!(
            set_api_key("hosted", "owned fixture".into()),
            Err(ClientError::Unavailable)
        ));
        assert!(matches!(
            remove_api_key("hosted"),
            Err(ClientError::Unavailable)
        ));
    }
}
