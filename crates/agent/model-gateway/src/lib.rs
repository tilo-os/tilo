//! Tilo hosted-model gateway protocol.
//!
//! The API key is accepted only by the per-user gateway and is never part of
//! Settings' CRDT/settings payload or an agent process. Sandboxed callers use
//! one launcher-passed connected Unix descriptor (`TILO_SVC_MODEL-GATEWAY_FD`).

#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::fd::{FromRawFd, RawFd};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thiserror::Error;
#[cfg(unix)]
use zeroize::Zeroizing;

pub mod openai;

pub const SERVICE_NAME: &str = "model-gateway";
// Launcher and runtime-portal uppercase the service name without replacing '-'.
pub const SERVICE_FD_ENV: &str = "TILO_SVC_MODEL-GATEWAY_FD";
pub const RPC_VERSION: u16 = 3;
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

/// A request that can cross the sandbox boundary. `SetApiKey` is only sent by
/// Settings; a server also authenticates the launcher-provided app preamble.
/// Caller-owned settings types cannot serialize it.
#[derive(Deserialize, Serialize)]
#[serde(tag = "verb", rename_all = "snake_case")]
pub enum Request {
    /// Read-only Agent check of the exact selection's local Plan prerequisites.
    /// A positive `configured` response does not establish provider reachability,
    /// accepted credentials, or live local-model availability. Older RPC3
    /// servers reject this additive verb; callers must treat that as unavailable.
    SelectionConfigured {
        profile_id: String,
        model_id: String,
        protocol: agent_provider_config::ProviderProtocol,
    },
    Plan {
        profile_id: String,
        model_id: String,
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
    /// One bounded, single-shot text transformation for tilo-agent: a
    /// system instruction and one input, answered with the model's text. No
    /// tools, no conversation, no provider envelope: the gateway builds the
    /// provider request itself, caps its size, output and deadline, and
    /// returns only the answer text.
    Transform {
        profile_id: String,
        model_id: String,
        instructions: String,
        input: String,
        max_output_tokens: u32,
        timeout_ms: u64,
    },
    /// Fixed-shape, device-local visual analysis for Photos. The gateway
    /// owns the prompt and refuses hosted, unpinned, or non-vision profiles;
    /// a sandboxed caller never obtains direct network access.
    AnalyzeImage {
        profile_id: String,
        model_id: String,
        image: VisionImage,
    },
    /// Fixed, consent-gated face detection and clustering.  The gateway owns
    /// the request schema and admits it only for the signed local face model.
    /// It returns opaque cluster/photo membership; no embedding, crop, face
    /// rectangle, identity name, or raw path can cross this boundary.
    ClusterFaces {
        profile_id: String,
        model_id: String,
        images: Vec<FaceImage>,
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

#[derive(Debug, Deserialize, Serialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured: Option<bool>,
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

/// Ask the isolated model gateway to plan against one configured provider.
///
/// # Errors
///
/// Returns [`ClientError`] when the gateway is unavailable, rejects the
/// request, returns a malformed response, or the provider request fails.
pub fn plan(
    profile_id: &str,
    model_id: &str,
    body: serde_json::Value,
    timeout: Duration,
) -> Result<String, ClientError> {
    let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    let response = request(Request::Plan {
        profile_id: profile_id.to_string(),
        model_id: model_id.to_string(),
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
    profile_id: &str,
    model_id: &str,
    body: serde_json::Value,
    timeout: Duration,
    trace: Option<PlanTrace>,
    on_event: &mut dyn FnMut(&str, &str),
) -> Result<String, ClientError> {
    stream_plan(profile_id, model_id, body, timeout, trace, false, on_event)
}

/// [`plan_streaming`] that also asks for [`EVENT_TOOL_CALL_DELTA`] frames:
/// `on_event` sees both `tool_call_delta` and `tool_call` events. A gateway
/// that predates deltas sends only `tool_call` events.
///
/// # Errors
///
/// As [`plan`].
pub fn plan_streaming_deltas(
    profile_id: &str,
    model_id: &str,
    body: serde_json::Value,
    timeout: Duration,
    trace: Option<PlanTrace>,
    on_event: &mut dyn FnMut(&str, &str),
) -> Result<String, ClientError> {
    stream_plan(profile_id, model_id, body, timeout, trace, true, on_event)
}

#[cfg(unix)]
fn stream_plan(
    profile_id: &str,
    model_id: &str,
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
                profile_id: profile_id.to_string(),
                model_id: model_id.to_string(),
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
    _profile_id: &str,
    _model_id: &str,
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

/// Run one bounded single-shot transformation through the isolated model
/// gateway and return the model's answer text. The gateway enforces the
/// `MAX_TRANSFORM_*` bounds; a request past them is refused, not truncated.
///
/// # Errors
///
/// Returns [`ClientError`] when the gateway is unavailable, refuses the
/// request (including for exceeding a bound), or the provider call fails.
pub fn transform(
    profile_id: &str,
    model_id: &str,
    instructions: &str,
    input: &str,
    max_output_tokens: u32,
    timeout: Duration,
) -> Result<String, ClientError> {
    let timeout_ms = u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX);
    let response = request(Request::Transform {
        profile_id: profile_id.to_string(),
        model_id: model_id.to_string(),
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
/// descriptor. This operation can only be authorized for Photos by the
/// gateway; callers cannot provide a URL, arbitrary prompt, or timeout.
///
/// # Errors
///
/// Returns [`ClientError`] if the capability is not passed by the launcher,
/// the gateway rejects the request, or the provider is unavailable.
pub fn analyze_image(
    profile_id: &str,
    model_id: &str,
    image: VisionImage,
) -> Result<String, ClientError> {
    let response = request(Request::AnalyzeImage {
        profile_id: profile_id.to_string(),
        model_id: model_id.to_string(),
        image,
    })?;
    response
        .output
        .ok_or_else(|| ClientError::Protocol("gateway returned no image analysis".into()))
}

/// Run the dedicated local face model against one bounded batch.  This is not
/// a generic multimodal request and cannot be redirected to a hosted model.
///
/// # Errors
///
/// Returns [`ClientError`] if the launcher gateway is absent, the local model
/// refuses the fixed protocol, or its response is unavailable.
pub fn cluster_faces(
    profile_id: &str,
    model_id: &str,
    images: Vec<FaceImage>,
) -> Result<String, ClientError> {
    let response = request(Request::ClusterFaces {
        profile_id: profile_id.to_string(),
        model_id: model_id.to_string(),
        images,
    })?;
    response
        .output
        .ok_or_else(|| ClientError::Protocol("gateway returned no face clusters".into()))
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
                configured: None,
                error: None,
                event: None,
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

#[cfg(all(test, not(unix)))]
mod unsupported_platform_tests {
    use super::*;

    #[test]
    fn photos_and_secret_operations_require_a_supported_gateway_capability() {
        assert!(!available());
        assert!(matches!(
            analyze_image(
                "local",
                "vision",
                VisionImage {
                    media_type: "image/png".into(),
                    data_base64: String::new(),
                    source_content_hash: String::new(),
                }
            ),
            Err(ClientError::Unavailable)
        ));
        assert!(matches!(
            cluster_faces("local", "faces", Vec::new()),
            Err(ClientError::Unavailable)
        ));
        assert!(matches!(
            plan(
                "hosted",
                "model",
                serde_json::Value::Null,
                Duration::from_secs(1)
            ),
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
