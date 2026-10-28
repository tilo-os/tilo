//! Model RPC over capabilities supplied by an authenticated host.

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

#[cfg(test)]
mod error_tests;
pub mod failure;
#[cfg(feature = "runtime-client")]
pub mod runtime_client;
pub mod transport;
pub mod validation;

// Launcher and runtime-portal uppercase the service name without replacing '-'.
pub const SERVICE_FD_ENV: &str = "TILO_SVC_MODEL-GATEWAY_FD";
/// Stable custody namespace used by origin-bound provider credentials.
pub const SERVICE_NAME: &str = "model-gateway";
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

pub use agent_provider_config::is_supervised_loopback;

/// Whether `revision` names an immutable model revision: a full lowercase
/// SHA-1 or SHA-256 commit id, never a branch or tag.
#[must_use]
pub fn is_pinned_revision(revision: &str) -> bool {
    matches!(revision.len(), 40 | 64)
        && revision
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

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
    /// Negotiate reusable, per-operation cancellation before starting work.
    /// Older gateways reject this verb; clients must not start paid work then.
    CancellationSupported,
    /// Native connection-local operation identity. Not an authentication claim.
    Cancellable {
        id: u64,
        request: Box<Request>,
    },
    /// Cancel only the matching operation on this authenticated connection.
    /// The acknowledgement is a synchronization barrier, even after completion.
    Cancel {
        id: u64,
    },
    /// Read-only Agent check of the exact selection's local Plan prerequisites.
    /// A positive `configured` response does not establish provider reachability,
    /// accepted credentials, or live local-model availability. Older RPC3
    /// servers reject this additive verb; callers must treat that as unavailable.
    SelectionConfigured {
        profile_id: String,
        model_id: String,
        protocol: agent_provider_config::ProviderProtocol,
    },
    /// Provider-neutral completion. Credentials and endpoints are resolved
    /// exclusively by the trusted gateway.
    Complete {
        profile_id: String,
        model_id: String,
        request: agent_provider_config::protocol::ModelRequest,
        timeout_ms: u64,
        #[serde(default)]
        stream: bool,
    },
    /// Resolve the deliberate enabled default once, when this inference starts.
    CompleteDefault {
        request: agent_provider_config::protocol::ModelRequest,
        timeout_ms: u64,
        #[serde(default)]
        stream: bool,
    },
    /// Read non-secret provider descriptions with their CAS revision.
    GetProviderConfig,
    SetProviderConfig {
        expected_revision: String,
        config: agent_provider_config::ProviderConfig,
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
    /// Find the subject of one bounded picture with the on-device
    /// segmentation model the person installed (Settings › Models). The
    /// gateway runs the model itself, on the CPU; no model file, path or
    /// network reaches the caller, and nothing leaves the device. Answers a
    /// [`SubjectMatte`] as JSON. Older servers reject this verb; callers
    /// treat that as no model.
    SegmentSubject {
        image: VisionImage,
    },
    SecretStatus {
        profile_id: String,
    },
    /// Detailed status, including unbound keys that require explicit migration.
    CredentialStatus {
        profile_id: String,
    },
    /// The models a provider offers, asked with its stored key or sign-in:
    /// Settings' model list and connection test. Answers the models as JSON
    /// (`ModelDescriptor`s); a failure names its kind, never what the
    /// provider wrote. Older gateways reject this verb.
    ListModels {
        profile_id: String,
    },
    /// Confirm the original endpoint independently of editable profile details.
    MigrateApiKey {
        profile_id: String,
        original_endpoint: String,
    },
    /// Delete only the selected legacy entry, never a bound replacement.
    RemoveLegacyApiKey {
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

/// How much of each pixel of a [`Request::SegmentSubject`] picture is its
/// subject, and what found it.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct SubjectMatte {
    /// The matte's size: the picture's.
    pub width: u32,
    pub height: u32,
    /// Standard base64 of a greyscale PNG, white where the subject is.
    pub matte_png_base64: String,
    /// The catalogue model and its pinned revision, for provenance.
    pub model_id: String,
    pub revision: String,
    /// Milliseconds spent loading the model (0 when it was loaded already)
    /// and running it.
    pub load_ms: u64,
    pub infer_ms: u64,
}

/// The message the gateway refuses [`Request::SegmentSubject`] with when no
/// segmentation model is installed, so a caller can offer to set one up.
pub const NO_SEGMENTATION_MODEL: &str = "no on-device subject model is installed";

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

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Response {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub configured: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// Additive RPC3 diagnostic. Older peers ignore this field; absent on older gateways.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_error: Option<agent_provider_config::protocol::ModelError>,
    /// Set only on intermediate streaming frames (see [`EVENT_TOOL_CALL`]).
    /// A response without an event is the final frame of a request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
}

impl Response {
    /// Construct a redacted failure while retaining its machine-readable kind.
    pub fn failure(mut error: agent_provider_config::protocol::ModelError) -> Self {
        error.message = agent_provider_config::protocol::ModelError::new(error.kind).message;
        Self {
            ok: false,
            output: None,
            configured: None,
            error: Some(error.message.clone()),
            model_error: Some(error),
            event: None,
        }
    }

    /// Typed errors take precedence. English matching is only for old gateways.
    pub fn failure_error(&self) -> ClientError {
        match &self.model_error {
            Some(error) => {
                ClientError::Model(agent_provider_config::protocol::ModelError::new(error.kind))
            }
            None => ClientError::Rejected(self.error.clone().unwrap_or_default()),
        }
    }

    pub fn into_result(self) -> Result<Self, ClientError> {
        if self.ok {
            Ok(self)
        } else {
            Err(self.failure_error())
        }
    }
}

#[derive(Error)]
pub enum ClientError {
    #[error("Tilo model gateway is unavailable")]
    Unavailable,
    #[error("{0}")]
    Model(agent_provider_config::protocol::ModelError),
    #[error("model gateway protocol error")]
    Protocol(String),
    #[error("model gateway refused request: {}", self.model_error())]
    Rejected(String),
}

impl std::fmt::Debug for ClientError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Display::fmt(self, formatter)
    }
}

impl ClientError {
    /// A safe, actionable diagnostic for neutral streaming consumers.
    pub fn model_error(&self) -> agent_provider_config::protocol::ModelError {
        use agent_provider_config::protocol::{ErrorKind, ModelError};
        let kind = match self {
            Self::Model(error) => return error.clone(),
            Self::Unavailable => ErrorKind::Unavailable,
            Self::Protocol(_) => ErrorKind::InvalidResponse,
            Self::Rejected(detail) => {
                // RPC3 predates typed errors. Classify only at this compatibility
                // boundary; never repeat the gateway's diagnostic to a frontend.
                let detail = detail.to_ascii_lowercase();
                let patterns: &[(ErrorKind, &[&str])] = &[
                    (
                        ErrorKind::CreditsExhausted,
                        &["credits", "quota", "subscription_required"],
                    ),
                    (
                        ErrorKind::ContextLimit,
                        &["context limit", "context_length"],
                    ),
                    (ErrorKind::Authentication, &["authentication", "api key"]),
                    (ErrorKind::PermissionDenied, &["permission denied"]),
                    (ErrorKind::RateLimited, &["rate limit"]),
                    (ErrorKind::TimedOut, &["timed out"]),
                    (ErrorKind::Cancelled, &["cancelled"]),
                    (
                        ErrorKind::Truncated,
                        &["truncated", "without a final response"],
                    ),
                    (
                        ErrorKind::Transport,
                        &["network", "transport failed", "stream interrupted"],
                    ),
                    (
                        ErrorKind::Refused,
                        &[
                            "model declined",
                            "model refused",
                            "provider refusal",
                            "content filter",
                        ],
                    ),
                    (ErrorKind::InvalidRequest, &["unsupported image media type"]),
                    (
                        ErrorKind::InvalidResponse,
                        &["malformed", "unreadable", "unsupported finish reason"],
                    ),
                ];
                patterns
                    .iter()
                    .find(|(_, phrases)| phrases.iter().any(|phrase| detail.contains(phrase)))
                    .map_or(ErrorKind::Provider, |(kind, _)| *kind)
            }
        };
        ModelError::new(kind)
    }
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
    let turn = ChannelTurn::take()?;
    let response = exchange(request);
    drop(turn);
    response
}

/// [`request`], on this thread's turn of the channel.
#[cfg(unix)]
fn exchange(request: Request) -> Result<Response, ClientError> {
    let mut stream = service_stream()?;
    write_frame(
        &mut stream,
        &Frame {
            version: RPC_VERSION,
            value: request,
        },
    )?;
    let response: Response = read_frame(&mut stream)?;
    response.into_result()
}

/// The process's one exchange on the launcher channel.
///
/// Every RPC duplicates the same connected socket, so the duplicates share
/// one byte stream, and the gateway answers a connection's requests in the
/// order they came. Two threads talking at once (the agent's transforms
/// beside its planning turn, two agents of one host) would read each
/// other's frames: one caller handed another's answer, or a frame split
/// between two readers. The gateway serves one request at a time anyway,
/// so taking turns costs no throughput.
#[cfg(unix)]
static CHANNEL: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(unix)]
thread_local! {
    /// Whether this thread is in an exchange: a call from inside a stream's
    /// event callback would otherwise wait on itself forever.
    static IN_EXCHANGE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// This thread's turn on the launcher channel, until dropped.
#[cfg(unix)]
struct ChannelTurn {
    _held: std::sync::MutexGuard<'static, ()>,
}

#[cfg(unix)]
impl ChannelTurn {
    /// Wait for the channel.
    ///
    /// # Errors
    ///
    /// [`ClientError::Protocol`] when this thread is already in an exchange
    /// (a call from a stream's event callback): its frames would be read as
    /// the stream's.
    fn take() -> Result<Self, ClientError> {
        if IN_EXCHANGE.get() {
            return Err(ClientError::Protocol(
                "a gateway call was made while this thread's call was in progress".into(),
            ));
        }
        // The guard protects no data: a panic mid-exchange leaves nothing
        // to repair, so a poisoned lock is taken as it is.
        let held = CHANNEL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        IN_EXCHANGE.set(true);
        Ok(Self { _held: held })
    }
}

#[cfg(unix)]
impl Drop for ChannelTurn {
    fn drop(&mut self) {
        IN_EXCHANGE.set(false);
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

/// Complete a neutral turn, delivering presentation-only deltas followed by
/// its authoritative response. Deltas never authorize tool execution.
///
/// # Errors
/// Rejects unavailable gateways, provider failures and malformed responses.
/// Malformed event payloads are rejected after draining the response so the
/// shared capability channel remains synchronized.
pub fn complete_streaming(
    profile_id: &str,
    model_id: &str,
    model_request: agent_provider_config::protocol::ModelRequest,
    timeout: Duration,
    on_event: &mut dyn FnMut(&agent_provider_config::protocol::StreamEvent),
) -> Result<agent_provider_config::protocol::ModelResponse, ClientError> {
    complete_stream(
        (profile_id, model_id),
        model_request,
        timeout,
        on_event,
        None,
    )
}

/// [`complete_streaming`] for a caller that may stop wanting the answer:
/// once `stop` says so (it is asked every [`STOP_POLL`] while the turn
/// runs), the gateway is told to stop the provider call, so
/// an abandoned turn is not paid for to its end and no longer holds the
/// process's one gateway channel.
///
/// # Errors
/// As [`complete_streaming`]; a stopped turn fails as
/// [`ErrorKind::Cancelled`](agent_provider_config::protocol::ErrorKind::Cancelled),
/// and a gateway that cannot stop calls refuses the turn before any work.
pub fn complete_streaming_until(
    profile_id: &str,
    model_id: &str,
    model_request: agent_provider_config::protocol::ModelRequest,
    timeout: Duration,
    on_event: &mut dyn FnMut(&agent_provider_config::protocol::StreamEvent),
    stop: &(dyn Fn() -> bool + Sync),
) -> Result<agent_provider_config::protocol::ModelResponse, ClientError> {
    complete_stream(
        (profile_id, model_id),
        model_request,
        timeout,
        on_event,
        Some(stop),
    )
}

fn complete_stream(
    (profile_id, model_id): (&str, &str),
    model_request: agent_provider_config::protocol::ModelRequest,
    timeout: Duration,
    on_event: &mut dyn FnMut(&agent_provider_config::protocol::StreamEvent),
    stop: Option<&(dyn Fn() -> bool + Sync)>,
) -> Result<agent_provider_config::protocol::ModelResponse, ClientError> {
    let request = Request::Complete {
        profile_id: profile_id.into(),
        model_id: model_id.into(),
        request: model_request,
        timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        stream: true,
    };
    let mut invalid = false;
    let output = streaming_request(
        request,
        true,
        &mut |kind, payload| {
            // Drain after a malformed event to preserve the shared legacy channel,
            // but never deliver more deltas from a poisoned turn.
            if invalid {
                return;
            }
            match transport::decode_event(kind, payload) {
                Ok(Some(event)) => on_event(&event),
                Ok(None) => {}
                Err(_) => invalid = true,
            }
        },
        stop,
    );
    let result = if invalid {
        Err(agent_provider_config::protocol::ModelError::invalid(
            "invalid gateway stream event",
        ))
    } else {
        output
            .map_err(|error| error.model_error())
            .and_then(|output| transport::decode_completion(&output))
    };
    transport::finish_stream(result, on_event).map_err(ClientError::Model)
}

/// How often a stoppable stream asks whether its caller still wants it.
pub const STOP_POLL: Duration = Duration::from_millis(100);

#[cfg(not(unix))]
fn streaming_request(
    _request: Request,
    _deltas: bool,
    _on_event: &mut dyn FnMut(&str, &str),
    _stop: Option<&(dyn Fn() -> bool + Sync)>,
) -> Result<String, ClientError> {
    Err(ClientError::Unavailable)
}

#[cfg(unix)]
fn streaming_request(
    request: Request,
    deltas: bool,
    on_event: &mut dyn FnMut(&str, &str),
    stop: Option<&(dyn Fn() -> bool + Sync)>,
) -> Result<String, ClientError> {
    let turn = ChannelTurn::take()?;
    let output = streaming_exchange(request, deltas, on_event, stop);
    drop(turn);
    output
}

/// [`streaming_request`], on this thread's turn of the channel.
#[cfg(unix)]
fn streaming_exchange(
    request: Request,
    deltas: bool,
    on_event: &mut dyn FnMut(&str, &str),
    stop: Option<&(dyn Fn() -> bool + Sync)>,
) -> Result<String, ClientError> {
    let mut stream = service_stream()?;
    let Some(stop) = stop else {
        write_frame(
            &mut stream,
            &Frame {
                version: RPC_VERSION,
                value: request,
            },
        )?;
        return read_stream(&mut stream, deltas, on_event, None);
    };
    let id = cancellable(&mut stream, request)?;
    let writer = stream.try_clone().map_err(|error| io_error(&error))?;
    let watch = StopWatch::default();
    std::thread::scope(|scope| {
        // Without a watcher the turn still runs, only unstoppable between
        // its events.
        let _ = std::thread::Builder::new()
            .name("model-gateway-stop".into())
            .spawn_scoped(scope, || watch.stop_when(stop, id, writer));
        let output = read_stream(&mut stream, deltas, on_event, Some((&watch, id)));
        watch.finish();
        output
    })
}

/// Start `request` as a cancellable operation on `stream`, after the
/// gateway agreed to stop operations: its id.
#[cfg(unix)]
fn cancellable(stream: &mut UnixStream, request: Request) -> Result<u64, ClientError> {
    use agent_provider_config::protocol::{ErrorKind, ModelError};
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    write_frame(
        stream,
        &Frame {
            version: RPC_VERSION,
            value: Request::CancellationSupported,
        },
    )?;
    let supported: Response = read_frame(stream)?;
    if !supported.ok
        || supported.event.is_some()
        || supported.output.as_deref() != Some("cancel-v1")
    {
        return Err(ClientError::Model(ModelError::new(ErrorKind::Unsupported)));
    }
    let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    write_frame(
        stream,
        &Frame {
            version: RPC_VERSION,
            value: Request::Cancellable {
                id,
                request: Box::new(request),
            },
        },
    )?;
    Ok(id)
}

/// Where a stoppable stream is: over (its answer read), or told to stop.
#[cfg(unix)]
#[derive(Default)]
struct StopWatch {
    state: std::sync::Mutex<Stopping>,
    over: std::sync::Condvar,
}

#[cfg(unix)]
#[derive(Default)]
struct Stopping {
    over: bool,
    cancelled: bool,
}

#[cfg(unix)]
impl StopWatch {
    /// Until the stream is over, ask `stop` every [`STOP_POLL`]; once it
    /// says so, tell the gateway to stop operation `id`.
    fn stop_when(&self, stop: &(dyn Fn() -> bool + Sync), id: u64, mut writer: UnixStream) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        while !state.over && !stop() {
            state = self
                .over
                .wait_timeout(state, STOP_POLL)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
        if !state.over {
            // Under the lock: the reader either sees the stream told to
            // stop, or has taken its answer and this is not sent.
            state.cancelled = write_frame(
                &mut writer,
                &Frame {
                    version: RPC_VERSION,
                    value: Request::Cancel { id },
                },
            )
            .is_ok();
        }
        drop(state);
    }

    /// One frame was read; `last` when it is the stream's answer. Whether
    /// the stream was told to stop, so the frame is not the caller's.
    fn read(&self, last: bool) -> bool {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let cancelled = state.cancelled;
        let over = last && !cancelled;
        if over {
            state.over = true;
        }
        drop(state);
        if over {
            self.over.notify_all();
        }
        cancelled
    }

    /// The stream ended, however it did: the watcher stops.
    fn finish(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .over = true;
        self.over.notify_all();
    }
}

/// Read a streamed answer's frames from `stream`, delivering its events. A
/// stoppable stream (`watch`) told to stop reads on, discarding, to the
/// gateway's acknowledgement, so no frame of it is left on the channel.
#[cfg(unix)]
fn read_stream(
    stream: &mut UnixStream,
    deltas: bool,
    on_event: &mut dyn FnMut(&str, &str),
    watch: Option<(&StopWatch, u64)>,
) -> Result<String, ClientError> {
    // A bounded number of intermediate frames: one per completed tool call,
    // and (only when asked for) a separately bounded number of deltas.
    let mut calls = 0_usize;
    let mut delta_frames = 0_usize;
    let mut stopped = 0_usize;
    loop {
        let response: Response = read_frame(stream)?;
        if let Some((watch, id)) = watch
            && watch.read(response.event.is_none())
        {
            if response.event.is_none()
                && response.ok
                && response.output.as_deref() == Some(format!("cancelled:{id}").as_str())
            {
                return Err(ClientError::Model(
                    agent_provider_config::protocol::ModelError::new(
                        agent_provider_config::protocol::ErrorKind::Cancelled,
                    ),
                ));
            }
            stopped = stopped.saturating_add(1);
            if stopped > MAX_STREAM_EVENTS + MAX_DELTA_EVENTS + 1 {
                let _ = stream.shutdown(std::net::Shutdown::Both);
                return Err(ClientError::Protocol(
                    "gateway did not acknowledge the stop".into(),
                ));
            }
            continue;
        }
        let response = response.into_result()?;
        match response.event {
            Some(event) => {
                let count = if deltas
                    && (matches!(
                        event.as_str(),
                        EVENT_TOOL_CALL_DELTA | "text_delta" | "reasoning_delta"
                    )) {
                    delta_frames = delta_frames.saturating_add(1);
                    delta_frames > MAX_DELTA_EVENTS
                } else {
                    calls = calls.saturating_add(1);
                    calls > MAX_STREAM_EVENTS
                };
                if count {
                    // Do not leave unread frames on the shared launcher channel.
                    let _ = stream.shutdown(std::net::Shutdown::Both);
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

/// [`transform`] for a caller that may stop wanting the answer (its
/// invocation was cancelled): once `stop` says so (it is asked every
/// [`STOP_POLL`]), the gateway is told to stop the provider call, which
/// then no longer holds the process's one gateway channel.
///
/// # Errors
/// As [`transform`]; a stopped transform fails as
/// [`ErrorKind::Cancelled`](agent_provider_config::protocol::ErrorKind::Cancelled).
pub fn transform_until(
    (profile_id, model_id): (&str, &str),
    (instructions, input): (&str, &str),
    max_output_tokens: u32,
    timeout: Duration,
    stop: &(dyn Fn() -> bool + Sync),
) -> Result<String, ClientError> {
    streaming_request(
        Request::Transform {
            profile_id: profile_id.to_string(),
            model_id: model_id.to_string(),
            instructions: instructions.to_string(),
            input: input.to_string(),
            max_output_tokens,
            timeout_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
        },
        false,
        &mut |_, _| {},
        Some(stop),
    )
}

/// Request a fixed-schema visual analysis through the launcher-issued gateway
/// descriptor. This operation can only be authorized for Photos by the
/// gateway; callers cannot provide a URL, arbitrary prompt, or timeout.
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

/// Compatibility alias for the shared administration contract.
pub use agent_provider_config::management::CredentialStatus;

/// Query detailed status without treating an unbound key as absent.
pub fn credential_status(profile_id: &str) -> Result<CredentialStatus, ClientError> {
    let response = request(Request::CredentialStatus {
        profile_id: profile_id.into(),
    })?;
    serde_json::from_str(response.output.as_deref().unwrap_or(""))
        .map_err(|_| ClientError::Protocol("gateway returned invalid credential status".into()))
}

/// Adopt an unbound key only after confirming its original endpoint.
pub fn migrate_api_key(profile_id: &str, original_endpoint: &str) -> Result<(), ClientError> {
    request(Request::MigrateApiKey {
        profile_id: profile_id.into(),
        original_endpoint: original_endpoint.into(),
    })?;
    Ok(())
}

/// Delete just the user-selected unbound key, even after editing the endpoint.
pub fn remove_legacy_api_key(profile_id: &str) -> Result<(), ClientError> {
    request(Request::RemoveLegacyApiKey {
        profile_id: profile_id.into(),
    })?;
    Ok(())
}

/// Find the subject of one bounded picture with the installed on-device
/// segmentation model.
///
/// # Errors
///
/// Returns [`ClientError`] if the launcher gateway is absent, no model is
/// installed ([`ClientError::Rejected`] with [`NO_SEGMENTATION_MODEL`]), the
/// picture is refused, or the answer is malformed.
pub fn segment_subject(image: VisionImage) -> Result<SubjectMatte, ClientError> {
    let response = request(Request::SegmentSubject { image })?;
    let output = response
        .output
        .ok_or_else(|| ClientError::Protocol("gateway returned no subject".into()))?;
    serde_json::from_str(&output)
        .map_err(|error| ClientError::Protocol(format!("malformed subject: {error}")))
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
/// too large, and a typed transport/timeout error when writing fails.
#[cfg(unix)]
pub fn write_frame<T: Serialize>(stream: &mut UnixStream, value: &T) -> Result<(), ClientError> {
    let body = Zeroizing::new(
        serde_json::to_vec(value).map_err(|error| ClientError::Protocol(error.to_string()))?,
    );
    FRAMING.write(stream, &body).map_err(|error| match error {
        wire_frame::FrameError::Io(error) => io_error(&error),
        _ => ClientError::Protocol("request exceeds gateway frame limit".into()),
    })?;
    stream.flush().map_err(|error| io_error(&error))
}

#[cfg(unix)]
fn io_error(error: &std::io::Error) -> ClientError {
    use agent_provider_config::protocol::{ErrorKind, ModelError};
    ClientError::Model(ModelError::new(match error.kind() {
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock => ErrorKind::TimedOut,
        _ => ErrorKind::Transport,
    }))
}

/// Read and decode one size-bounded JSON frame from a gateway stream.
///
/// # Errors
///
/// Returns a typed transport/truncation error on stream loss and
/// [`ClientError::Protocol`] for an invalid length or payload.
#[cfg(unix)]
pub fn read_frame<T: for<'a> Deserialize<'a>>(stream: &mut UnixStream) -> Result<T, ClientError> {
    use agent_provider_config::protocol::{ErrorKind, ModelError};
    let mut body = Zeroizing::new(Vec::new());
    match FRAMING.read_into(stream, &mut body) {
        Ok(true) => {}
        Err(wire_frame::FrameError::TooLarge(_)) => {
            return Err(ClientError::Protocol("gateway frame exceeds limit".into()));
        }
        Ok(false) | Err(wire_frame::FrameError::Truncated | wire_frame::FrameError::Closed) => {
            return Err(ClientError::Model(ModelError::new(ErrorKind::Truncated)));
        }
        Err(wire_frame::FrameError::Io(error)) => return Err(io_error(&error)),
        Err(_) => return Err(ClientError::Protocol("invalid gateway frame".into())),
    }
    serde_json::from_slice(&body)
        .map_err(|_| ClientError::Protocol("invalid gateway response".into()))
}

#[cfg(test)]
#[cfg(unix)]
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
                model_error: None,
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

    fn answer(output: &str, event: Option<&str>) -> Response {
        Response {
            ok: true,
            output: Some(output.into()),
            configured: None,
            error: None,
            model_error: None,
            event: event.map(Into::into),
        }
    }

    /// The agent's transforms run beside its streamed planning turn, over
    /// the one launcher channel. A transform asked while the turn's caller
    /// is still busy with one of its events used to read the turn's next
    /// frame as its own answer, and the turn then read the transform's: it
    /// now waits its turn, and each caller gets its own answer.
    #[test]
    fn a_call_beside_a_stream_on_the_shared_channel_gets_its_own_answer() {
        use std::os::fd::AsRawFd as _;
        let (app, mut gateway) = UnixStream::pair().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(SERVICE_FD_ENV, app.as_raw_fd().to_string());
        let (asked_by, asked) = std::sync::mpsc::channel();
        let server = std::thread::spawn(move || {
            // The stream: one event, then its answer, written at once, as a
            // gateway writes them whenever the provider has them.
            let frame: Frame<Request> = read_frame(&mut gateway).unwrap();
            assert!(matches!(frame.value, Request::Plan { .. }));
            write_frame(&mut gateway, &answer("{}", Some(EVENT_TOOL_CALL))).unwrap();
            write_frame(&mut gateway, &answer("the plan", None)).unwrap();
            // Then each transform, answered with its own input.
            while let Ok(frame) = read_frame::<Frame<Request>>(&mut gateway) {
                let Request::Transform { input, .. } = frame.value else {
                    panic!("only a transform follows the stream");
                };
                let _ = asked_by.send(());
                write_frame(&mut gateway, &answer(&input, None)).unwrap();
            }
        });
        let (in_event, event_seen) = std::sync::mpsc::channel();
        let (go_on, go) = std::sync::mpsc::channel::<()>();
        let stream = std::thread::spawn(move || {
            streaming_request(
                Request::Plan {
                    profile_id: "hosted".into(),
                    model_id: "model".into(),
                    body: serde_json::Value::Null,
                    timeout_ms: 1000,
                    stream: true,
                    deltas: false,
                    trace: None,
                },
                false,
                &mut |kind, _| {
                    assert_eq!(kind, EVENT_TOOL_CALL);
                    // The turn's caller is busy with the event (running the
                    // call) while the transform is asked.
                    in_event.send(()).unwrap();
                    let _ = go.recv_timeout(tilo_test_env::hang_bound());
                },
                None,
            )
        });
        tilo_test_env::recv("the stream's event", &event_seen);
        let transform = std::thread::spawn(|| {
            transform(
                "hosted",
                "model",
                "Shorten",
                "the document",
                64,
                Duration::from_secs(1),
            )
        });
        // Long enough for a transform on the channel to have read a frame.
        let _ = asked.recv_timeout(Duration::from_millis(300));
        std::thread::sleep(Duration::from_millis(100));
        go_on.send(()).unwrap();
        assert_eq!(stream.join().unwrap().unwrap(), "the plan");
        assert_eq!(transform.join().unwrap().unwrap(), "the document");
        drop(env);
        drop(app);
        server.join().unwrap();
    }

    /// A gateway that streams one event of a cancellable `Plan`, then
    /// (when `answers_first`) its answer, and acknowledges the stop it is
    /// then sent; afterwards it answers each transform with its input.
    /// Hands back the id the stop named.
    fn stoppable_gateway(
        mut gateway: UnixStream,
        answers_first: bool,
    ) -> std::thread::JoinHandle<u64> {
        std::thread::spawn(move || {
            gateway
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            let frame: Frame<Request> = read_frame(&mut gateway).unwrap();
            assert!(matches!(frame.value, Request::CancellationSupported));
            write_frame(&mut gateway, &answer("cancel-v1", None)).unwrap();
            let frame: Frame<Request> = read_frame(&mut gateway).unwrap();
            let Request::Cancellable { id, request } = frame.value else {
                panic!("a stoppable stream starts a cancellable operation");
            };
            assert!(matches!(*request, Request::Plan { .. }));
            write_frame(&mut gateway, &answer("{}", Some(EVENT_TOOL_CALL))).unwrap();
            if answers_first {
                write_frame(&mut gateway, &answer("the plan", None)).unwrap();
            }
            let frame: Frame<Request> = read_frame(&mut gateway).unwrap();
            let Request::Cancel { id: stopped } = frame.value else {
                panic!("the caller stops the operation");
            };
            if !answers_first {
                // A frame the provider produced before the stop arrived.
                write_frame(&mut gateway, &answer("{}", Some(EVENT_TOOL_CALL))).unwrap();
            }
            write_frame(&mut gateway, &answer(&format!("cancelled:{stopped}"), None)).unwrap();
            assert_eq!(stopped, id);
            while let Ok(frame) = read_frame::<Frame<Request>>(&mut gateway) {
                let Request::Transform { input, .. } = frame.value else {
                    panic!("only a transform follows the stream");
                };
                write_frame(&mut gateway, &answer(&input, None)).unwrap();
            }
            stopped
        })
    }

    /// Ask a stoppable `Plan` that its caller gives up on at its first event,
    /// then a transform on the same channel.
    fn give_up_then_transform() -> (Result<String, ClientError>, Result<String, ClientError>) {
        let stop = std::sync::atomic::AtomicBool::new(false);
        let mut events = 0_usize;
        let plan = streaming_request(
            Request::Plan {
                profile_id: "hosted".into(),
                model_id: "model".into(),
                body: serde_json::Value::Null,
                timeout_ms: 1000,
                stream: true,
                deltas: false,
                trace: None,
            },
            false,
            &mut |_, _| {
                events = events.saturating_add(1);
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                // Long enough for the watcher to see it before the answer
                // is read.
                std::thread::sleep(Duration::from_millis(300));
            },
            Some(&|| stop.load(std::sync::atomic::Ordering::Relaxed)),
        );
        assert_eq!(events, 1, "nothing after the stop reaches the caller");
        let after = transform(
            "hosted",
            "model",
            "Shorten",
            "after the stop",
            64,
            Duration::from_secs(1),
        );
        (plan, after)
    }

    /// The agent lets go of a planning turn when its task is stopped. The
    /// turn used to run to its end at the provider, billed, holding the
    /// channel: the gateway is now told to stop it, the turn ends as
    /// cancelled, and the next call on the channel gets its own answer.
    #[test]
    fn a_stream_its_caller_gives_up_on_is_stopped_at_the_gateway() {
        use std::os::fd::AsRawFd as _;
        let (app, gateway) = UnixStream::pair().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(SERVICE_FD_ENV, app.as_raw_fd().to_string());
        let server = stoppable_gateway(gateway, false);
        let (plan, after) = give_up_then_transform();
        let Err(ClientError::Model(error)) = plan else {
            panic!("a stopped turn is cancelled, not answered: {plan:?}");
        };
        assert_eq!(
            error.kind,
            agent_provider_config::protocol::ErrorKind::Cancelled
        );
        assert_eq!(after.unwrap(), "after the stop");
        drop(env);
        drop(app);
        server.join().unwrap();
    }

    /// The stop can cross the answer: the gateway acknowledges it after
    /// the answer went out, and that acknowledgement is read here, not
    /// left on the channel as the next call's answer.
    #[test]
    fn a_stop_that_crosses_the_answer_leaves_nothing_on_the_channel() {
        use std::os::fd::AsRawFd as _;
        let (app, gateway) = UnixStream::pair().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(SERVICE_FD_ENV, app.as_raw_fd().to_string());
        let server = stoppable_gateway(gateway, true);
        let (plan, after) = give_up_then_transform();
        assert!(
            matches!(&plan, Err(ClientError::Model(error)) if error.kind == agent_provider_config::protocol::ErrorKind::Cancelled),
            "{plan:?}"
        );
        assert_eq!(after.unwrap(), "after the stop");
        drop(env);
        drop(app);
        server.join().unwrap();
    }

    /// A call from inside a stream's event callback, on the stream's own
    /// thread, would read the stream's frames as its answer, and waiting
    /// for the channel would wait on itself: it is refused instead.
    #[test]
    fn a_call_from_inside_a_stream_is_refused_rather_than_waiting_on_itself() {
        use std::os::fd::AsRawFd as _;
        let (app, mut gateway) = UnixStream::pair().unwrap();
        let mut env = tilo_test_env::ScopedEnv::new();
        env.set(SERVICE_FD_ENV, app.as_raw_fd().to_string());
        let server = std::thread::spawn(move || {
            let _: Frame<Request> = read_frame(&mut gateway).unwrap();
            write_frame(&mut gateway, &answer("{}", Some(EVENT_TOOL_CALL))).unwrap();
            write_frame(&mut gateway, &answer("the plan", None)).unwrap();
        });
        let mut nested = None;
        let plan = streaming_request(
            Request::Plan {
                profile_id: "hosted".into(),
                model_id: "model".into(),
                body: serde_json::Value::Null,
                timeout_ms: 1000,
                stream: true,
                deltas: false,
                trace: None,
            },
            false,
            &mut |_, _| {
                nested = Some(request(Request::Trace {
                    trace: PlanTrace { id: 1, turn: 0 },
                    event: TraceEvent::ActionSubmitted,
                    at_unix_ms: None,
                }));
            },
            None,
        );
        assert_eq!(plan.unwrap(), "the plan");
        assert!(matches!(nested, Some(Err(ClientError::Protocol(_)))));
        server.join().unwrap();
        drop(env);
    }
}

#[cfg(test)]
#[cfg(not(unix))]
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

#[cfg(test)]
mod local_model_tests {
    use super::*;

    #[test]
    fn local_vision_requires_an_immutable_model_revision() {
        assert!(is_pinned_revision(&"a".repeat(40)));
        assert!(is_pinned_revision(&"b".repeat(64)));
        assert!(!is_pinned_revision("main"));
        assert!(!is_pinned_revision(&"A".repeat(40)));
        assert!(!is_pinned_revision(&"g".repeat(40)));
    }
}
