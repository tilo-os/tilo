//! Per-user hosted model gateway.
//!
//! This is a narrow OpenAI-compatible/Anthropic relay for a profile-selected
//! planning call. It does not include a general `LiteLLM` control plane.

use std::fs;
use std::io::{BufRead, Read};
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;
use std::os::fd::FromRawFd;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use accounts::{AccountId, Secret, SecretVault};
use agent_provider_config::{
    EndpointLocation, ProviderConfig, ProviderProfile, ProviderProtocol, ReasoningParam,
};
use base64::Engine as _;
use clap::{Parser, Subcommand};
#[cfg(test)]
use tilo_model_gateway::read_frame;
use tilo_model_gateway::{
    EVENT_TOOL_CALL, EVENT_TOOL_CALL_DELTA, FRAMING, FaceImage, Frame, MAX_DELTA_EVENTS,
    MAX_STREAM_EVENTS, PlanTrace, RPC_VERSION, Request, Response, TraceEvent, VisionImage,
    write_frame,
};
use zeroize::{Zeroize, Zeroizing};

#[derive(Parser)]
#[command(name = "tilo-model-gateway", about = "Tilo hosted model gateway")]
struct Cli {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// systemd-owned service endpoint; not a user-facing command.
    #[command(hide = true)]
    Serve {
        #[arg(long)]
        socket: PathBuf,
    },
    SecretStatus {
        #[arg(long)]
        profile: String,
    },
    RemoveApiKey {
        #[arg(long)]
        profile: String,
    },
}

/// The launcher's handoff: a little-endian `u32` length, then JSON.
const HANDOFF_FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(4096);
const SETTINGS_APP: &str = "so.tilo.settings";
const AGENT_APP: &str = "so.tilo.agent";
const PHOTOS_APP: &str = "so.tilo.photos";
const MAX_VISION_IMAGE_BYTES: usize = 1024 * 1024;
/// Face batches are intentionally small.  A large library is scanned as
/// bounded background batches by Photos, never materialized into one giant
/// biometric request or kept in the gateway's memory.
const MAX_FACE_IMAGES: usize = 12;
const MAX_FACE_IMAGE_BYTES: usize = 128 * 1024;
const VISION_TIMEOUT: Duration = Duration::from_secs(45);
/// A connection must present its launcher handoff promptly. The launcher
/// writes it right after connecting, so only a stuck or foreign client waits.
const HANDOFF_TIMEOUT: Duration = Duration::from_secs(2);
/// Once a request frame has started, the rest of it must arrive within this.
/// There is deliberately no idle limit *between* frames: the agent's channel
/// is one launcher-connected socket for the life of the process, with no way
/// to reconnect, so closing an idle one would silently disable its model.
/// Each connection has its own thread, so an idle one blocks nobody.
const FRAME_TIMEOUT: Duration = Duration::from_secs(10);
/// A client that stops reading its responses cannot hold a thread forever.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// Same-user connections served at once; more are closed on accept.
const MAX_CONNECTIONS: usize = 64;

#[derive(serde::Deserialize)]
struct Handoff {
    protocol: u16,
    app_id: String,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.action {
        Action::Serve { socket } => serve(&socket),
        Action::SecretStatus { profile } => control(Request::SecretStatus {
            profile_id: profile,
        }),
        Action::RemoveApiKey { profile } => control(Request::RemoveApiKey {
            profile_id: profile,
        }),
    };
    if let Err(error) = result {
        // Never interpolate a request or provider response here; either could
        // contain a bearer value or user document content.
        eprintln!("tilo-model-gateway: {error}");
        std::process::exit(1);
    }
}

fn control(request: Request) -> Result<(), String> {
    let response = tilo_model_gateway::request(request).map_err(|error| error.to_string())?;
    if let Some(configured) = response.configured {
        println!("{configured}");
    }
    Ok(())
}

fn serve(socket: &PathBuf) -> Result<(), String> {
    let listener = if let Some(listener) = systemd_listener(socket)? {
        listener
    } else {
        bind_listener(socket)?
    };
    accept_loop(&listener, same_user)
}

/// Serve every accepted connection on its own thread, so one idle, slow or
/// long-running client (a Plan in flight, a stalled handoff) never delays
/// another. `authenticate` is the same-user peer check in production.
fn accept_loop(
    listener: &UnixListener,
    authenticate: fn(&UnixStream) -> bool,
) -> Result<(), String> {
    static ACTIVE: AtomicUsize = AtomicUsize::new(0);
    struct Slot;
    impl Drop for Slot {
        fn drop(&mut self) {
            ACTIVE.fetch_sub(1, Ordering::AcqRel);
        }
    }
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.to_string()),
        };
        if !authenticate(&stream) {
            continue;
        }
        if ACTIVE.fetch_add(1, Ordering::AcqRel) >= MAX_CONNECTIONS {
            ACTIVE.fetch_sub(1, Ordering::AcqRel);
            continue;
        }
        let slot = Slot;
        let spawned = std::thread::Builder::new()
            .name("gateway-client".into())
            .spawn(move || {
                let _slot = slot;
                serve_connection(stream);
            });
        if spawned.is_err() {
            // The closure (and its slot) was dropped with the failed spawn.
            continue;
        }
    }
    Ok(())
}

fn bind_listener(socket: &PathBuf) -> Result<UnixListener, String> {
    if let Some(parent) = socket.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    if socket.exists() {
        let metadata = fs::symlink_metadata(socket).map_err(|error| error.to_string())?;
        if !metadata.file_type().is_socket() {
            return Err("refusing to replace a non-socket gateway path".into());
        }
        fs::remove_file(socket).map_err(|error| error.to_string())?;
    }
    let listener = UnixListener::bind(socket).map_err(|error| error.to_string())?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    Ok(listener)
}

#[allow(
    clippy::disallowed_methods,
    reason = "systemd socket activation: LISTEN_* must not leak to spawned children; cleared once, right after the descriptor is taken at the start of `serve`, before any thread is spawned"
)]
fn systemd_listener(expected: &Path) -> Result<Option<UnixListener>, String> {
    const SYSTEMD_LISTEN_FD: i32 = 3;

    let pid_matches = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|value| value.parse::<u32>().ok())
        == Some(std::process::id());
    if !pid_matches {
        return Ok(None);
    }
    if std::env::var("LISTEN_FDS").as_deref() != Ok("1") {
        return Err("systemd must pass exactly one model-gateway listener".into());
    }
    // SAFETY: fcntl only validates whether fd 3 is open; it consumes no pointer.
    if unsafe { libc::fcntl(SYSTEMD_LISTEN_FD, libc::F_GETFD) } == -1 {
        return Err("systemd model-gateway listener fd is closed".into());
    }
    // SAFETY: systemd's LISTEN_PID/FDS contract transfers ownership of fd 3
    // to this process exactly once.
    let listener = unsafe { UnixListener::from_raw_fd(SYSTEMD_LISTEN_FD) };
    let actual = listener.local_addr().map_err(|error| error.to_string())?;
    if actual.as_pathname() != Some(expected) {
        return Err("systemd model-gateway listener has an unexpected path".into());
    }
    // systemd supplies activation descriptors without CLOEXEC.
    // This daemon can launch request-handling subprocesses; none may inherit
    // the session's authenticated listener or activation metadata.
    if unsafe { libc::fcntl(SYSTEMD_LISTEN_FD, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        return Err("cannot mark systemd model-gateway listener close-on-exec".into());
    }
    // SAFETY: activation is consumed once, before worker threads exist.
    unsafe {
        std::env::remove_var("LISTEN_PID");
        std::env::remove_var("LISTEN_FDS");
        std::env::remove_var("LISTEN_FDNAMES");
    }
    Ok(Some(listener))
}

/// One authenticated connection: the launcher handoff, then framed requests
/// in order. Framing, caller authorization and response shapes are unchanged
/// from the single-threaded server.
fn serve_connection(mut stream: UnixStream) {
    if stream.set_write_timeout(Some(WRITE_TIMEOUT)).is_err()
        || stream.set_read_timeout(Some(HANDOFF_TIMEOUT)).is_err()
    {
        return;
    }
    let Ok(handoff) = read_handoff(&mut stream) else {
        return;
    };
    loop {
        let frame = match read_request_frame(&mut stream) {
            Ok(frame) => frame,
            Err(()) => return,
        };
        let response = match serde_json::from_slice::<Frame<Request>>(&frame) {
            Ok(frame) if frame.version == RPC_VERSION => {
                let mut broken = false;
                let response = handle_streaming(&handoff.app_id, frame.value, &mut |event| {
                    broken = broken || write_frame(&mut stream, event).is_err();
                    !broken
                });
                if broken {
                    return;
                }
                response
            }
            Ok(_) => rejected("unsupported gateway RPC version"),
            // The single-threaded server closed on an undecodable frame too.
            Err(_) => return,
        };
        if write_frame(&mut stream, &response).is_err() {
            return;
        }
    }
}

/// Read one length-prefixed request frame. Waiting for its first byte has no
/// limit (see [`FRAME_TIMEOUT`]); the rest must follow promptly. The buffer
/// is zeroized on drop because a Settings frame can carry an API key.
fn read_request_frame(stream: &mut UnixStream) -> Result<Zeroizing<Vec<u8>>, ()> {
    let mut length = [0_u8; 4];
    stream.set_read_timeout(None).map_err(|_| ())?;
    loop {
        match stream.read(&mut length[..1]) {
            Ok(0) => return Err(()),
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(()),
        }
    }
    stream
        .set_read_timeout(Some(FRAME_TIMEOUT))
        .map_err(|_| ())?;
    stream.read_exact(&mut length[1..]).map_err(|_| ())?;
    let length = FRAMING.decode_prefix(length).map_err(|_| ())?;
    let mut body = Zeroizing::new(vec![0_u8; length]);
    stream.read_exact(&mut body).map_err(|_| ())?;
    Ok(body)
}

fn read_handoff(stream: &mut UnixStream) -> Result<Handoff, String> {
    let body = match HANDOFF_FRAMING.read(stream) {
        Ok(Some(body)) => body,
        Ok(None) => return Err("missing launcher authentication".into()),
        Err(wire_frame::FrameError::TooLarge(_)) => {
            return Err("oversized launcher authentication".into());
        }
        Err(_) => return Err("truncated launcher authentication".into()),
    };
    let handoff: Handoff =
        serde_json::from_slice(&body).map_err(|_| "invalid launcher authentication".to_string())?;
    if handoff.protocol != 1
        || (handoff.app_id != SETTINGS_APP
            && handoff.app_id != AGENT_APP
            && handoff.app_id != PHOTOS_APP)
    {
        return Err("launcher is not authorized for the model gateway".into());
    }
    Ok(handoff)
}

#[cfg(test)]
fn handle(app_id: &str, request: Request) -> Response {
    handle_streaming(app_id, request, &mut |_| true)
}

/// Authorize and serve one request. `emit` writes an intermediate streaming
/// frame and reports whether the client is still there; only a streaming
/// Plan calls it.
fn handle_streaming(
    app_id: &str,
    request: Request,
    emit: &mut dyn FnMut(&Response) -> bool,
) -> Response {
    let result = match request {
        Request::SelectionConfigured {
            profile_id,
            model_id,
            protocol,
        } if app_id == AGENT_APP => {
            selection_configured(&profile_id, &model_id, protocol).map(|()| Response {
                ok: true,
                output: None,
                configured: Some(true),
                error: None,
                event: None,
            })
        }
        Request::Plan {
            profile_id,
            model_id,
            body,
            timeout_ms,
            stream,
            deltas,
            trace,
        } if app_id == AGENT_APP => plan(
            &PlanCall {
                profile_id: &profile_id,
                model_id: &model_id,
                timeout: Duration::from_millis(timeout_ms),
                stream,
                deltas: stream && deltas,
                trace,
            },
            body,
            emit,
        )
        .map(|output| Response {
            ok: true,
            output: Some(output),
            configured: None,
            error: None,
            event: None,
        }),
        Request::Plan { .. } => Err("only tilo-agent may plan through the model gateway".into()),
        Request::Trace {
            trace,
            event,
            at_unix_ms,
        } if app_id == AGENT_APP => {
            log_mark(trace, event, at_unix_ms);
            Ok(Response {
                ok: true,
                output: None,
                configured: None,
                error: None,
                event: None,
            })
        }
        Request::AnalyzeImage {
            profile_id,
            model_id,
            image,
        } if app_id == PHOTOS_APP => {
            analyze_image(&profile_id, &model_id, &image).map(|output| Response {
                ok: true,
                output: Some(output),
                configured: None,
                error: None,
                event: None,
            })
        }
        Request::AnalyzeImage { .. } => {
            Err("only Photos may request fixed local image analysis".into())
        }
        Request::ClusterFaces {
            profile_id,
            model_id,
            images,
        } if app_id == PHOTOS_APP => {
            cluster_faces(&profile_id, &model_id, &images).map(|output| Response {
                ok: true,
                output: Some(output),
                configured: None,
                error: None,
                event: None,
            })
        }
        Request::ClusterFaces { .. } => {
            Err("only Photos may request fixed local face clustering".into())
        }
        Request::SecretStatus { profile_id } if app_id == SETTINGS_APP => {
            secret_status(&profile_id).map(|configured| Response {
                ok: true,
                output: None,
                configured: Some(configured),
                error: None,
                event: None,
            })
        }
        Request::SetApiKey {
            profile_id,
            api_key,
        } if app_id == SETTINGS_APP => set_key(&profile_id, api_key).map(|()| Response {
            ok: true,
            output: None,
            configured: None,
            error: None,
            event: None,
        }),
        Request::RemoveApiKey { profile_id } if app_id == SETTINGS_APP => remove_key(&profile_id)
            .map(|()| Response {
                ok: true,
                output: None,
                configured: None,
                error: None,
                event: None,
            }),
        _ => Err("caller is not authorized for this gateway operation".into()),
    };
    result.unwrap_or_else(|error| rejected(&error))
}

fn rejected(error: &str) -> Response {
    Response {
        ok: false,
        output: None,
        configured: None,
        error: Some(error.into()),
        event: None,
    }
}

fn vault_id(profile_id: &str) -> Result<AccountId, String> {
    // A fixed namespace means unrelated account secrets cannot be addressed.
    if profile_id.is_empty()
        || profile_id.len() > 128
        || !profile_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("invalid provider profile id".into());
    }
    Ok(AccountId::new(format!("agent-provider-{profile_id}")))
}

fn configured_profile(profile_id: &str) -> Result<ProviderProfile, String> {
    let config = ProviderConfig::load();
    if !config.validation_errors().is_empty() {
        return Err("provider configuration is invalid".into());
    }
    config
        .profiles
        .into_iter()
        .find(|profile| profile.id == profile_id && profile.enabled)
        .ok_or_else(|| "provider is not enabled".into())
}

fn secret_status(profile_id: &str) -> Result<bool, String> {
    let _ = configured_profile(profile_id)?;
    Ok(SecretVault::open()
        .get(&vault_id(profile_id)?)
        .is_some_and(|secret| !secret.password.is_empty()))
}

/// Check only prerequisites available locally. No HTTP client is created and
/// no request is sent; local models retain Plan's existing credential rule.
fn selection_configured(
    profile_id: &str,
    model_id: &str,
    protocol: ProviderProtocol,
) -> Result<(), String> {
    let account = vault_id(profile_id)?;
    if model_id.is_empty() || model_id.len() > 256 || model_id.chars().any(char::is_control) {
        return Err("invalid selected model id".into());
    }
    let profile = configured_profile(profile_id)?;
    if profile.protocol != protocol {
        return Err("provider protocol does not match the selected protocol".into());
    }
    if !profile.models.iter().any(|model| model.id == model_id) {
        return Err("model is not configured for this provider".into());
    }
    let endpoint = url::Url::parse(&provider_url(&profile))
        .map_err(|_| "provider endpoint is invalid".to_string())?;
    if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host().is_none() {
        return Err("provider endpoint is invalid".into());
    }
    let secret = SecretVault::open()
        .get(&account)
        .map(|secret| Zeroizing::new(secret.password))
        .filter(|secret| !secret.is_empty())
        .ok_or_else(|| "API key is not configured".to_string())?;
    // Match provider_attempt's x-api-key / bearer_auth header representation.
    // Header parsing is local and its error never includes credential bytes.
    let header = match protocol {
        ProviderProtocol::Anthropic => secret,
        ProviderProtocol::OpenAiCompatible => Zeroizing::new(format!("Bearer {}", secret.as_str())),
    };
    reqwest::header::HeaderValue::from_str(header.as_str())
        .map_err(|_| "API key cannot form a provider request".to_string())?;
    Ok(())
}

/// Vault writes are read-modify-write of one file; connections now run on
/// separate threads, so they are serialized.
static VAULT_WRITES: Mutex<()> = Mutex::new(());

fn set_key(profile_id: &str, api_key: String) -> Result<(), String> {
    let _ = configured_profile(profile_id)?;
    let _serial = VAULT_WRITES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut key = Zeroizing::new(api_key);
    if key.trim().is_empty() || key.len() > 16 * 1024 {
        return Err("API key is empty or too large".into());
    }
    let mut vault = SecretVault::open();
    vault.put(&vault_id(profile_id)?, &Secret::password(key.as_str()));
    key.zeroize();
    vault.save();
    Ok(())
}

fn remove_key(profile_id: &str) -> Result<(), String> {
    let _ = configured_profile(profile_id)?;
    let _serial = VAULT_WRITES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut vault = SecretVault::open();
    vault.remove(&vault_id(profile_id)?);
    vault.save();
    Ok(())
}

const MAX_PROVIDER_TIMEOUT: Duration = Duration::from_secs(60);
// Retry policy remains scoped to the separate Photos operations. Plan must
// never infer permission to replay paid work from an HTTP or transport error.
const RETRY_ATTEMPTS: u32 = 4;
const FIRST_RETRY_WAIT: Duration = Duration::from_millis(500);
const LONGEST_RETRY_WAIT: Duration = Duration::from_secs(4);
const ROOM_TO_ANSWER: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Again {
    Never,
    Soon(Option<Duration>),
}

struct ProviderFailure {
    why: String,
    again: Again,
    /// False only when request construction proves no dispatch occurred.
    outcome_unknown: bool,
    /// The provider's HTTP status, when one was received.
    status: Option<u16>,
}

/// One Plan request's non-content parameters.
struct PlanCall<'a> {
    profile_id: &'a str,
    model_id: &'a str,
    timeout: Duration,
    stream: bool,
    /// Also emit coalesced `tool_call_delta` frames (streaming only).
    deltas: bool,
    trace: Option<PlanTrace>,
}

fn plan(
    call: &PlanCall<'_>,
    body: serde_json::Value,
    emit: &mut dyn FnMut(&Response) -> bool,
) -> Result<String, String> {
    let mut timing = PlanTiming::start();
    let result = plan_dispatch(call, body, emit, &mut timing);
    timing.log(call, result.as_ref().map(|_| ()));
    result
}

fn plan_dispatch(
    call: &PlanCall<'_>,
    body: serde_json::Value,
    emit: &mut dyn FnMut(&Response) -> bool,
    timing: &mut PlanTiming,
) -> Result<String, String> {
    let profile = configured_profile(call.profile_id)?;
    if !profile.models.iter().any(|model| model.id == call.model_id) {
        return Err("model is not configured for this provider".into());
    }
    let mut secret = SecretVault::open()
        .get(&vault_id(call.profile_id)?)
        .map(|secret| Zeroizing::new(secret.password))
        .filter(|secret| !secret.is_empty())
        .ok_or_else(|| "API key is not configured".to_string())?;
    // Only the OpenAI-compatible stream is parsed here; an Anthropic Plan is
    // always answered with one final frame, which streaming clients accept.
    let streaming = call.stream && profile.protocol == ProviderProtocol::OpenAiCompatible;
    timing.stream = streaming;
    let body = outbound_body(&profile, call.model_id, body, streaming)?;
    let endpoint = provider_url(&profile);
    let timeout = call.timeout.min(MAX_PROVIDER_TIMEOUT);
    let client = plan_client()?;
    let budget = timeout
        .checked_sub(timing.started.elapsed())
        .filter(|budget| !budget.is_zero())
        .ok_or_else(|| "provider request timed out".to_string())?;
    let result = (|| {
        let response = send_provider(
            client,
            &profile,
            &endpoint,
            Some(secret.as_str()),
            &body,
            budget,
        )
        .inspect_err(|failure| timing.status = failure.status)?;
        timing.status = Some(response.status().as_u16());
        timing.first_byte = Some(timing.started.elapsed());
        if streaming {
            read_stream(response, emit, timing, call.deltas)
        } else {
            let text = response.text().map_err(|_| ProviderFailure {
                why: "provider response was unreadable".into(),
                again: Again::Never,
                outcome_unknown: true,
                status: None,
            })?;
            timing.observe_body(&text);
            Ok(text)
        }
    })()
    .map_err(|failure| {
        if failure.outcome_unknown {
            // There is no provider acceptance/status or idempotency authority.
            // Keep the error bounded and free of provider bodies or credentials.
            format!(
                "provider outcome is unknown; request was not retried ({})",
                failure.why
            )
        } else {
            failure.why
        }
    });
    secret.zeroize();
    result
}

/// The request actually sent. The gateway owns it: the checked `model_id`
/// replaces whatever model the caller named (the allowlist cannot be
/// bypassed through the body), and for OpenAI-compatible profiles the
/// profile's configured reasoning setting and token-limit field name are
/// applied here, so the model is configured in exactly one place (the
/// profile). Nothing here depends on the endpoint's host.
fn outbound_body(
    profile: &ProviderProfile,
    model_id: &str,
    body: serde_json::Value,
    streaming: bool,
) -> Result<serde_json::Value, String> {
    let serde_json::Value::Object(mut object) = body else {
        return Err("plan body must be a JSON object".into());
    };
    object.insert("model".into(), serde_json::Value::String(model_id.into()));
    match profile.protocol {
        ProviderProtocol::OpenAiCompatible => {
            // The profile names its provider's token-limit field; whichever
            // one the caller sent is moved there. No host is inferred.
            if let Some(param) = profile.token_limit_param {
                let limit = object
                    .remove("max_completion_tokens")
                    .or_else(|| object.remove("max_tokens"));
                object.remove("max_tokens");
                if let Some(limit) = limit {
                    object.insert(param.key().into(), limit);
                }
            }
            // The profile's reasoning setting replaces any the caller sent.
            if let Some(reasoning) = &profile.reasoning {
                object.remove("reasoning_effort");
                object.remove("reasoning");
                let effort = serde_json::Value::String(reasoning.effort.clone());
                match reasoning.param {
                    ReasoningParam::ReasoningEffort => {
                        object.insert("reasoning_effort".into(), effort);
                    }
                    ReasoningParam::ReasoningObject => {
                        object.insert("reasoning".into(), serde_json::json!({ "effort": effort }));
                    }
                    ReasoningParam::Omit => {}
                }
            }
            if streaming {
                object.insert("stream".into(), serde_json::Value::Bool(true));
                object.insert(
                    "stream_options".into(),
                    serde_json::json!({ "include_usage": true }),
                );
            } else {
                object.remove("stream");
                object.remove("stream_options");
            }
            // Last: fields this provider rejects, whoever added them.
            for param in &profile.omit_params {
                object.remove(param.as_str());
            }
        }
        ProviderProtocol::Anthropic => {
            object.remove("stream");
        }
    }
    Ok(serde_json::Value::Object(object))
}

/// One keep-alive HTTP client for every Plan, so consecutive model turns
/// reuse the provider's TLS connection instead of handshaking each time.
fn plan_client() -> Result<&'static reqwest::blocking::Client, String> {
    static CLIENT: OnceLock<Option<reqwest::blocking::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            tilo_net::http::blocking_client()
                .connect_timeout(Duration::from_secs(8))
                // Below common provider keep-alive limits, so a pooled
                // connection is rarely one the provider already closed.
                .pool_idle_timeout(Duration::from_secs(45))
                .tcp_keepalive(Duration::from_secs(30))
                // Neither redirects nor protocol-nack retries establish provider
                // idempotency. One Plan authorizes at most one provider POST.
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()
                .ok()
        })
        .as_ref()
        .ok_or_else(|| "gateway HTTP client is unavailable".to_string())
}

/// The largest provider stream read for one Plan.
const MAX_STREAM_BYTES: u64 = 1024 * 1024;

/// Consume an OpenAI-compatible SSE stream, emitting each tool call as soon
/// as it is complete, and return the assembled non-streaming body.
fn read_stream(
    response: reqwest::blocking::Response,
    emit: &mut dyn FnMut(&Response) -> bool,
    timing: &mut PlanTiming,
    deltas: bool,
) -> Result<String, ProviderFailure> {
    let mut reader = std::io::BufReader::new(response.take(MAX_STREAM_BYTES));
    read_sse(&mut reader, emit, timing, deltas, &mut Instant::now)
}

fn stream_failure(why: &str) -> ProviderFailure {
    ProviderFailure {
        why: why.into(),
        again: Again::Never,
        outcome_unknown: true,
        status: None,
    }
}

/// The SSE loop of [`read_stream`] over any reader, with an injectable clock
/// for the delta coalescing window.
fn read_sse(
    reader: &mut dyn BufRead,
    emit: &mut dyn FnMut(&Response) -> bool,
    timing: &mut PlanTiming,
    deltas: bool,
    clock: &mut dyn FnMut() -> Instant,
) -> Result<String, ProviderFailure> {
    let mut assembly = StreamAssembly::default();
    let mut line = Vec::new();
    let mut emitted = 0_usize;
    let mut coalescer = deltas.then(DeltaCoalescer::default);
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line).map_err(|error| {
            if error.kind() == std::io::ErrorKind::TimedOut
                || error.to_string().contains("timed out")
            {
                stream_failure("provider request timed out")
            } else {
                stream_failure("provider response was unreadable")
            }
        })?;
        if read == 0 {
            break;
        }
        let Some(data) = line.trim_ascii().strip_prefix(b"data:") else {
            continue;
        };
        let data = data.trim_ascii();
        if data == b"[DONE]" {
            break;
        }
        let chunk: serde_json::Value = serde_json::from_slice(data)
            .map_err(|_| stream_failure("provider stream was unreadable"))?;
        // Some providers (OpenRouter) report a failure after a 200 status as
        // an error chunk inside the stream.
        if chunk.get("error").is_some() {
            return Err(stream_failure(&format!(
                "provider stream failed{}",
                provider_error_detail(data)
            )));
        }
        if assembly.push(&chunk) && timing.first_token.is_none() {
            timing.first_token = Some(timing.started.elapsed());
        }
        if assembly.reasoning_seen && timing.first_reasoning.is_none() {
            timing.first_reasoning = Some(timing.started.elapsed());
        }
        let complete = assembly.complete();
        emitted = emit_complete(
            &assembly,
            emitted,
            complete,
            emit,
            timing,
            coalescer.as_mut(),
        )?;
        if let Some(coalescer) = coalescer.as_mut() {
            coalescer.pending(&assembly, emitted, clock(), false, emit)?;
        }
    }
    if assembly.finish.is_none() {
        return Err(stream_failure("provider stream ended early"));
    }
    emit_complete(
        &assembly,
        emitted,
        assembly.calls.len(),
        emit,
        timing,
        coalescer.as_mut(),
    )?;
    timing.observe_usage(assembly.usage.as_ref());
    timing.tool_calls = assembly.calls.len();
    Ok(assembly.into_completion().to_string())
}

/// The smallest gap between two delta frames of one tool call.
const DELTA_WINDOW: Duration = Duration::from_millis(40);
/// Buffered argument bytes that force a delta frame before the window ends.
const DELTA_FLUSH_BYTES: usize = 256;

/// Per-call argument deltas already sent, for `tool_call_delta` frames.
#[derive(Default)]
struct DeltaCoalescer {
    /// `(argument bytes already sent, when the last frame went)` per index.
    sent: Vec<(usize, Option<Instant>)>,
    frames: usize,
}

impl DeltaCoalescer {
    /// Emit coalesced delta frames for the in-progress calls at and after
    /// `from`; `force` flushes every buffered byte (before a `tool_call`).
    fn pending(
        &mut self,
        assembly: &StreamAssembly,
        from: usize,
        now: Instant,
        force: bool,
        emit: &mut dyn FnMut(&Response) -> bool,
    ) -> Result<(), ProviderFailure> {
        for index in from..assembly.calls.len() {
            self.flush(assembly, index, now, force, emit)?;
        }
        Ok(())
    }

    fn flush(
        &mut self,
        assembly: &StreamAssembly,
        index: usize,
        now: Instant,
        force: bool,
        emit: &mut dyn FnMut(&Response) -> bool,
    ) -> Result<(), ProviderFailure> {
        let call = &assembly.calls[index];
        while self.sent.len() <= index {
            self.sent.push((0, None));
        }
        let (sent, last) = self.sent[index];
        // `arguments` only grows by whole provider strings, so both offsets
        // are character boundaries.
        let buffered = call.arguments.len().saturating_sub(sent);
        if buffered == 0 || self.frames >= MAX_DELTA_EVENTS {
            return Ok(());
        }
        let due = force
            || buffered >= DELTA_FLUSH_BYTES
            || last.is_none_or(|last| now.saturating_duration_since(last) >= DELTA_WINDOW);
        if !due {
            return Ok(());
        }
        let event = Response {
            ok: true,
            output: Some(
                serde_json::json!({
                    "index": index,
                    "id": (!call.id.is_empty()).then_some(call.id.as_str()),
                    "name": call.name,
                    "delta": &call.arguments[sent..],
                })
                .to_string(),
            ),
            configured: None,
            error: None,
            event: Some(EVENT_TOOL_CALL_DELTA.into()),
        };
        self.frames += 1;
        self.sent[index] = (call.arguments.len(), Some(now));
        if emit(&event) {
            Ok(())
        } else {
            Err(stream_failure(
                "the agent left before the provider finished",
            ))
        }
    }
}

fn emit_complete(
    assembly: &StreamAssembly,
    emitted: usize,
    complete: usize,
    emit: &mut dyn FnMut(&Response) -> bool,
    timing: &mut PlanTiming,
    mut coalescer: Option<&mut DeltaCoalescer>,
) -> Result<usize, ProviderFailure> {
    let mut emitted = emitted;
    while emitted < complete {
        let call = &assembly.calls[emitted];
        let now = timing.started.elapsed();
        timing.first_tool_call.get_or_insert(now);
        if call.name == "plan" {
            timing.plan_call.get_or_insert(now);
        }
        if let Some(coalescer) = coalescer.as_deref_mut() {
            // Every argument byte goes out as a delta before its call.
            coalescer.flush(assembly, emitted, Instant::now(), true, emit)?;
        }
        if emitted < MAX_STREAM_EVENTS {
            let event = Response {
                ok: true,
                output: Some(
                    serde_json::json!({
                        "id": call.id,
                        "name": call.name,
                        "arguments": call.arguments,
                    })
                    .to_string(),
                ),
                configured: None,
                error: None,
                event: Some(EVENT_TOOL_CALL.into()),
            };
            if !emit(&event) {
                return Err(stream_failure(
                    "the agent left before the provider finished",
                ));
            }
        }
        emitted += 1;
    }
    Ok(emitted)
}

#[derive(Default)]
struct StreamCall {
    id: String,
    name: String,
    arguments: String,
}

/// Rebuilds a `chat.completion` body from `chat.completion.chunk` deltas.
#[derive(Default)]
struct StreamAssembly {
    id: Option<serde_json::Value>,
    created: Option<serde_json::Value>,
    model: Option<serde_json::Value>,
    content: Option<String>,
    refusal: Option<String>,
    calls: Vec<StreamCall>,
    /// Tool calls at an index below this one are complete.
    frontier: usize,
    finish: Option<String>,
    usage: Option<serde_json::Value>,
    /// Whether a reasoning delta (never forwarded) has been seen.
    reasoning_seen: bool,
}

impl StreamAssembly {
    /// Fold one chunk in; true when it carried output (text or a tool call).
    fn push(&mut self, chunk: &serde_json::Value) -> bool {
        for (field, slot) in [
            ("id", &mut self.id),
            ("created", &mut self.created),
            ("model", &mut self.model),
        ] {
            if slot.is_none() {
                *slot = chunk.get(field).cloned();
            }
        }
        if let Some(usage) = chunk.get("usage").filter(|usage| usage.is_object()) {
            self.usage = Some(usage.clone());
        }
        let Some(choice) = chunk
            .get("choices")
            .and_then(serde_json::Value::as_array)
            .and_then(|choices| {
                choices.iter().find(|choice| {
                    choice
                        .get("index")
                        .and_then(serde_json::Value::as_u64)
                        .unwrap_or(0)
                        == 0
                })
            })
        else {
            return false;
        };
        let mut output = false;
        let delta = choice.get("delta");
        let text = |key: &str| {
            delta
                .and_then(|delta| delta.get(key))
                .and_then(serde_json::Value::as_str)
        };
        if let Some(content) = text("content").filter(|content| !content.is_empty()) {
            self.content.get_or_insert_default().push_str(content);
            output = true;
        }
        // Reasoning text is timed but neither assembled nor forwarded.
        if text("reasoning")
            .or_else(|| text("reasoning_content"))
            .is_some_and(|reasoning| !reasoning.is_empty())
        {
            self.reasoning_seen = true;
        }
        if let Some(refusal) = text("refusal").filter(|refusal| !refusal.is_empty()) {
            self.refusal.get_or_insert_default().push_str(refusal);
            output = true;
        }
        for call in delta
            .and_then(|delta| delta.get("tool_calls"))
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(index) = call
                .get("index")
                .and_then(serde_json::Value::as_u64)
                .and_then(|index| usize::try_from(index).ok())
                .filter(|index| *index < MAX_STREAM_EVENTS)
            else {
                continue;
            };
            while self.calls.len() <= index {
                self.calls.push(StreamCall::default());
            }
            // A delta for a later index means every earlier call is complete.
            self.frontier = self.frontier.max(index);
            let slot = &mut self.calls[index];
            if let Some(id) = call.get("id").and_then(serde_json::Value::as_str) {
                slot.id = id.into();
            }
            let function = call.get("function");
            if let Some(name) = function
                .and_then(|function| function.get("name"))
                .and_then(serde_json::Value::as_str)
            {
                slot.name.push_str(name);
            }
            if let Some(arguments) = function
                .and_then(|function| function.get("arguments"))
                .and_then(serde_json::Value::as_str)
            {
                slot.arguments.push_str(arguments);
            }
            output = true;
        }
        if let Some(finish) = choice
            .get("finish_reason")
            .and_then(serde_json::Value::as_str)
        {
            self.finish = Some(finish.into());
        }
        output
    }

    /// How many leading tool calls are complete.
    fn complete(&self) -> usize {
        if self.finish.is_some() {
            self.calls.len()
        } else {
            self.frontier.min(self.calls.len())
        }
    }

    fn into_completion(self) -> serde_json::Value {
        let mut message = serde_json::json!({
            "role": "assistant",
            "content": self.content,
        });
        if let Some(refusal) = self.refusal {
            message["refusal"] = serde_json::Value::String(refusal);
        }
        if !self.calls.is_empty() {
            message["tool_calls"] = self
                .calls
                .into_iter()
                .map(|call| {
                    serde_json::json!({
                        "id": call.id,
                        "type": "function",
                        "function": { "name": call.name, "arguments": call.arguments },
                    })
                })
                .collect();
        }
        serde_json::json!({
            "id": self.id,
            "object": "chat.completion",
            "created": self.created,
            "model": self.model,
            "choices": [{ "index": 0, "message": message, "finish_reason": self.finish }],
            "usage": self.usage,
        })
    }
}

/// Timing for one Plan, logged as one content-free line on stderr.
struct PlanTiming {
    started: Instant,
    start_unix_ms: u128,
    stream: bool,
    status: Option<u16>,
    first_byte: Option<Duration>,
    first_token: Option<Duration>,
    first_reasoning: Option<Duration>,
    first_tool_call: Option<Duration>,
    plan_call: Option<Duration>,
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    /// Prompt tokens the provider served from its prompt cache.
    cached_tokens: Option<u64>,
    tool_calls: usize,
}

impl PlanTiming {
    fn start() -> Self {
        Self {
            started: Instant::now(),
            start_unix_ms: unix_ms(),
            stream: false,
            status: None,
            first_byte: None,
            first_token: None,
            first_reasoning: None,
            first_tool_call: None,
            plan_call: None,
            prompt_tokens: None,
            completion_tokens: None,
            reasoning_tokens: None,
            cached_tokens: None,
            tool_calls: 0,
        }
    }

    /// Token counts and tool-call count from a non-streaming body.
    fn observe_body(&mut self, text: &str) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
            return;
        };
        self.observe_usage(value.get("usage"));
        let openai = value
            .pointer("/choices/0/message/tool_calls")
            .and_then(serde_json::Value::as_array);
        let anthropic = value.get("content").and_then(serde_json::Value::as_array);
        self.tool_calls = match (openai, anthropic) {
            (Some(calls), _) => calls.len(),
            (None, Some(blocks)) => blocks
                .iter()
                .filter(|block| {
                    block.get("type").and_then(serde_json::Value::as_str) == Some("tool_use")
                })
                .count(),
            _ => 0,
        };
    }

    fn observe_usage(&mut self, usage: Option<&serde_json::Value>) {
        let Some(usage) = usage else { return };
        let number = |pointer: &str| usage.pointer(pointer).and_then(serde_json::Value::as_u64);
        self.prompt_tokens = number("/prompt_tokens").or_else(|| number("/input_tokens"));
        self.completion_tokens = number("/completion_tokens").or_else(|| number("/output_tokens"));
        self.reasoning_tokens = number("/completion_tokens_details/reasoning_tokens");
        // OpenAI-compatible (Cerebras, OpenAI) and Anthropic spellings.
        self.cached_tokens = number("/prompt_tokens_details/cached_tokens")
            .or_else(|| number("/cache_read_input_tokens"));
    }

    fn log(&self, call: &PlanCall<'_>, outcome: Result<(), &String>) {
        if std::env::var_os("TILO_MODEL_GATEWAY_TIMING").is_some_and(|value| value == "0") {
            return;
        }
        let ms = |value: Option<Duration>| {
            value.map_or_else(|| "-".to_string(), |value| value.as_millis().to_string())
        };
        let count = |value: Option<u64>| value.map_or_else(|| "-".to_string(), |v| v.to_string());
        let trace = call.trace.map_or_else(
            || "-".to_string(),
            |trace| format!("{:016x}/{}", trace.id, trace.turn),
        );
        eprintln!(
            "tilo-model-gateway: timing plan trace={trace} profile={} model={} stream={} status={} \
             start_unix_ms={} first_byte_ms={} first_reasoning_ms={} first_token_ms={} first_tool_call_ms={} \
             plan_call_ms={} done_ms={} prompt_tokens={} completion_tokens={} \
             reasoning_tokens={} cached_tokens={} tool_calls={} outcome={}",
            log_token(call.profile_id),
            log_token(call.model_id),
            self.stream,
            self.status
                .map_or_else(|| "-".to_string(), |status| status.to_string()),
            self.start_unix_ms,
            ms(self.first_byte),
            ms(self.first_reasoning),
            ms(self.first_token),
            ms(self.first_tool_call),
            ms(self.plan_call),
            self.started.elapsed().as_millis(),
            count(self.prompt_tokens),
            count(self.completion_tokens),
            count(self.reasoning_tokens),
            count(self.cached_tokens),
            self.tool_calls,
            if outcome.is_ok() { "ok" } else { "error" },
        );
    }
}

/// Log an agent mark with the gateway's clock, next to its Plan lines.
fn log_mark(trace: PlanTrace, event: TraceEvent, at_unix_ms: Option<u64>) {
    if std::env::var_os("TILO_MODEL_GATEWAY_TIMING").is_some_and(|value| value == "0") {
        return;
    }
    let event = match event {
        TraceEvent::PlanReady => "plan_ready",
        TraceEvent::PlanFailed => "plan_failed",
        TraceEvent::ActionSubmitted => "action_submitted",
        TraceEvent::ActionSettled => "action_settled",
    };
    // A held mark carries the time it happened; how late it arrived is
    // logged apart so a timeline uses the true instant.
    let now = unix_ms();
    let at = at_unix_ms.map_or(now, u128::from).min(now);
    eprintln!(
        "tilo-model-gateway: timing mark trace={:016x}/{} event={event} unix_ms={at} delivered_late_ms={}",
        trace.id,
        trace.turn,
        now - at
    );
}

fn unix_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis())
}

/// A configuration identifier, reduced to a log-safe token.
fn log_token(value: &str) -> String {
    if !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        value.into()
    } else {
        "?".into()
    }
}

/// Run Photos' one fixed local-only visual-analysis operation. No arbitrary
/// prompt crosses the app boundary: the schema, token budget, and image URL
/// shape are all constructed here after validating the launcher-authenticated
/// caller and model capability.
fn analyze_image(profile_id: &str, model_id: &str, image: &VisionImage) -> Result<String, String> {
    validate_vision_image(image)?;
    let profile = configured_profile(profile_id)?;
    if profile.location != EndpointLocation::ThisDevice
        || profile.protocol != ProviderProtocol::OpenAiCompatible
        || !loopback_http_endpoint(&profile.endpoint)
    {
        return Err("image analysis requires a supervised loopback OpenAI-compatible model".into());
    }
    let model = profile
        .models
        .iter()
        .find(|model| model.id == model_id)
        .ok_or_else(|| "model is not configured for this provider".to_string())?;
    if !model.vision {
        return Err("the selected local model does not declare image-analysis capability".into());
    }
    if !model.revision.as_deref().is_some_and(is_pinned_revision) {
        return Err("the selected local vision model has no pinned revision".into());
    }

    let endpoint = provider_url(&profile);
    let body = local_vision_body(model_id, image);
    let client = tilo_net::http::blocking_client()
        .connect_timeout(Duration::from_secs(8))
        .build()
        .map_err(|_| "gateway HTTP client is unavailable".to_string())?;
    let began = Instant::now();
    // A local vLLM endpoint is normally unauthenticated. Do not invent a
    // bearer token, and never look up a hosted-provider secret for it.
    while_busy(
        || VISION_TIMEOUT.checked_sub(began.elapsed()),
        std::thread::sleep,
        |budget| provider_attempt(&client, &profile, &endpoint, None, &body, budget),
    )
}

/// Run one fixed face-clustering request against the separate signed local
/// capability.  An OpenAI-compatible local runner can expose its face model
/// behind the normal chat endpoint, but it must explicitly advertise
/// `face_recognition`; generic image description models cannot use this path.
fn cluster_faces(profile_id: &str, model_id: &str, images: &[FaceImage]) -> Result<String, String> {
    validate_face_images(images)?;
    let profile = configured_profile(profile_id)?;
    if profile.location != EndpointLocation::ThisDevice
        || profile.protocol != ProviderProtocol::OpenAiCompatible
        || !loopback_http_endpoint(&profile.endpoint)
    {
        return Err("face clustering requires a supervised loopback local model".into());
    }
    let model = profile
        .models
        .iter()
        .find(|model| model.id == model_id)
        .ok_or_else(|| "model is not configured for this provider".to_string())?;
    if !model.face_recognition {
        return Err("the selected local model does not declare face-recognition capability".into());
    }
    if !model.revision.as_deref().is_some_and(is_pinned_revision) {
        return Err("the selected local face model has no pinned revision".into());
    }
    let endpoint = provider_url(&profile);
    let body = local_face_body(model_id, images);
    let client = tilo_net::http::blocking_client()
        .connect_timeout(Duration::from_secs(8))
        .build()
        .map_err(|_| "gateway HTTP client is unavailable".to_string())?;
    let began = Instant::now();
    while_busy(
        || VISION_TIMEOUT.checked_sub(began.elapsed()),
        std::thread::sleep,
        |budget| provider_attempt(&client, &profile, &endpoint, None, &body, budget),
    )
}

fn validate_vision_image(image: &VisionImage) -> Result<(), String> {
    if !matches!(
        image.media_type.as_str(),
        "image/jpeg" | "image/png" | "image/webp"
    ) {
        return Err("image analysis accepts JPEG, PNG, or WebP previews only".into());
    }
    if image.source_content_hash.len() != 64
        || !image
            .source_content_hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err("image analysis requires a SHA-256 source-content hash".into());
    }
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&image.data_base64)
        .map_err(|_| "image analysis preview is not valid base64".to_string())?;
    if decoded.is_empty() || decoded.len() > MAX_VISION_IMAGE_BYTES {
        return Err("image analysis preview exceeds the 1 MiB input limit".into());
    }
    Ok(())
}

fn validate_face_images(images: &[FaceImage]) -> Result<(), String> {
    if images.is_empty() || images.len() > MAX_FACE_IMAGES {
        return Err("face clustering accepts 1 through 12 bounded photographs".into());
    }
    let mut ids = std::collections::BTreeSet::new();
    for image in images {
        if !image.photo_id.starts_with("photos.photo:")
            || image.photo_id.len() > 4096
            || image.photo_id.chars().any(char::is_control)
            || !ids.insert(&image.photo_id)
        {
            return Err("face clustering requires unique Photos resource ids".into());
        }
        if image.media_type != "image/jpeg"
            || image.source_content_hash.len() != 64
            || !image
                .source_content_hash
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            return Err("face clustering requires hashed JPEG previews".into());
        }
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&image.data_base64)
            .map_err(|_| "face preview is not valid base64".to_string())?;
        if decoded.is_empty() || decoded.len() > MAX_FACE_IMAGE_BYTES {
            return Err("face preview exceeds the 128 KiB input limit".into());
        }
    }
    Ok(())
}

fn loopback_http_endpoint(endpoint: &str) -> bool {
    let Ok(url) = url::Url::parse(endpoint) else {
        return false;
    };
    if !matches!(url.scheme(), "http" | "https") || url.username() != "" || url.password().is_some()
    {
        return false;
    }
    match url.host() {
        Some(url::Host::Domain("localhost")) => true,
        Some(url::Host::Ipv4(address)) => address.is_loopback(),
        Some(url::Host::Ipv6(address)) => address.is_loopback(),
        _ => false,
    }
}

fn is_pinned_revision(revision: &str) -> bool {
    matches!(revision.len(), 40 | 64)
        && revision
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && revision.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn local_vision_body(model_id: &str, image: &VisionImage) -> serde_json::Value {
    const INSTRUCTIONS: &str = "You are a device-local image indexing component. Return JSON only, with exactly these keys: description (a concise factual caption, at most 480 Unicode characters), labels (an array of up to 24 common visible object, animal, place, or activity labels; do not guess identity or sensitive traits), pets (an array of up to 24 visible companion-animal labels, or an empty array), ocr (literal visible text, or an empty string). A pet must be visibly present; do not infer one from context. Do not name people: face identity requires a separate consented face-clustering capability. If uncertain, omit the label or use an empty value.";
    let data_url = format!("data:{};base64,{}", image.media_type, image.data_base64);
    serde_json::json!({
        "model": model_id,
        "temperature": 0,
        "max_tokens": 700,
        "response_format": { "type": "json_object" },
        "messages": [
            { "role": "system", "content": INSTRUCTIONS },
            { "role": "user", "content": [
                { "type": "text", "text": "Index this photo using the fixed JSON schema." },
                { "type": "image_url", "image_url": { "url": data_url, "detail": "low" } }
            ] }
        ]
    })
}

fn local_face_body(model_id: &str, images: &[FaceImage]) -> serde_json::Value {
    const INSTRUCTIONS: &str = "You are a device-local face detection and clustering component. Return JSON only with exactly one key, clusters: an array of objects with id and photo_ids. Detect visible faces and group only faces that you can determine are the same person among the supplied photographs. Never output a person's name, age, gender, race, emotion, face rectangle, crop, confidence, embedding, or any biometric value. A cluster id must be a short opaque token and photo_ids must contain only supplied ids. Omit uncertain matches and omit single-photo clusters.";
    let mut content = Vec::with_capacity(images.len() + 1);
    content.push(serde_json::json!({
        "type": "text",
        "text": "Detect and cluster faces in this bounded local batch using the fixed JSON schema."
    }));
    for image in images {
        content.push(serde_json::json!({
            "type": "text",
            "text": format!("Photo resource id: {}", image.photo_id),
        }));
        content.push(serde_json::json!({
            "type": "image_url",
            "image_url": { "url": format!("data:{};base64,{}", image.media_type, image.data_base64), "detail": "low" }
        }));
    }
    serde_json::json!({
        "model": model_id,
        "temperature": 0,
        "max_tokens": 700,
        "response_format": { "type": "json_object" },
        "messages": [
            { "role": "system", "content": INSTRUCTIONS },
            { "role": "user", "content": content }
        ]
    })
}

fn provider_attempt(
    client: &reqwest::blocking::Client,
    profile: &ProviderProfile,
    endpoint: &str,
    secret: Option<&str>,
    body: &serde_json::Value,
    budget: Duration,
) -> Result<String, ProviderFailure> {
    send_provider(client, profile, endpoint, secret, body, budget)?
        .text()
        .map_err(|_| ProviderFailure {
            why: "provider response was unreadable".into(),
            again: Again::Never,
            outcome_unknown: true,
            status: None,
        })
}

/// Send one provider POST and return the successful response unread.
fn send_provider(
    client: &reqwest::blocking::Client,
    profile: &ProviderProfile,
    endpoint: &str,
    secret: Option<&str>,
    body: &serde_json::Value,
    budget: Duration,
) -> Result<reqwest::blocking::Response, ProviderFailure> {
    let mut request = client
        .post(endpoint)
        .timeout(budget)
        .header("content-type", "application/json");
    match profile.protocol {
        ProviderProtocol::Anthropic => {
            let Some(secret) = secret else {
                return Err(ProviderFailure {
                    why: "Anthropic requests require a configured credential".into(),
                    again: Again::Never,
                    outcome_unknown: false,
                    status: None,
                });
            };
            request = request
                .header("anthropic-version", "2023-06-01")
                .header("x-api-key", secret);
        }
        ProviderProtocol::OpenAiCompatible => {
            if let Some(secret) = secret {
                request = request.bearer_auth(secret);
            }
        }
    }
    let response = request.json(body).send().map_err(|error| ProviderFailure {
        // A builder error occurs before any network send. Once dispatch starts,
        // even connect/timeout errors cannot grant permission to replay Plan.
        outcome_unknown: !error.is_builder(),
        why: if error.is_timeout() {
            "provider request timed out".into()
        } else {
            "provider transport failed".into()
        },
        again: if error.is_timeout() || error.is_connect() {
            Again::Soon(None)
        } else {
            Again::Never
        },
        status: None,
    })?;
    let status = response.status();
    if !status.is_success() {
        let again = again_after(
            status.as_u16(),
            response
                .headers()
                .get(reqwest::header::RETRY_AFTER)
                .and_then(|value| value.to_str().ok()),
        );
        // A bounded, numeric Retry-After is passed on so the caller can see
        // when a rate-limited provider will take work again; Plan itself
        // never replays.
        let retry_after = match again {
            Again::Soon(Some(wait)) if wait <= Duration::from_secs(3600) => {
                format!(" retry_after_s={}", wait.as_secs_f32().ceil())
            }
            _ => String::new(),
        };
        let mut detail = Vec::new();
        let _ = response.take(MAX_ERROR_BODY).read_to_end(&mut detail);
        return Err(ProviderFailure {
            why: format!(
                "provider returned HTTP {}{}{retry_after}",
                status.as_u16(),
                provider_error_detail(&detail)
            ),
            again,
            outcome_unknown: true,
            status: Some(status.as_u16()),
        });
    }
    Ok(response)
}

/// The most of a provider error body read to classify it.
const MAX_ERROR_BODY: u64 = 16 * 1024;

/// The provider's machine-readable error classification, and nothing else:
/// `error.type`, `error.code` and `error.param` (OpenAI) or `error.type`
/// (Anthropic), each only when it is a short identifier. The free-text
/// `message` is never passed on, because it can quote request content, and
/// no header is ever read.
fn provider_error_detail(body: &[u8]) -> String {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return String::new();
    };
    let Some(error) = value.get("error") else {
        return String::new();
    };
    let identifier = |key: &str| {
        error
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|value| {
                !value.is_empty()
                    && value.len() <= 48
                    && value.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric()
                            || matches!(byte, b'_' | b'-' | b'.' | b'[' | b']')
                    })
            })
    };
    let parts = ["type", "code", "param"]
        .into_iter()
        .filter_map(|key| {
            identifier(key)
                .map(str::to_owned)
                .or_else(|| {
                    error
                        .get(key)
                        .and_then(serde_json::Value::as_u64)
                        .map(|code| code.to_string())
                })
                .map(|value| format!("{key}={value}"))
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        String::new()
    } else {
        format!(" ({})", parts.join(" "))
    }
}

fn again_after(status: u16, retry_after: Option<&str>) -> Again {
    let soon = || Again::Soon(retry_after.and_then(after_seconds));
    match status {
        408 | 429 | 500 | 502 | 503 | 504 | 529 => soon(),
        _ => Again::Never,
    }
}

fn after_seconds(value: &str) -> Option<Duration> {
    Duration::try_from_secs_f64(value.trim().parse::<f64>().ok()?).ok()
}

fn retry_wait(retry: u32, asked_for: Option<Duration>) -> Duration {
    if let Some(wait) = asked_for {
        return wait;
    }
    let doubling = 1u32
        .checked_shl(retry.saturating_sub(1))
        .unwrap_or(u32::MAX);
    FIRST_RETRY_WAIT
        .checked_mul(doubling)
        .unwrap_or(LONGEST_RETRY_WAIT)
        .min(LONGEST_RETRY_WAIT)
}

fn while_busy<L, W, A>(left: L, mut wait: W, mut attempt: A) -> Result<String, String>
where
    L: Fn() -> Option<Duration>,
    W: FnMut(Duration),
    A: FnMut(Duration) -> Result<String, ProviderFailure>,
{
    let began = left();
    let elapsed = |now: Option<Duration>| match (began, now) {
        (Some(start), Some(remaining)) => start.saturating_sub(remaining),
        (Some(start), None) => start,
        _ => Duration::ZERO,
    };
    let mut made = 0_u32;
    loop {
        let Some(budget) = left().filter(|budget| !budget.is_zero()) else {
            return Err("provider request timed out".into());
        };
        made = made.saturating_add(1);
        let ProviderFailure { why, again, .. } = match attempt(budget) {
            Ok(output) => return Ok(output),
            Err(failure) => failure,
        };
        let Again::Soon(asked_for) = again else {
            return Err(why);
        };
        let give_up = || {
            if made == 1 {
                why.clone()
            } else {
                format!(
                    "{why} ({made} attempts over {:.1}s)",
                    elapsed(left()).as_secs_f32()
                )
            }
        };
        if made >= RETRY_ATTEMPTS {
            return Err(give_up());
        }
        let waiting = retry_wait(made, asked_for);
        if left().is_none_or(|remaining| remaining < waiting.saturating_add(ROOM_TO_ANSWER)) {
            return Err(give_up());
        }
        wait(waiting);
    }
}

fn provider_url(profile: &ProviderProfile) -> String {
    let base = profile.endpoint.trim_end_matches('/');
    match profile.protocol {
        ProviderProtocol::Anthropic if base.ends_with("/v1/messages") => base.into(),
        ProviderProtocol::Anthropic => format!("{base}/v1/messages"),
        ProviderProtocol::OpenAiCompatible if base.ends_with("/chat/completions") => base.into(),
        ProviderProtocol::OpenAiCompatible => format!("{base}/chat/completions"),
    }
}

#[cfg(target_os = "linux")]
fn same_user(stream: &UnixStream) -> bool {
    let mut credentials = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let Ok(mut length) = libc::socklen_t::try_from(std::mem::size_of::<libc::ucred>()) else {
        return false;
    };
    // SAFETY: getsockopt writes into a correctly sized ucred buffer.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &raw mut length,
        )
    };
    result == 0
        && usize::try_from(length).ok() == Some(std::mem::size_of::<libc::ucred>())
        && unsafe { credentials.assume_init().uid } == unsafe { libc::geteuid() }
}

// Peer authentication is implemented only for the native Linux gateway.
// Other hosts may exercise provider logic but must not accept service clients.
#[cfg(not(target_os = "linux"))]
fn same_user(_stream: &UnixStream) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn cached_prompt_tokens_are_read_from_either_usage_spelling() {
        let mut timing = PlanTiming::start();
        timing.observe_usage(Some(&serde_json::json!({
            "prompt_tokens": 12000, "completion_tokens": 40,
            "prompt_tokens_details": {"cached_tokens": 11520}
        })));
        assert_eq!(timing.cached_tokens, Some(11520));
        timing.observe_usage(Some(&serde_json::json!({
            "input_tokens": 900, "output_tokens": 5, "cache_read_input_tokens": 800
        })));
        assert_eq!(timing.cached_tokens, Some(800));
        timing.observe_usage(Some(&serde_json::json!({"prompt_tokens": 1})));
        assert_eq!(timing.cached_tokens, None);
    }

    fn vision_image() -> VisionImage {
        VisionImage {
            media_type: "image/jpeg".into(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(b"preview"),
            source_content_hash: "a".repeat(64),
        }
    }

    /// The launcher's handoff: a little-endian `u32` length, then JSON.
    #[test]
    fn launcher_handoff_bytes_are_pinned() {
        use std::io::Write as _;
        let (mut launcher, mut gateway) = UnixStream::pair().unwrap();
        launcher.write_all(&[39, 0, 0, 0]).unwrap();
        launcher
            .write_all(br#"{"protocol":1,"app_id":"so.tilo.agent"}"#)
            .unwrap();
        assert_eq!(read_handoff(&mut gateway).unwrap().app_id, AGENT_APP);
    }

    #[test]
    fn local_vision_input_is_closed_and_bounded() {
        assert!(validate_vision_image(&vision_image()).is_ok());
        let mut foreign = vision_image();
        foreign.media_type = "image/svg+xml".into();
        assert!(validate_vision_image(&foreign).is_err());
        let mut corrupt = vision_image();
        corrupt.data_base64 = "not base64".into();
        assert!(validate_vision_image(&corrupt).is_err());
        let mut bad_hash = vision_image();
        bad_hash.source_content_hash = "x".repeat(64);
        assert!(validate_vision_image(&bad_hash).is_err());
    }

    #[test]
    fn local_vision_never_routes_to_a_lan_or_hosted_endpoint() {
        assert!(loopback_http_endpoint("http://127.0.0.1:8123/v1"));
        assert!(loopback_http_endpoint("http://[::1]:8123/v1"));
        assert!(!loopback_http_endpoint("http://192.168.1.20:8123/v1"));
        assert!(!loopback_http_endpoint("https://models.example.test/v1"));
        assert!(!loopback_http_endpoint(
            "http://localhost.evil.test:8123/v1"
        ));
    }

    #[test]
    fn local_vision_requires_an_immutable_model_revision() {
        assert!(is_pinned_revision(&"a".repeat(40)));
        assert!(is_pinned_revision(&"b".repeat(64)));
        assert!(!is_pinned_revision("main"));
        assert!(!is_pinned_revision(&"A".repeat(40)));
    }

    #[test]
    fn local_vision_prompt_is_fixed_and_contains_no_identity_request() {
        let body = local_vision_body("vision-model", &vision_image());
        let encoded = body.to_string();
        assert!(encoded.contains("device-local image indexing component"));
        assert!(encoded.contains("Do not name people"));
        assert!(!encoded.contains("ignore previous"));
        assert_eq!(body["model"], "vision-model");
    }

    #[test]
    fn local_face_input_is_bounded_and_prompt_never_requests_identity() {
        let face = FaceImage {
            photo_id: "photos.photo:one".into(),
            media_type: "image/jpeg".into(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(b"preview"),
            source_content_hash: "a".repeat(64),
        };
        assert!(validate_face_images(&[face.clone()]).is_ok());
        let mut duplicate = face.clone();
        duplicate.photo_id = face.photo_id.clone();
        assert!(validate_face_images(&[face.clone(), duplicate]).is_err());
        let body = local_face_body("face-model", &[face]);
        let encoded = body.to_string();
        assert!(encoded.contains("face detection and clustering"));
        assert!(encoded.contains("Never output a person's name"));
        assert!(!encoded.contains("identify the person"));
    }

    struct Fake {
        left: Duration,
        waits: Vec<Duration>,
        calls: usize,
    }

    fn run_retries(
        left: Duration,
        outcomes: &[Result<&str, (u16, Option<&str>)>],
    ) -> (Result<String, String>, Fake) {
        let fake = RefCell::new(Fake {
            left,
            waits: Vec::new(),
            calls: 0,
        });
        let result = while_busy(
            || {
                let fake = fake.borrow();
                (!fake.left.is_zero()).then_some(fake.left)
            },
            |duration| {
                let mut fake = fake.borrow_mut();
                fake.waits.push(duration);
                fake.left = fake.left.saturating_sub(duration);
            },
            |_budget| {
                let mut fake = fake.borrow_mut();
                let index = fake.calls.min(outcomes.len().saturating_sub(1));
                fake.calls = fake.calls.saturating_add(1);
                fake.left = fake.left.saturating_sub(Duration::from_millis(200));
                match outcomes[index] {
                    Ok(output) => Ok(output.to_string()),
                    Err((status, retry_after)) => Err(ProviderFailure {
                        why: format!("provider returned HTTP {status}"),
                        again: again_after(status, retry_after),
                        outcome_unknown: true,
                        status: Some(status),
                    }),
                }
            },
        );
        (result, fake.into_inner())
    }

    #[test]
    fn photos_retry_policy_retries_transient_provider_failures() {
        let (result, fake) = run_retries(
            Duration::from_secs(20),
            &[Err((529, None)), Err((503, None)), Ok("answer")],
        );
        assert_eq!(result.as_deref(), Ok("answer"));
        assert_eq!(fake.calls, 3);
        assert_eq!(
            fake.waits,
            [Duration::from_millis(500), Duration::from_secs(1)]
        );
    }

    #[test]
    fn settled_provider_failures_are_not_retried() {
        for status in [400, 401, 403, 404, 422] {
            let (result, fake) = run_retries(Duration::from_secs(20), &[Err((status, None))]);
            assert!(result.unwrap_err().contains(&status.to_string()));
            assert_eq!(fake.calls, 1, "HTTP {status}");
            assert!(fake.waits.is_empty(), "HTTP {status}");
        }
    }

    #[test]
    fn retry_after_and_transient_statuses_are_classified_on_the_http_side() {
        for status in [408, 429, 500, 502, 503, 504, 529] {
            assert_eq!(again_after(status, None), Again::Soon(None));
        }
        assert_eq!(
            again_after(429, Some("0.25")),
            Again::Soon(Some(Duration::from_millis(250)))
        );
        assert_eq!(again_after(401, Some("1")), Again::Never);
    }

    fn hosted_openai() -> ProviderProfile {
        let mut profile = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
        profile.endpoint = "https://models.example.test/api/v1".into();
        profile.models = vec![agent_provider_config::ModelDescriptor::new(
            "vendor/model-a",
        )];
        profile
    }

    fn reasoning(
        param: ReasoningParam,
        effort: &str,
    ) -> Option<agent_provider_config::ReasoningSetting> {
        Some(agent_provider_config::ReasoningSetting {
            param,
            effort: effort.into(),
        })
    }

    #[test]
    fn outbound_body_is_owned_by_the_gateway() {
        let caller = serde_json::json!({
            "model": "other/model", "max_completion_tokens": 64, "stream": true,
            "stream_options": {"include_usage": true}, "reasoning_effort": "high", "messages": []
        });
        // An unconfigured profile: only the model, and the caller's stream, change.
        let body =
            outbound_body(&hosted_openai(), "vendor/model-a", caller.clone(), false).unwrap();
        assert_eq!(body["model"], "vendor/model-a");
        assert_eq!(body["max_completion_tokens"], 64);
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body.get("stream").is_none() && body.get("stream_options").is_none());

        let streamed =
            outbound_body(&hosted_openai(), "vendor/model-a", caller.clone(), true).unwrap();
        assert_eq!(streamed["stream"], true);
        assert_eq!(streamed["stream_options"]["include_usage"], true);

        // OpenRouter-style: `max_tokens` and the reasoning object.
        let mut router = hosted_openai();
        router.token_limit_param = Some(agent_provider_config::TokenLimitParam::MaxTokens);
        router.reasoning = reasoning(ReasoningParam::ReasoningObject, "low");
        let body = outbound_body(&router, "vendor/model-a", caller.clone(), false).unwrap();
        assert_eq!(body["max_tokens"], 64);
        assert!(body.get("max_completion_tokens").is_none());
        assert_eq!(body["reasoning"], serde_json::json!({"effort": "low"}));
        assert!(body.get("reasoning_effort").is_none());

        // OpenAI-style: `max_completion_tokens` and top-level effort.
        let mut openai = hosted_openai();
        openai.token_limit_param =
            Some(agent_provider_config::TokenLimitParam::MaxCompletionTokens);
        openai.reasoning = reasoning(ReasoningParam::ReasoningEffort, "none");
        let legacy = serde_json::json!({"max_tokens": 9, "reasoning": {"effort": "high"}});
        let body = outbound_body(&openai, "vendor/model-a", legacy, false).unwrap();
        assert_eq!(body["max_completion_tokens"], 9);
        assert!(body.get("max_tokens").is_none() && body.get("reasoning").is_none());
        assert_eq!(body["reasoning_effort"], "none");

        // Cerebras-style: fields the provider rejects are stripped last,
        // including the gateway's own stream_options.
        let mut strict = hosted_openai();
        strict.omit_params = vec!["stream_options".into(), "parallel_tool_calls".into()];
        let with_extras = serde_json::json!({"parallel_tool_calls": false, "messages": []});
        let body = outbound_body(&strict, "vendor/model-a", with_extras, true).unwrap();
        assert_eq!(body["stream"], true);
        assert!(body.get("stream_options").is_none() && body.get("parallel_tool_calls").is_none());

        // `none` strips every reasoning field.
        let mut omit = hosted_openai();
        omit.reasoning = reasoning(ReasoningParam::Omit, "");
        let body = outbound_body(&omit, "vendor/model-a", caller, false).unwrap();
        assert!(body.get("reasoning_effort").is_none() && body.get("reasoning").is_none());

        let mut anthropic = ProviderProfile::hosted(ProviderProtocol::Anthropic);
        anthropic.models = vec![agent_provider_config::ModelDescriptor::new("claude-x")];
        anthropic.reasoning = reasoning(ReasoningParam::ReasoningEffort, "low");
        anthropic.token_limit_param =
            Some(agent_provider_config::TokenLimitParam::MaxCompletionTokens);
        let body = outbound_body(
            &anthropic,
            "claude-x",
            serde_json::json!({"model": "other", "max_tokens": 9, "stream": true}),
            true,
        )
        .unwrap();
        assert_eq!(body["model"], "claude-x");
        assert_eq!(body["max_tokens"], 9);
        assert!(body.get("stream").is_none() && body.get("reasoning_effort").is_none());

        assert!(
            outbound_body(
                &hosted_openai(),
                "vendor/model-a",
                serde_json::json!([]),
                false
            )
            .is_err()
        );
    }

    #[test]
    fn provider_error_detail_passes_identifiers_never_messages() {
        let openai = br#"{"error":{"message":"secret document text","type":"invalid_request_error","param":"messages[2].content","code":"unsupported_value"}}"#;
        assert_eq!(
            provider_error_detail(openai),
            " (type=invalid_request_error code=unsupported_value param=messages[2].content)"
        );
        let anthropic =
            br#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#;
        assert_eq!(provider_error_detail(anthropic), " (type=overloaded_error)");
        let router = br#"{"error":{"code":429,"message":"Rate limited upstream"}}"#;
        assert_eq!(provider_error_detail(router), " (code=429)");
        let hostile =
            br#"{"error":{"type":"a b","code":"x\ny","param":"Authorization: Bearer k"}}"#;
        assert_eq!(provider_error_detail(hostile), "");
        let long = format!(r#"{{"error":{{"code":"{}"}}}}"#, "x".repeat(49));
        assert_eq!(provider_error_detail(long.as_bytes()), "");
        assert_eq!(provider_error_detail(b"<html>bad gateway</html>"), "");
        assert_eq!(provider_error_detail(b""), "");
    }

    #[test]
    fn stream_assembly_completes_tool_calls_in_order() {
        let mut assembly = StreamAssembly::default();
        let chunk = |value: serde_json::Value| value;
        assert!(!assembly.push(&chunk(serde_json::json!({"id": "c", "choices": [{"index": 0, "delta": {"role": "assistant"}}]}))));
        assert!(assembly.push(&chunk(serde_json::json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "id": "a", "function": {"name": "list_", "arguments": "{\"x\""}}]}}]}))));
        assert_eq!(assembly.complete(), 0);
        assembly.push(&chunk(serde_json::json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 0, "function": {"name": "operations", "arguments": ":1}"}}]}}]})));
        assert_eq!(assembly.complete(), 0);
        assembly.push(&chunk(serde_json::json!({"choices": [{"index": 0, "delta": {"tool_calls": [{"index": 1, "id": "b", "function": {"name": "plan", "arguments": "{}"}}]}}]})));
        assert_eq!(
            assembly.complete(),
            1,
            "a later index completes the earlier call"
        );
        assembly.push(&chunk(serde_json::json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]})));
        assert_eq!(assembly.complete(), 2);
        assembly.push(&chunk(
            serde_json::json!({"choices": [], "usage": {"completion_tokens": 3}}),
        ));
        let completion = assembly.into_completion();
        assert_eq!(completion["id"], "c");
        let calls = &completion["choices"][0]["message"]["tool_calls"];
        assert_eq!(calls[0]["function"]["name"], "list_operations");
        assert_eq!(calls[0]["function"]["arguments"], "{\"x\":1}");
        assert_eq!(calls[1]["id"], "b");
        assert_eq!(
            completion["choices"][0]["message"]["content"],
            serde_json::Value::Null
        );
        assert_eq!(completion["usage"]["completion_tokens"], 3);

        let mut thinking = StreamAssembly::default();
        assert!(
            !thinking.push(
                &serde_json::json!({"choices": [{"index": 0, "delta": {"reasoning": "hm"}}]})
            )
        );
        assert!(thinking.reasoning_seen);
        assert!(!thinking.into_completion().to_string().contains("hm"));

        let mut text = StreamAssembly::default();
        text.push(&serde_json::json!({"choices": [{"index": 0, "delta": {"content": "he"}}]}));
        text.push(&serde_json::json!({"choices": [{"index": 0, "delta": {"content": "llo"}, "finish_reason": "stop"}]}));
        let completion = text.into_completion();
        assert_eq!(completion["choices"][0]["message"]["content"], "hello");
        assert!(
            completion["choices"][0]["message"]
                .get("tool_calls")
                .is_none()
        );
    }

    /// A client over the real accept loop (peer check stubbed: macOS has no
    /// SO_PEERCRED, and the check itself is unchanged).
    fn serve_test_socket() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("gw-{}-{nonce}.sock", std::process::id()));
        let listener = UnixListener::bind(&path).unwrap();
        std::thread::spawn(move || accept_loop(&listener, |_| true));
        path
    }

    fn settings_client(path: &Path) -> UnixStream {
        let mut stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write_frame(
            &mut stream,
            &serde_json::json!({"protocol": 1, "app_id": AGENT_APP}),
        )
        .unwrap();
        stream
    }

    fn unauthorized_round_trip(stream: &mut UnixStream) -> Duration {
        let began = Instant::now();
        // Answered without configuration or network: Agent may not read secrets.
        write_frame(
            stream,
            &serde_json::json!({"version": RPC_VERSION, "verb": "secret_status", "profile_id": "p"}),
        )
        .unwrap();
        let response: Response = tilo_model_gateway::read_frame(stream).unwrap();
        assert!(!response.ok);
        assert_eq!(
            response.error.as_deref(),
            Some("caller is not authorized for this gateway operation")
        );
        began.elapsed()
    }

    #[test]
    fn one_idle_or_stalled_client_never_blocks_another() {
        let path = serve_test_socket();
        // A connection that never sends its handoff, and one that sent it and
        // then went idle (the agent's normal, process-long channel).
        let mut silent = UnixStream::connect(&path).unwrap();
        let mut idle = settings_client(&path);
        std::thread::sleep(Duration::from_millis(100));
        let mut active = settings_client(&path);
        for _ in 0..3 {
            assert!(unauthorized_round_trip(&mut active) < Duration::from_secs(1));
        }
        // The stalled handoff is closed after HANDOFF_TIMEOUT...
        silent
            .set_read_timeout(Some(HANDOFF_TIMEOUT + Duration::from_secs(3)))
            .unwrap();
        let began = Instant::now();
        let mut byte = [0_u8; 1];
        assert_eq!(silent.read(&mut byte).unwrap(), 0, "closed, not answered");
        assert!(began.elapsed() < HANDOFF_TIMEOUT + Duration::from_secs(2));
        // ...while the idle authenticated channel stays usable indefinitely.
        std::thread::sleep(Duration::from_millis(200));
        assert!(unauthorized_round_trip(&mut idle) < Duration::from_secs(1));
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn retries_never_consume_room_needed_for_an_answer() {
        let (result, fake) = run_retries(Duration::from_secs(3), &[Err((529, Some("1")))]);
        assert!(result.is_err());
        assert_eq!(fake.calls, 1);
        assert!(fake.waits.is_empty());
    }
}

#[cfg(test)]
mod plan_tests;

#[cfg(test)]
mod selection_tests;
