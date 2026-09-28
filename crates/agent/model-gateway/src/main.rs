//! Per-user hosted model gateway.
//!
//! This is a narrow OpenAI-compatible/Anthropic relay for a profile-selected
//! planning call. It does not include a general `LiteLLM` control plane.

use std::fs;
#[cfg(test)]
use std::io::{BufRead, Read};
use std::os::fd::FromRawFd;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixListener;
#[cfg(test)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use accounts::{AccountId, Secret, SecretVault};
use agent_provider_config::assign::{self, Inputs, ModelAssignments, Resolution};
use agent_provider_config::tasks::{self, Consumer, TaskRequest};
use agent_provider_config::{
    Credential, EndpointCustody, LocalRuntime, ModelCapability, ModelOptions, ProviderConfig,
    ProviderProfile, ProviderProtocol, ReasoningParam,
};
use base64::Engine as _;
use clap::{Parser, Subcommand};
use std::future::Future;
use tilo_model_gateway::{
    CredentialState, EVENT_TOOL_CALL, EVENT_TOOL_CALL_DELTA, FRAMING, FaceImage, Frame,
    MAX_DELTA_EVENTS, MAX_STREAM_EVENTS, PlanTrace, Principal, ProviderHealth, RPC_VERSION,
    Reachability, Request, Response, Route, TraceEvent, VisionImage,
};
#[cfg(test)]
use tilo_model_gateway::{read_frame, write_frame};
use tokio::io::AsyncReadExt as _;
use tokio::sync::Semaphore;
use zeroize::{Zeroize, Zeroizing};

mod review;

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

const SETTINGS_APP: &str = "so.tilo.settings";
const AGENT_APP: &str = tasks::AGENT_APP;
const PHOTOS_APP: &str = tasks::PHOTOS_APP;
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
/// Each connection is its own task, so an idle one blocks nobody.
const FRAME_TIMEOUT: Duration = Duration::from_secs(10);
/// A client that stops reading its responses cannot hold its slot forever.
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);
/// How many clients the gateway serves at once. Its callers are a few
/// same-user apps; past this, new connections wait in the listen backlog.
const MAX_CLIENTS: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

/// Writes one intermediate streaming frame and reports whether the client is
/// still there.
type Emit<'a> = &'a mut (dyn FnMut(&Response) -> bool + Send);

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
    // Activation is taken over before the runtime starts any thread.
    let listener = if let Some(listener) = systemd_listener(socket)? {
        listener
    } else {
        bind_listener(socket)?
    };
    listener
        .set_nonblocking(true)
        .map_err(|error| error.to_string())?;
    let http = Arc::new(provider_client()?);
    tilo_rt::daemon("model-gateway")
        .run(async move {
            let listener =
                tokio::net::UnixListener::from_std(listener).map_err(|error| error.to_string())?;
            serve_on(listener, http, MAX_CLIENTS, same_user).await
        })
        .map_err(|error| error.to_string())?
}

/// The one keep-alive HTTP client every provider request shares, so
/// connections and TLS sessions to a provider are reused across requests and
/// clients, and consecutive model turns skip the handshake.
fn provider_client() -> Result<reqwest::Client, String> {
    tilo_net::http::client()
        .connect_timeout(CONNECT_TIMEOUT)
        // Below common provider keep-alive limits, so a pooled connection is
        // rarely one the provider already closed.
        .pool_idle_timeout(Duration::from_secs(45))
        .tcp_keepalive(Duration::from_secs(30))
        // Neither redirects nor protocol-nack retries establish provider
        // idempotency. One Plan authorizes at most one provider POST; the
        // Photos operations retry explicitly in `while_busy`.
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
        .map_err(|_| "gateway HTTP client is unavailable".to_string())
}

/// Serve clients concurrently, each as its own task, at most `limit` at
/// once, so one idle, slow or long-running client (a Plan in flight, a
/// stalled handoff) never delays another. `authenticate` is the same-user
/// peer check in production. Returns only when accepting fails.
async fn serve_on(
    listener: tokio::net::UnixListener,
    http: Arc<reqwest::Client>,
    limit: usize,
    authenticate: fn(&tokio::net::UnixStream) -> bool,
) -> Result<(), String> {
    let slots = Arc::new(Semaphore::new(limit.max(1)));
    loop {
        let slot = Arc::clone(&slots)
            .acquire_owned()
            .await
            .map_err(|error| error.to_string())?;
        let stream = match listener.accept().await {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(error.to_string()),
        };
        if !authenticate(&stream) {
            continue;
        }
        let http = Arc::clone(&http);
        tokio::spawn(async move {
            serve_connection(stream, &http).await;
            drop(slot);
        });
    }
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

/// One authenticated connection: the [`Principal`] handoff, then framed
/// requests in order.
async fn serve_connection(mut stream: tokio::net::UnixStream, http: &reqwest::Client) {
    let Ok(Ok(principal)) = tokio::time::timeout(HANDOFF_TIMEOUT, read_handoff(&mut stream)).await
    else {
        return;
    };
    loop {
        let Ok(frame) = read_request_frame(&mut stream).await else {
            return;
        };
        let response = match serde_json::from_slice::<Frame<Request>>(&frame) {
            Ok(frame) if frame.version == RPC_VERSION => {
                match respond(&mut stream, &principal, frame.value, http).await {
                    Some(response) => response,
                    None => return,
                }
            }
            Ok(_) => rejected("unsupported gateway RPC version"),
            // The single-threaded server closed on an undecodable frame too.
            Err(_) => return,
        };
        if send_frame(&mut stream, &response).await.is_err() {
            return;
        }
    }
}

/// Serve one request, writing its intermediate streaming frames to the
/// client while the provider is still answering. `None` when the client
/// stopped reading them.
async fn respond(
    stream: &mut tokio::net::UnixStream,
    principal: &Principal,
    request: Request,
    http: &reqwest::Client,
) -> Option<Response> {
    // Events are bounded by MAX_STREAM_EVENTS + MAX_DELTA_EVENTS per Plan.
    let (events, mut queued) = tokio::sync::mpsc::unbounded_channel::<Zeroizing<Vec<u8>>>();
    let handling = async move {
        let mut emit = |event: &Response| {
            serde_json::to_vec(event).is_ok_and(|body| events.send(Zeroizing::new(body)).is_ok())
        };
        handle_streaming(principal, request, http, &mut emit).await
        // `events` drops here, which ends the writer below.
    };
    let writing = async move {
        while let Some(body) = queued.recv().await {
            if send_body(stream, &body).await.is_err() {
                // Dropping `queued` makes every later emit report the loss.
                return false;
            }
        }
        true
    };
    let (response, written) = tokio::join!(handling, writing);
    written.then_some(response)
}

async fn send_frame<T: serde::Serialize + Sync>(
    stream: &mut tokio::net::UnixStream,
    value: &T,
) -> Result<(), ()> {
    let body = Zeroizing::new(serde_json::to_vec(value).map_err(|_| ())?);
    send_body(stream, &body).await
}

async fn send_body(stream: &mut tokio::net::UnixStream, body: &[u8]) -> Result<(), ()> {
    match tokio::time::timeout(WRITE_TIMEOUT, FRAMING.write_async(stream, body)).await {
        Ok(Ok(())) => Ok(()),
        _ => Err(()),
    }
}

/// Read one length-prefixed request frame. Waiting for its first byte has no
/// limit (see [`FRAME_TIMEOUT`]); the rest must follow promptly. The buffer
/// is zeroized on drop because a Settings frame can carry an API key.
async fn read_request_frame(stream: &mut tokio::net::UnixStream) -> Result<Zeroizing<Vec<u8>>, ()> {
    let mut length = [0_u8; 4];
    loop {
        match stream.read(&mut length[..1]).await {
            Ok(0) => return Err(()),
            Ok(_) => break,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return Err(()),
        }
    }
    let rest = async {
        stream.read_exact(&mut length[1..]).await.map_err(|_| ())?;
        let length = FRAMING.decode_prefix(length).map_err(|_| ())?;
        let mut body = Zeroizing::new(vec![0_u8; length]);
        stream.read_exact(&mut body).await.map_err(|_| ())?;
        Ok(body)
    };
    tokio::time::timeout(FRAME_TIMEOUT, rest)
        .await
        .map_err(|_| ())?
}

/// Read the connection's [`Principal`]. An app principal is written by the
/// launcher before the app holds the descriptor; the session principal can
/// only come from a process that reaches this socket, which no sandbox does.
async fn read_handoff(stream: &mut tokio::net::UnixStream) -> Result<Principal, String> {
    let body = match tilo_model_gateway::HANDOFF_FRAMING.read_async(stream).await {
        Ok(Some(body)) => body,
        Ok(None) => return Err("missing launcher authentication".into()),
        Err(wire_frame::FrameError::TooLarge(_)) => {
            return Err("oversized launcher authentication".into());
        }
        Err(_) => return Err("truncated launcher authentication".into()),
    };
    let principal = Principal::from_handoff(&body).map_err(|error| error.to_string())?;
    match principal.app_id() {
        Some(app) if app != SETTINGS_APP && app != AGENT_APP && app != PHOTOS_APP => {
            Err("app is not authorized for the model gateway".into())
        }
        _ => Ok(principal),
    }
}

#[cfg(test)]
async fn handle(principal: &Principal, request: Request, http: &reqwest::Client) -> Response {
    handle_streaming(principal, request, http, &mut |_| true).await
}

/// A successful final response.
fn succeeded(output: Option<String>, configured: Option<bool>) -> Response {
    Response {
        ok: true,
        output,
        configured,
        ..Response::default()
    }
}

/// Authorize and serve one request. `emit` writes an intermediate streaming
/// frame and reports whether the client is still there; only a streaming
/// Plan calls it. Configuration and vault work, which reads and writes
/// files, runs on the runtime's bounded blocking pool.
async fn handle_streaming(
    principal: &Principal,
    request: Request,
    http: &reqwest::Client,
    emit: Emit<'_>,
) -> Response {
    let app_id = principal.app_id().unwrap_or_default();
    let session = *principal == Principal::Session;
    let output = |output| succeeded(Some(output), None);
    let done = |()| succeeded(None, None);
    let configured = |configured| succeeded(None, Some(configured));
    let consumer = Consumer::from(principal);
    let result = match request {
        Request::Resolve { task, agent } => {
            blocking(move || resolve_for(&consumer, &task, agent.as_deref()))
                .await
                .map(|resolution| Response {
                    ok: true,
                    route: Some(Route::from_resolution(&resolution)),
                    ..Response::default()
                })
        }
        Request::SelectionConfigured {
            profile_id,
            model_id,
            protocol,
        } if app_id == AGENT_APP => {
            blocking(move || selection_configured(&profile_id, &model_id, protocol))
                .await
                .map(|()| configured(true))
        }
        Request::Plan {
            task,
            agent,
            protocol,
            body,
            timeout_ms,
            stream,
            deltas,
            trace,
        } => plan(
            http,
            &PlanCall {
                consumer,
                task: &task,
                agent: agent.as_deref(),
                protocol,
                timeout: Duration::from_millis(timeout_ms),
                stream,
                deltas: stream && deltas,
                trace,
            },
            body,
            emit,
        )
        .await
        .map(output),
        Request::Transform {
            task,
            agent,
            content,
            instructions,
            input,
            max_output_tokens,
            timeout_ms,
        } => transform(
            http,
            consumer,
            task,
            agent,
            TransformAsk {
                content,
                instructions,
                input,
                max_output_tokens,
                timeout: Duration::from_millis(timeout_ms),
            },
        )
        .await
        .map(output),
        Request::Trace {
            trace,
            event,
            at_unix_ms,
        } if app_id == AGENT_APP => {
            log_mark(trace, event, at_unix_ms);
            Ok(done(()))
        }
        Request::AnalyzeImage { image } => analyze_image(http, consumer, image).await.map(output),
        Request::ClusterFaces { images } => cluster_faces(http, consumer, images).await.map(output),
        Request::ReviewTrace { trace } if session => {
            review::review_trace(http, trace).await.map(output)
        }
        Request::ReviewTrace { .. } => {
            Err("only the desktop session may request feedback review".into())
        }
        other if app_id == SETTINGS_APP => handle_settings(other, http).await,
        _ => Err("caller is not authorized for this gateway operation".into()),
    };
    result.unwrap_or_else(|error| rejected(&error))
}

/// The credential verbs only Settings may send: whether a provider's key is
/// stored, storing or removing it, and probing the provider with it.
async fn handle_settings(request: Request, http: &reqwest::Client) -> Result<Response, String> {
    let done = |()| succeeded(None, None);
    match request {
        Request::SecretStatus { profile_id } => blocking(move || secret_status(&profile_id))
            .await
            .map(|configured| succeeded(None, Some(configured))),
        Request::SetApiKey {
            profile_id,
            api_key,
        } => blocking(move || set_key(&profile_id, api_key))
            .await
            .map(done),
        Request::RemoveApiKey { profile_id } => {
            blocking(move || remove_key(&profile_id)).await.map(done)
        }
        Request::Probe { profile_id } => probe(http, profile_id).await.map(|health| Response {
            ok: true,
            health: Some(health),
            ..Response::default()
        }),
        _ => Err("caller is not authorized for this gateway operation".into()),
    }
}

/// Resolve `task` for `consumer` through the person's assignments, with this
/// device's runtime and hardware.
fn resolve_for(consumer: &Consumer, task: &str, agent: Option<&str>) -> Result<Resolution, String> {
    resolve_task(consumer, task, agent).map_err(|error| error.to_string())
}

/// [`resolve_for`], keeping the typed reason.
fn resolve_task(
    consumer: &Consumer,
    task: &str,
    agent: Option<&str>,
) -> Result<Resolution, assign::ResolveError> {
    resolve_task_for(consumer, task, agent, tasks::PrivacyClass::Open)
}

/// [`resolve_task`] for content at least as private as `content`.
fn resolve_task_for(
    consumer: &Consumer,
    task: &str,
    agent: Option<&str>,
    content: tasks::PrivacyClass,
) -> Result<Resolution, assign::ResolveError> {
    let config = ProviderConfig::load();
    let assignments = ModelAssignments::load();
    let local = LocalRuntime::load();
    assign::resolve_for(
        &Inputs {
            config: &config,
            assignments: &assignments,
            hardware: local.hardware.as_ref(),
        },
        consumer,
        task,
        agent,
        content,
    )
}

/// Run synchronous configuration and vault work — file reads, key
/// decryption, saves — on the runtime's bounded blocking pool rather than
/// on a worker that is serving other clients.
async fn blocking<T, F>(work: F) -> Result<T, String>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, String> + Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| "gateway worker failed".to_string())?
}

fn rejected(error: &str) -> Response {
    Response {
        ok: false,
        error: Some(error.into()),
        ..Response::default()
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
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| "provider is not configured".into())
}

fn secret_status(profile_id: &str) -> Result<bool, String> {
    let _ = configured_profile(profile_id)?;
    Ok(SecretVault::open()
        .get(&vault_id(profile_id)?)
        .is_some_and(|secret| !secret.password.is_empty()))
}

/// Check only prerequisites available locally: the profile is configured,
/// valid and enabled, lists the model, speaks `protocol`, and its credential
/// is present and forms the header its dialect sends, by Plan's own
/// credential rule. No HTTP client is created and no request is sent.
fn selection_configured(
    profile_id: &str,
    model_id: &str,
    protocol: ProviderProtocol,
) -> Result<(), String> {
    vault_id(profile_id)?;
    if model_id.is_empty() || model_id.len() > 256 || model_id.chars().any(char::is_control) {
        return Err("invalid selected model id".into());
    }
    let profile = configured_profile(profile_id)?;
    if !profile.enabled {
        return Err("provider is not enabled".into());
    }
    if profile.protocol != protocol {
        return Err("provider protocol does not match the selected protocol".into());
    }
    if profile.model(model_id).is_none() {
        return Err("model is not configured for this provider".into());
    }
    let endpoint = url::Url::parse(&provider_url(&profile))
        .map_err(|_| "provider endpoint is invalid".to_string())?;
    if !matches!(endpoint.scheme(), "http" | "https") || endpoint.host().is_none() {
        return Err("provider endpoint is invalid".into());
    }
    let bearer = credential_for(&profile)?;
    // Match authorize's header representation for each dialect. Header
    // parsing is local and its error never includes credential bytes.
    let header = match (protocol, &bearer) {
        (ProviderProtocol::Anthropic | ProviderProtocol::AzureOpenAi, Bearer::ApiKey(key)) => {
            Zeroizing::new(key.as_str().to_owned())
        }
        (ProviderProtocol::Anthropic, _) => {
            return Err("Anthropic requests require a configured API key".into());
        }
        (
            ProviderProtocol::AzureOpenAi | ProviderProtocol::OpenAiCompatible,
            Bearer::Token(secret),
        )
        | (ProviderProtocol::OpenAiCompatible, Bearer::ApiKey(secret)) => {
            Zeroizing::new(format!("Bearer {}", secret.as_str()))
        }
        (ProviderProtocol::AzureOpenAi, Bearer::None) => {
            return Err("Azure OpenAI requests require a key or a signed-in account".into());
        }
        (ProviderProtocol::OpenAiCompatible, Bearer::None) => return Ok(()),
    };
    reqwest::header::HeaderValue::from_str(header.as_str())
        .map_err(|_| "API key cannot form a provider request".to_string())?;
    Ok(())
}

/// Vault writes are read-modify-write of one file; requests run on separate
/// blocking-pool threads, so they are serialized.
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
    let _serial = VAULT_WRITES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let mut vault = SecretVault::open();
    vault.remove(&vault_id(profile_id)?);
    vault.save();
    Ok(())
}

/// The credential a request to `profile` carries, read from the vault.
enum Bearer {
    None,
    /// An API key the person entered.
    ApiKey(Zeroizing<String>),
    /// An OAuth access token of a signed-in account.
    Token(Zeroizing<String>),
}

impl Bearer {
    fn state(&self) -> CredentialState {
        match self {
            Self::None => CredentialState::NotNeeded,
            Self::ApiKey(_) | Self::Token(_) => CredentialState::Present,
        }
    }
}

/// Read (and, for an account, refresh) `profile`'s credential. Blocking.
fn credential_for(profile: &ProviderProfile) -> Result<Bearer, String> {
    credential_in(&account_custody::Custodian::live(), profile)
}

/// [`credential_for`] over `custodian`'s stores: an API key from the vault,
/// or the access token custody hands out for the signed-in account, which
/// it refreshes and reseals through Tilo's one client registration.
fn credential_in(
    custodian: &account_custody::Custodian,
    profile: &ProviderProfile,
) -> Result<Bearer, String> {
    match &profile.credential {
        None => Ok(Bearer::None),
        Some(Credential::ApiKey { .. }) => custodian
            .config_root()
            .map_or_else(SecretVault::open, SecretVault::open_in)
            .get(&vault_id(&profile.id)?)
            .map(|secret| Zeroizing::new(secret.password))
            .filter(|secret| !secret.is_empty())
            .map(Bearer::ApiKey)
            .ok_or_else(|| "API key is not configured".to_string()),
        Some(Credential::Account { account_id }) => custodian
            .account_access_token(&AccountId::new(account_id.clone()), false)
            .map(Bearer::Token)
            .map_err(|error| error.to_string()),
    }
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
    /// Who asks, as the task registry admits consumers.
    consumer: Consumer,
    task: &'a str,
    /// The agent the caller acts as, for per-agent assignments.
    agent: Option<&'a str>,
    /// The dialect the caller wrote the body in.
    protocol: ProviderProtocol,
    timeout: Duration,
    stream: bool,
    /// Also emit coalesced `tool_call_delta` frames (streaming only).
    deltas: bool,
    trace: Option<PlanTrace>,
}

async fn plan(
    http: &reqwest::Client,
    call: &PlanCall<'_>,
    body: serde_json::Value,
    emit: Emit<'_>,
) -> Result<String, String> {
    let mut timing = PlanTiming::start();
    let result = plan_dispatch(http, call, body, emit, &mut timing).await;
    timing.log(call, result.as_ref().map(|_| ()));
    result
}

/// Resolve a caller-dialect task and read its credential.
fn plan_route(
    consumer: &Consumer,
    task: &str,
    agent: Option<&str>,
    protocol: ProviderProtocol,
) -> Result<(Resolution, Bearer), String> {
    let spec = tasks::task(task).ok_or("unknown model task")?;
    if spec.request != TaskRequest::CallerDialect {
        return Err("this task is served only through its fixed gateway operation".into());
    }
    let resolution = resolve_for(consumer, task, agent)?;
    if resolution.profile.protocol != protocol {
        return Err("the model assignment changed; resolve the task again".into());
    }
    let bearer = credential_for(&resolution.profile)?;
    Ok((resolution, bearer))
}

async fn plan_dispatch(
    http: &reqwest::Client,
    call: &PlanCall<'_>,
    body: serde_json::Value,
    emit: Emit<'_>,
    timing: &mut PlanTiming,
) -> Result<String, String> {
    let (consumer, task, agent) = (
        call.consumer.clone(),
        call.task.to_owned(),
        call.agent.map(str::to_owned),
    );
    let protocol = call.protocol;
    let (resolution, bearer) =
        blocking(move || plan_route(&consumer, &task, agent.as_deref(), protocol)).await?;
    let profile = &resolution.profile;
    timing.route = Some((profile.id.clone(), resolution.model.id.clone()));
    // Only the OpenAI-dialect stream is parsed here; an Anthropic Plan is
    // always answered with one final frame, which streaming clients accept.
    let streaming = call.stream && protocol != ProviderProtocol::Anthropic;
    timing.stream = streaming;
    // The assignment, not the caller, names the model and its options.
    let (body, headers) = outbound_body(
        profile,
        &resolution.model.id,
        resolution.options,
        body,
        streaming,
    )?;
    let http = &profile_client(http, profile)?;
    let endpoint = provider_url(profile);
    let timeout = call.timeout.min(MAX_PROVIDER_TIMEOUT);
    let budget = timeout
        .checked_sub(timing.started.elapsed())
        .filter(|budget| !budget.is_zero())
        .ok_or_else(|| "provider request timed out".to_string())?;
    let result = async {
        let response = send_provider(http, profile, &endpoint, &bearer, &headers, &body, budget)
            .await
            .inspect_err(|failure| timing.status = failure.status)?;
        timing.status = Some(response.status().as_u16());
        timing.first_byte = Some(timing.started.elapsed());
        if streaming {
            read_stream(response, emit, timing, call.deltas).await
        } else {
            let text = response.text().await.map_err(|_| ProviderFailure {
                why: "provider response was unreadable".into(),
                again: Again::Never,
                outcome_unknown: true,
                status: None,
            })?;
            timing.observe_body(&text);
            Ok(text)
        }
    }
    .await
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
    drop(bearer);
    result
}

/// The request actually sent, and the extra headers it needs. The gateway
/// owns it: the resolved `model_id` replaces whatever model the caller named
/// (the assignment cannot be bypassed through the body), the assignment's
/// typed `options` are written in the provider's dialect, and for
/// OpenAI-dialect profiles the profile's token-limit field name, reasoning
/// field and rejected fields are applied here, so each is configured in
/// exactly one place. Nothing here depends on the endpoint's host.
/// A provider request body and the headers its options require.
type OutboundBody = (serde_json::Value, Vec<(&'static str, &'static str)>);

fn outbound_body(
    profile: &ProviderProfile,
    model_id: &str,
    options: ModelOptions,
    mut body: serde_json::Value,
    streaming: bool,
) -> Result<OutboundBody, String> {
    if !body.is_object() {
        return Err("plan body must be a JSON object".into());
    }
    let headers = options.apply(profile.protocol, &mut body);
    let Some(object) = body.as_object_mut() else {
        return Err("plan body must be a JSON object".into());
    };
    object.insert("model".into(), serde_json::Value::String(model_id.into()));
    match profile.protocol {
        ProviderProtocol::OpenAiCompatible | ProviderProtocol::AzureOpenAi => {
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
            // It names the field; the assignment's effort, when it sets one,
            // is the value, and the profile's own effort otherwise.
            if let Some(reasoning) = &profile.reasoning {
                object.remove("reasoning_effort");
                object.remove("reasoning");
                let effort = serde_json::Value::String(
                    options
                        .reasoning_effort
                        .map_or_else(|| reasoning.effort.clone(), |effort| effort.wire().into()),
                );
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
    Ok((body, headers))
}

/// The largest provider stream read for one Plan.
const MAX_STREAM_BYTES: usize = 1024 * 1024;

/// Consume an OpenAI-dialect SSE stream, emitting each tool call as soon
/// as it is complete, and return the assembled non-streaming body.
async fn read_stream(
    mut response: reqwest::Response,
    emit: Emit<'_>,
    timing: &mut PlanTiming,
    deltas: bool,
) -> Result<String, ProviderFailure> {
    let mut sse = SseReader::new(deltas);
    let mut pending = Vec::new();
    let mut read = 0_usize;
    while read < MAX_STREAM_BYTES {
        let chunk = response.chunk().await.map_err(|error| {
            if error.is_timeout() {
                stream_failure("provider request timed out")
            } else {
                stream_failure("provider response was unreadable")
            }
        })?;
        let Some(chunk) = chunk else {
            break;
        };
        let take = chunk.len().min(MAX_STREAM_BYTES.saturating_sub(read));
        read = read.saturating_add(take);
        pending.extend(chunk.iter().take(take));
        let mut consumed = 0_usize;
        for line in pending.split_inclusive(|byte| *byte == b'\n') {
            if line.last() != Some(&b'\n') {
                // A partial line waits for the rest of it.
                break;
            }
            consumed = consumed.saturating_add(line.len());
            if sse.line(line, emit, timing, &mut Instant::now)? == Flow::Done {
                return sse.finish(emit, timing);
            }
        }
        pending.drain(..consumed);
    }
    // A last line without its newline, as a buffered line reader returns it.
    if !pending.is_empty() {
        sse.line(&pending, emit, timing, &mut Instant::now)?;
    }
    sse.finish(emit, timing)
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
#[cfg(test)]
fn read_sse(
    reader: &mut dyn BufRead,
    emit: Emit<'_>,
    timing: &mut PlanTiming,
    deltas: bool,
    clock: &mut dyn FnMut() -> Instant,
) -> Result<String, ProviderFailure> {
    let mut sse = SseReader::new(deltas);
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|_| stream_failure("provider response was unreadable"))?;
        if read == 0 || sse.line(&line, emit, timing, clock)? == Flow::Done {
            break;
        }
    }
    sse.finish(emit, timing)
}

#[derive(Debug, PartialEq, Eq)]
enum Flow {
    More,
    Done,
}

/// The state of one SSE stream, fed a line at a time.
struct SseReader {
    assembly: StreamAssembly,
    emitted: usize,
    coalescer: Option<DeltaCoalescer>,
}

impl SseReader {
    fn new(deltas: bool) -> Self {
        Self {
            assembly: StreamAssembly::default(),
            emitted: 0,
            coalescer: deltas.then(DeltaCoalescer::default),
        }
    }

    fn line(
        &mut self,
        line: &[u8],
        emit: Emit<'_>,
        timing: &mut PlanTiming,
        clock: &mut dyn FnMut() -> Instant,
    ) -> Result<Flow, ProviderFailure> {
        let Some(data) = line.trim_ascii().strip_prefix(b"data:") else {
            return Ok(Flow::More);
        };
        let data = data.trim_ascii();
        if data == b"[DONE]" {
            return Ok(Flow::Done);
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
        let assembly = &mut self.assembly;
        if assembly.push(&chunk) && timing.first_token.is_none() {
            timing.first_token = Some(timing.started.elapsed());
        }
        if assembly.reasoning_seen && timing.first_reasoning.is_none() {
            timing.first_reasoning = Some(timing.started.elapsed());
        }
        let complete = assembly.complete();
        self.emitted = emit_complete(
            assembly,
            self.emitted,
            complete,
            emit,
            timing,
            self.coalescer.as_mut(),
        )?;
        if let Some(coalescer) = self.coalescer.as_mut() {
            coalescer.pending(assembly, self.emitted, clock(), false, emit)?;
        }
        Ok(Flow::More)
    }

    fn finish(
        mut self,
        emit: Emit<'_>,
        timing: &mut PlanTiming,
    ) -> Result<String, ProviderFailure> {
        if self.assembly.finish.is_none() {
            return Err(stream_failure("provider stream ended early"));
        }
        emit_complete(
            &self.assembly,
            self.emitted,
            self.assembly.calls.len(),
            emit,
            timing,
            self.coalescer.as_mut(),
        )?;
        timing.observe_usage(self.assembly.usage.as_ref());
        timing.tool_calls = self.assembly.calls.len();
        Ok(self.assembly.into_completion().to_string())
    }
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
        emit: Emit<'_>,
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
        emit: Emit<'_>,
    ) -> Result<(), ProviderFailure> {
        let Some(call) = assembly.calls.get(index) else {
            return Ok(());
        };
        while self.sent.len() <= index {
            self.sent.push((0, None));
        }
        let Some(slot) = self.sent.get_mut(index) else {
            return Ok(());
        };
        let (sent, last) = *slot;
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
        // `arguments` only grows by whole provider strings, so both offsets
        // are character boundaries.
        let Some(delta) = call.arguments.get(sent..) else {
            return Ok(());
        };
        let event = Response {
            ok: true,
            output: Some(
                serde_json::json!({
                    "index": index,
                    "id": (!call.id.is_empty()).then_some(call.id.as_str()),
                    "name": call.name,
                    "delta": delta,
                })
                .to_string(),
            ),
            event: Some(EVENT_TOOL_CALL_DELTA.into()),
            ..Response::default()
        };
        self.frames = self.frames.saturating_add(1);
        *slot = (call.arguments.len(), Some(now));
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
    emit: Emit<'_>,
    timing: &mut PlanTiming,
    mut coalescer: Option<&mut DeltaCoalescer>,
) -> Result<usize, ProviderFailure> {
    let mut emitted = emitted;
    while emitted < complete {
        let Some(call) = assembly.calls.get(emitted) else {
            break;
        };
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
                event: Some(EVENT_TOOL_CALL.into()),
                ..Response::default()
            };
            if !emit(&event) {
                return Err(stream_failure(
                    "the agent left before the provider finished",
                ));
            }
        }
        emitted = emitted.saturating_add(1);
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
            let Some(slot) = self.calls.get_mut(index) else {
                continue;
            };
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
        let mut message = serde_json::Map::new();
        message.insert("role".into(), "assistant".into());
        message.insert(
            "content".into(),
            self.content
                .map_or(serde_json::Value::Null, serde_json::Value::String),
        );
        if let Some(refusal) = self.refusal {
            message.insert("refusal".into(), serde_json::Value::String(refusal));
        }
        if !self.calls.is_empty() {
            let calls = self
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
            message.insert("tool_calls".into(), calls);
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
    /// The profile and model the task resolved to, once it has.
    route: Option<(String, String)>,
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
            route: None,
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
            "tilo-model-gateway: timing plan trace={trace} task={} profile={} model={} stream={} status={} \
             start_unix_ms={} first_byte_ms={} first_reasoning_ms={} first_token_ms={} first_tool_call_ms={} \
             plan_call_ms={} done_ms={} prompt_tokens={} completion_tokens={} \
             reasoning_tokens={} cached_tokens={} tool_calls={} outcome={}",
            log_token(call.task),
            self.route
                .as_ref()
                .map_or_else(|| "-".to_string(), |(profile, _)| log_token(profile)),
            self.route
                .as_ref()
                .map_or_else(|| "-".to_string(), |(_, model)| log_token(model)),
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
        now.saturating_sub(at)
    );
}

fn unix_ms() -> u128 {
    u128::from(tilo_clock::unix_now_ms())
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

/// Resolve one of Photos' fixed device-only tasks and re-check, beside the
/// resolver, that the model is the supervised loopback runtime, declares
/// `capability` and is pinned ([`local_profile`]).
fn fixed_local_route(
    consumer: &Consumer,
    task: &str,
    capability: ModelCapability,
    purpose: &str,
) -> Result<Resolution, String> {
    let mut resolution = resolve_for(consumer, task, None)?;
    resolution.profile = local_profile(
        resolution.profile,
        &resolution.model.id,
        capability,
        purpose,
    )?;
    Ok(resolution)
}

/// One single-shot transformation, as tilo-agent asked for it.
struct TransformAsk {
    /// How private the input is, as the caller knows it.
    content: tasks::PrivacyClass,
    instructions: String,
    input: String,
    max_output_tokens: u32,
    timeout: Duration,
}

/// Refuse a transformation past any of the published bounds. Refused, not
/// truncated: a silently shortened input would be answered as if it were
/// whole.
fn validate_transform(ask: &TransformAsk) -> Result<(), String> {
    if ask.instructions.trim().is_empty() || ask.input.trim().is_empty() {
        return Err("a transform needs instructions and an input".into());
    }
    if ask.instructions.len() > tilo_model_gateway::MAX_TRANSFORM_INSTRUCTIONS_BYTES {
        return Err("transform instructions exceed the 16 KiB limit".into());
    }
    if ask.input.len() > tilo_model_gateway::MAX_TRANSFORM_INPUT_BYTES {
        return Err("transform input exceeds the 256 KiB limit".into());
    }
    if ask.max_output_tokens == 0
        || ask.max_output_tokens > tilo_model_gateway::MAX_TRANSFORM_OUTPUT_TOKENS
    {
        return Err("transform output must be between 1 and 4096 tokens".into());
    }
    if ask.timeout.is_zero() {
        return Err("a transform needs a deadline".into());
    }
    Ok(())
}

/// The provider request for one transformation. Deterministic sampling, one
/// system instruction, one user turn, and no tools in either dialect.
fn transform_body(
    protocol: ProviderProtocol,
    model_id: &str,
    ask: &TransformAsk,
) -> serde_json::Value {
    match protocol {
        ProviderProtocol::Anthropic => serde_json::json!({
            "model": model_id,
            "max_tokens": ask.max_output_tokens,
            "temperature": 0,
            "system": ask.instructions,
            "messages": [{ "role": "user", "content": ask.input }]
        }),
        ProviderProtocol::OpenAiCompatible | ProviderProtocol::AzureOpenAi => serde_json::json!({
            "model": model_id,
            "max_tokens": ask.max_output_tokens,
            "temperature": 0,
            "response_format": { "type": "json_object" },
            "messages": [
                { "role": "system", "content": ask.instructions },
                { "role": "user", "content": ask.input }
            ]
        }),
    }
}

/// The answer text out of a provider response. A refusal or a cut-off answer
/// is an error rather than text: the caller would otherwise validate half an
/// answer.
fn transform_answer(protocol: ProviderProtocol, body: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(body).map_err(|_| "provider response was unreadable".to_string())?;
    let text = match protocol {
        ProviderProtocol::Anthropic => {
            match value.get("stop_reason").and_then(serde_json::Value::as_str) {
                Some("refusal") => return Err("the model declined the transform".into()),
                Some("max_tokens") => return Err("the model's answer was cut off".into()),
                _ => {}
            }
            value
                .get("content")
                .and_then(serde_json::Value::as_array)
                .and_then(|blocks| {
                    blocks.iter().find(|block| {
                        block.get("type").and_then(serde_json::Value::as_str) == Some("text")
                    })
                })
                .and_then(|block| block.get("text"))
                .and_then(serde_json::Value::as_str)
        }
        ProviderProtocol::OpenAiCompatible | ProviderProtocol::AzureOpenAi => {
            let choice = value
                .get("choices")
                .and_then(serde_json::Value::as_array)
                .and_then(|choices| choices.first());
            if choice
                .and_then(|choice| choice.get("finish_reason"))
                .and_then(serde_json::Value::as_str)
                == Some("length")
            {
                return Err("the model's answer was cut off".into());
            }
            choice
                .and_then(|choice| choice.get("message"))
                .and_then(|message| message.get("content"))
                .and_then(serde_json::Value::as_str)
        }
    }
    .ok_or_else(|| "provider response carried no answer text".to_string())?;
    if text.len() > tilo_model_gateway::MAX_TRANSFORM_OUTPUT_BYTES {
        return Err("the model's answer exceeds the 256 KiB limit".into());
    }
    Ok(text.to_string())
}

/// The tasks the transform verb serves.
const TRANSFORM_TASKS: [&str; 2] = [tasks::AGENT_TRANSFORM.id, tasks::FEEDBACK_TRIAGE.id];

/// Resolve a transform task for `consumer` over `content` and read its
/// credential. Only the tasks served by the transform verb may be named
/// here; a caller-dialect or another fixed task goes through its own verb.
fn transform_route(
    consumer: &Consumer,
    task: &str,
    agent: Option<&str>,
    content: tasks::PrivacyClass,
) -> Result<(Resolution, Bearer), String> {
    if !TRANSFORM_TASKS.contains(&task) {
        return Err("this task is not served through the transform operation".into());
    }
    let resolution =
        resolve_task_for(consumer, task, agent, content).map_err(|error| error.to_string())?;
    let bearer = credential_for(&resolution.profile)?;
    Ok((resolution, bearer))
}

async fn transform(
    http: &reqwest::Client,
    consumer: Consumer,
    task: String,
    agent: Option<String>,
    ask: TransformAsk,
) -> Result<String, String> {
    validate_transform(&ask)?;
    let content = ask.content;
    let (resolution, bearer) =
        blocking(move || transform_route(&consumer, &task, agent.as_deref(), content)).await?;
    let protocol = resolution.profile.protocol;
    let mut body = transform_body(protocol, &resolution.model.id, &ask);
    // The assignment, not the caller, sets the model's typed options.
    let headers = resolution.options.apply(protocol, &mut body);
    let http = &profile_client(http, &resolution.profile)?;
    let endpoint = provider_url(&resolution.profile);
    let timeout = ask.timeout.min(tilo_model_gateway::MAX_TRANSFORM_TIMEOUT);
    let began = Instant::now();
    let result = while_busy(
        || timeout.checked_sub(began.elapsed()),
        tokio::time::sleep,
        |budget| {
            provider_attempt(
                http,
                &resolution.profile,
                &endpoint,
                &bearer,
                &headers,
                &body,
                budget,
            )
        },
    )
    .await;
    drop(bearer);
    transform_answer(protocol, &result?)
}

/// Validate Photos' one fixed local-only visual-analysis operation and
/// build its request. No arbitrary prompt crosses the app boundary: the
/// schema, token budget, and image URL shape are all constructed here.
fn vision_request(
    consumer: &Consumer,
    image: &VisionImage,
) -> Result<(ProviderProfile, serde_json::Value), String> {
    validate_vision_image(image)?;
    let resolution = fixed_local_route(
        consumer,
        tasks::PHOTOS_VISION.id,
        ModelCapability::Vision,
        "image analysis",
    )?;
    let body = local_vision_body(&resolution.model.id, image);
    Ok((resolution.profile, body))
}

/// Run Photos' fixed visual-analysis operation (see [`vision_request`]).
async fn analyze_image(
    http: &reqwest::Client,
    consumer: Consumer,
    image: VisionImage,
) -> Result<String, String> {
    let (profile, body) = blocking(move || vision_request(&consumer, &image)).await?;
    let http = &profile_client(http, &profile)?;
    let endpoint = provider_url(&profile);
    let began = Instant::now();
    // A local vLLM endpoint is unauthenticated. Do not invent a bearer
    // token, and never look up a hosted-provider secret for it.
    while_busy(
        || VISION_TIMEOUT.checked_sub(began.elapsed()),
        tokio::time::sleep,
        |budget| provider_attempt(http, &profile, &endpoint, &Bearer::None, &[], &body, budget),
    )
    .await
}

/// Validate one fixed face-clustering request against the separate signed
/// local capability and build it. Generic image description models cannot
/// use this path.
fn face_request(
    consumer: &Consumer,
    images: &[FaceImage],
) -> Result<(ProviderProfile, serde_json::Value), String> {
    validate_face_images(images)?;
    let resolution = fixed_local_route(
        consumer,
        tasks::PHOTOS_FACES.id,
        ModelCapability::FaceRecognition,
        "face clustering",
    )?;
    let body = local_face_body(&resolution.model.id, images);
    Ok((resolution.profile, body))
}

/// Run one fixed face-clustering request (see [`face_request`]).
async fn cluster_faces(
    http: &reqwest::Client,
    consumer: Consumer,
    images: Vec<FaceImage>,
) -> Result<String, String> {
    let (profile, body) = blocking(move || face_request(&consumer, &images)).await?;
    let http = &profile_client(http, &profile)?;
    let endpoint = provider_url(&profile);
    let began = Instant::now();
    while_busy(
        || VISION_TIMEOUT.checked_sub(began.elapsed()),
        tokio::time::sleep,
        |budget| provider_attempt(http, &profile, &endpoint, &Bearer::None, &[], &body, budget),
    )
    .await
}

const PROBE_TIMEOUT: Duration = Duration::from_secs(8);

/// Whether `profile_id`'s credential is present here and its endpoint lists
/// models with it. A listing is read-only and costs no tokens.
async fn probe(http: &reqwest::Client, profile_id: String) -> Result<ProviderHealth, String> {
    let (profile, bearer) = blocking(move || {
        let profile = configured_profile(&profile_id)?;
        let bearer = credential_for(&profile);
        Ok((profile, bearer))
    })
    .await?;
    let Ok(bearer) = bearer else {
        return Ok(ProviderHealth {
            credential: CredentialState::Missing,
            reachability: Reachability::NotChecked,
        });
    };
    let mut request = authorize(http.get(models_url(&profile)), &profile, &bearer, &[])
        .map_err(|failure| failure.why)?
        .timeout(PROBE_TIMEOUT);
    if profile.protocol == ProviderProtocol::Anthropic {
        request = request.header("anthropic-version", "2023-06-01");
    }
    let reachability = match request.send().await {
        Ok(response) if response.status().is_success() => Reachability::Reachable,
        Ok(response) if matches!(response.status().as_u16(), 401 | 403) => {
            Reachability::Unauthorized
        }
        Ok(response) => Reachability::Failed {
            status: response.status().as_u16(),
        },
        Err(_) => Reachability::Unreachable,
    };
    Ok(ProviderHealth {
        credential: bearer.state(),
        reachability,
    })
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

/// `profile`, when it may serve the fixed local verb `purpose` with
/// `model_id`: a device-local loopback endpoint whose model declares the
/// capability in signed metadata and has a pinned revision. Hosted and
/// private-network profiles are refused, whatever their models claim.
fn local_profile(
    profile: ProviderProfile,
    model_id: &str,
    capability: ModelCapability,
    purpose: &str,
) -> Result<ProviderProfile, String> {
    if !profile.is_supervised() {
        return Err(format!(
            "{purpose} requires the supervised model runtime on this device"
        ));
    }
    let model = profile
        .models
        .iter()
        .find(|model| model.id == model_id)
        .ok_or_else(|| "model is not configured for this provider".to_string())?;
    if !model.declares(capability) {
        return Err(format!(
            "the selected local model does not declare {purpose} capability"
        ));
    }
    if profile.pinned_local_model(model_id, capability).is_none() {
        return Err(format!(
            "the selected local {purpose} model has no pinned revision"
        ));
    }
    Ok(profile)
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
    let mut content = Vec::with_capacity(images.len().saturating_add(1));
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

/// Attach `bearer` the way `profile`'s protocol expects, plus any headers
/// the request's typed options need.
fn authorize(
    mut request: reqwest::RequestBuilder,
    profile: &ProviderProfile,
    bearer: &Bearer,
    headers: &[(&'static str, &'static str)],
) -> Result<reqwest::RequestBuilder, ProviderFailure> {
    match (profile.protocol, bearer) {
        (ProviderProtocol::Anthropic, Bearer::ApiKey(key)) => {
            request = request
                .header("anthropic-version", "2023-06-01")
                .header("x-api-key", key.as_str());
        }
        (ProviderProtocol::Anthropic, _) => {
            return Err(ProviderFailure {
                why: "Anthropic requests require a configured API key".into(),
                again: Again::Never,
                outcome_unknown: false,
                status: None,
            });
        }
        (ProviderProtocol::AzureOpenAi, Bearer::ApiKey(key)) => {
            request = request.header("api-key", key.as_str());
        }
        (ProviderProtocol::AzureOpenAi, Bearer::None) => {
            return Err(ProviderFailure {
                why: "Azure OpenAI requests require a key or a signed-in account".into(),
                again: Again::Never,
                outcome_unknown: false,
                status: None,
            });
        }
        (
            ProviderProtocol::AzureOpenAi | ProviderProtocol::OpenAiCompatible,
            Bearer::Token(token),
        )
        | (ProviderProtocol::OpenAiCompatible, Bearer::ApiKey(token)) => {
            request = request.bearer_auth(token.as_str());
        }
        (ProviderProtocol::OpenAiCompatible, Bearer::None) => {}
    }
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    Ok(request)
}

async fn provider_attempt(
    client: &reqwest::Client,
    profile: &ProviderProfile,
    endpoint: &str,
    bearer: &Bearer,
    headers: &[(&'static str, &'static str)],
    body: &serde_json::Value,
    budget: Duration,
) -> Result<String, ProviderFailure> {
    send_provider(client, profile, endpoint, bearer, headers, body, budget)
        .await?
        .text()
        .await
        .map_err(|_| ProviderFailure {
            why: "provider response was unreadable".into(),
            again: Again::Never,
            outcome_unknown: true,
            status: None,
        })
}

/// Send one provider POST and return the successful response unread.
async fn send_provider(
    client: &reqwest::Client,
    profile: &ProviderProfile,
    endpoint: &str,
    bearer: &Bearer,
    headers: &[(&'static str, &'static str)],
    body: &serde_json::Value,
    budget: Duration,
) -> Result<reqwest::Response, ProviderFailure> {
    let request = authorize(
        client
            .post(endpoint)
            .timeout(budget)
            .header("content-type", "application/json"),
        profile,
        bearer,
        headers,
    )?;
    let response = request
        .json(body)
        .send()
        .await
        .map_err(|error| ProviderFailure {
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
        let detail = error_body(response).await;
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
const MAX_ERROR_BODY: usize = 16 * 1024;

/// Up to [`MAX_ERROR_BODY`] bytes of an unsuccessful response's body.
async fn error_body(mut response: reqwest::Response) -> Vec<u8> {
    let mut detail = Vec::new();
    while detail.len() < MAX_ERROR_BODY {
        let Ok(Some(chunk)) = response.chunk().await else {
            break;
        };
        let take = chunk.len().min(MAX_ERROR_BODY.saturating_sub(detail.len()));
        detail.extend(chunk.iter().take(take));
    }
    detail
}

/// The provider's machine-readable error classification, and nothing else:
/// `error.type`, `error.code` and `error.param` (`OpenAI`) or `error.type`
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

async fn while_busy<L, W, WF, A, AF>(left: L, mut wait: W, mut attempt: A) -> Result<String, String>
where
    L: Fn() -> Option<Duration>,
    W: FnMut(Duration) -> WF,
    WF: Future<Output = ()>,
    A: FnMut(Duration) -> AF,
    AF: Future<Output = Result<String, ProviderFailure>>,
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
        let ProviderFailure { why, again, .. } = match attempt(budget).await {
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
        wait(waiting).await;
    }
}

/// The client that reaches `profile`: the shared one, or, for a runtime the
/// model manager supervises, one bound to its private socket. Such a runtime
/// has no network and no TCP listener; the socket is its only door.
pub(crate) fn profile_client(
    shared: &reqwest::Client,
    profile: &ProviderProfile,
) -> Result<reqwest::Client, String> {
    match &profile.custody {
        EndpointCustody::Supervised { socket } => tilo_net::http::client()
            .unix_socket(socket.clone())
            .connect_timeout(CONNECT_TIMEOUT)
            // As the shared client: one Plan authorizes at most one POST.
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .build()
            .map_err(|_| "the local model's socket client is unavailable".to_string()),
        EndpointCustody::UserConfigured => Ok(shared.clone()),
    }
}

fn provider_url(profile: &ProviderProfile) -> String {
    let base = profile.endpoint.trim_end_matches('/');
    match profile.protocol {
        ProviderProtocol::Anthropic if base.ends_with("/v1/messages") => base.into(),
        ProviderProtocol::Anthropic => format!("{base}/v1/messages"),
        ProviderProtocol::OpenAiCompatible | ProviderProtocol::AzureOpenAi
            if base.ends_with("/chat/completions") =>
        {
            base.into()
        }
        ProviderProtocol::OpenAiCompatible | ProviderProtocol::AzureOpenAi => {
            format!("{base}/chat/completions")
        }
    }
}

/// The read-only model listing a probe asks for.
fn models_url(profile: &ProviderProfile) -> String {
    let base = profile.endpoint.trim_end_matches('/');
    let base = base
        .strip_suffix("/chat/completions")
        .or_else(|| base.strip_suffix("/v1/messages"))
        .unwrap_or(base);
    match profile.protocol {
        ProviderProtocol::Anthropic => format!("{base}/v1/models"),
        ProviderProtocol::OpenAiCompatible | ProviderProtocol::AzureOpenAi => {
            format!("{base}/models")
        }
    }
}

/// Whether the peer's `SO_PEERCRED` uid is this process's effective uid.
#[cfg(target_os = "linux")]
fn same_user(stream: &tokio::net::UnixStream) -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    stream.peer_cred().is_ok_and(|peer| peer.uid() == euid)
}

// Peer authentication is implemented only for the native Linux gateway.
// Other hosts may exercise provider logic but must not accept service clients.
#[cfg(not(target_os = "linux"))]
fn same_user(_stream: &tokio::net::UnixStream) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use agent_provider_config::{EndpointLocation, ModelDescriptor};

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

    fn read_principal(body: &[u8]) -> Result<Principal, String> {
        use std::io::Write as _;
        let (mut peer, gateway) = std::os::unix::net::UnixStream::pair().unwrap();
        peer.write_all(&u32::try_from(body.len()).unwrap().to_le_bytes())
            .unwrap();
        peer.write_all(body).unwrap();
        gateway.set_nonblocking(true).unwrap();
        let runtime = tilo_rt::daemon("gateway-test").workers(1).build().unwrap();
        runtime.block_on(async {
            let mut gateway = tokio::net::UnixStream::from_std(gateway).unwrap();
            read_handoff(&mut gateway).await
        })
    }

    #[test]
    fn unknown_apps_and_malformed_handoffs_are_refused() {
        for body in [
            br#"{"protocol":2,"principal":"app","app_id":"so.tilo.notes"}"#.as_slice(),
            br#"{"protocol":1,"app_id":"so.tilo.agent"}"#,
            br#"{"protocol":2,"principal":"session","app_id":"so.tilo.agent"}"#,
            br#"{"protocol":2,"principal":"root"}"#,
            br#"{"protocol":2,"principal":"app"}"#,
            br#"{"protocol":2,"principal":"session","extra":true}"#,
        ] {
            assert!(
                read_principal(body).is_err(),
                "{}",
                String::from_utf8_lossy(body)
            );
        }
    }

    fn profile(
        location: EndpointLocation,
        endpoint: &str,
        revision: Option<&str>,
    ) -> ProviderProfile {
        ProviderProfile {
            id: "local".into(),
            location,
            endpoint: endpoint.into(),
            models: vec![ModelDescriptor {
                revision: revision.map(Into::into),
                capabilities: [
                    ModelCapability::Vision,
                    ModelCapability::FaceRecognition,
                    ModelCapability::TraceReview,
                ]
                .into(),
                ..ModelDescriptor::new("m")
            }],
            ..ProviderProfile::default()
        }
    }

    const VERBS: [(ModelCapability, &str); 3] = [
        (ModelCapability::Vision, "image analysis"),
        (ModelCapability::FaceRecognition, "face clustering"),
        (ModelCapability::TraceReview, "trace review"),
    ];

    fn handled(principal: &Principal, request: Request) -> Response {
        let runtime = tilo_rt::daemon("gateway-test").workers(1).build().unwrap();
        let http = provider_client().unwrap();
        runtime.block_on(handle(principal, request, &http))
    }

    fn review_request() -> Request {
        Request::ReviewTrace {
            trace: tilo_feedback_review::ReviewTraceRequest { steps: Vec::new() },
        }
    }

    #[test]
    fn only_the_desktop_session_may_review_a_trace() {
        for app in [SETTINGS_APP, AGENT_APP, PHOTOS_APP] {
            let response = handled(&Principal::App { app_id: app.into() }, review_request());
            assert!(!response.ok);
            assert_eq!(
                response.error.as_deref(),
                Some("only the desktop session may request feedback review"),
                "{app}"
            );
        }
        // The session gets past authorization; this request then fails the
        // contract before any profile is read.
        let response = handled(&Principal::Session, review_request());
        assert!(!response.ok);
        assert_ne!(
            response.error.as_deref(),
            Some("only the desktop session may request feedback review")
        );
    }

    #[test]
    fn the_desktop_session_is_authorized_for_review_only() {
        let refused = [
            Request::SecretStatus {
                profile_id: "hosted".into(),
            },
            Request::SetApiKey {
                profile_id: "hosted".into(),
                api_key: "owned fixture".into(),
            },
            Request::Plan {
                task: tasks::AGENT_PLANNING.id.into(),
                agent: None,
                protocol: ProviderProtocol::Anthropic,
                body: serde_json::Value::Null,
                timeout_ms: 1,
                stream: false,
                deltas: false,
                trace: None,
            },
            Request::AnalyzeImage {
                image: vision_image(),
            },
        ];
        for request in refused {
            let response = handled(&Principal::Session, request);
            assert!(!response.ok);
            let error = response.error.unwrap_or_default();
            assert!(
                error.starts_with("only ")
                    || error == "caller is not authorized for this gateway operation"
                    || error == "caller may not request this model task",
                "{error}"
            );
        }
    }

    fn hosted_at(protocol: ProviderProtocol, endpoint: &str) -> ProviderProfile {
        let mut profile = ProviderProfile::hosted(protocol);
        profile.endpoint = endpoint.into();
        profile
    }

    #[test]
    fn each_protocol_is_addressed_and_authorized_in_its_own_dialect() {
        let azure = hosted_at(
            ProviderProtocol::AzureOpenAi,
            "https://tilo.openai.azure.com/openai/v1/",
        );
        assert_eq!(
            provider_url(&azure),
            "https://tilo.openai.azure.com/openai/v1/chat/completions"
        );
        assert_eq!(
            models_url(&azure),
            "https://tilo.openai.azure.com/openai/v1/models"
        );
        let anthropic = hosted_at(ProviderProtocol::Anthropic, "https://api.anthropic.com");
        assert_eq!(
            models_url(&anthropic),
            "https://api.anthropic.com/v1/models"
        );

        let http = provider_client().unwrap();
        let key = || Bearer::ApiKey(Zeroizing::new("k".into()));
        let token = || Bearer::Token(Zeroizing::new("t".into()));
        let built = |profile: &ProviderProfile, bearer: &Bearer| {
            authorize(
                http.post("https://example.test"),
                profile,
                bearer,
                &[("x-extra", "1")],
            )
            .map(|request| request.build().unwrap())
        };
        let with_key = built(&azure, &key()).ok().unwrap();
        assert_eq!(with_key.headers()["api-key"], "k");
        assert!(with_key.headers().get("authorization").is_none());
        assert_eq!(with_key.headers()["x-extra"], "1");
        let signed_in = built(&azure, &token()).ok().unwrap();
        assert_eq!(signed_in.headers()["authorization"], "Bearer t");
        assert!(built(&azure, &Bearer::None).is_err());
        assert_eq!(
            built(&anthropic, &key()).ok().unwrap().headers()["x-api-key"],
            "k"
        );
        assert!(built(&anthropic, &token()).is_err());
    }

    #[test]
    fn a_hub_provider_signed_in_through_custody_carries_the_custody_token() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("tilo");
        let id = AccountId::new("azure-openai-work");
        let tokens = account_custody::TokenSet {
            access_token: "custody-access".into(),
            refresh_token: "custody-refresh".into(),
            expires_in: 3600,
            obtained_at: 0,
        }
        .stamp_now();
        let mut vault = SecretVault::open_in(&root);
        vault.put(&id, &account_custody::oauth_secret(&tokens));
        vault.try_save().unwrap();
        let mut store = accounts::AccountStore::load_in(&root);
        store.upsert(accounts::Account::new(
            id.clone(),
            account_custody::providers::AZURE_OPENAI_TYPE,
        ));
        store.save_in(&root).unwrap();
        let custodian = account_custody::Custodian::for_root(Some(root));

        let mut azure = hosted_at(
            ProviderProtocol::AzureOpenAi,
            "https://tilo.openai.azure.com/openai/v1/",
        );
        azure.credential = Some(Credential::Account {
            account_id: id.as_str().into(),
        });
        let bearer = credential_in(&custodian, &azure).unwrap();
        assert!(matches!(&bearer, Bearer::Token(token) if token.as_str() == "custody-access"));
        let request = authorize(
            provider_client().unwrap().get(provider_url(&azure)),
            &azure,
            &bearer,
            &[],
        )
        .ok()
        .unwrap()
        .build()
        .unwrap();
        assert_eq!(request.headers()["authorization"], "Bearer custody-access");

        // An account the store no longer holds is not signed in.
        azure.credential = Some(Credential::Account {
            account_id: "gone".into(),
        });
        assert!(credential_in(&custodian, &azure).is_err());
    }

    #[test]
    fn a_fixed_task_cannot_be_driven_through_the_caller_dialect_verb() {
        let error = plan_route(
            &Consumer::app(PHOTOS_APP),
            tasks::PHOTOS_VISION.id,
            None,
            ProviderProtocol::OpenAiCompatible,
        )
        .err()
        .unwrap();
        assert!(error.contains("fixed gateway operation"), "{error}");
    }

    /// The handoff bytes are pinned; local vision input is closed and bounded; the local verbs run only
    /// on the pinned, supervised runtime, never on a LAN, hosted or typed-in loopback endpoint; and the
    /// vision and face prompts are fixed and never ask for identity.
    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "each case keeps its original fixture and assertions"
    )]
    fn local_vision_is_pinned_bounded_local_and_never_asks_identity() {
        // Phase: handoff_bytes_are_pinned.
        // The handoff: a little-endian `u32` length, then JSON. The launcher
        // writes the app form, the desktop session the session form.
        {
            assert_eq!(
                read_principal(br#"{"protocol":2,"principal":"app","app_id":"so.tilo.agent"}"#),
                Ok(Principal::App {
                    app_id: AGENT_APP.into()
                })
            );
            assert_eq!(
                read_principal(br#"{"protocol":2,"principal":"session"}"#),
                Ok(Principal::Session)
            );
            assert_eq!(
                Principal::Session.handoff().unwrap(),
                br#"{"protocol":2,"principal":"session"}"#
            );
        }

        // Phase: local_vision_input_is_closed_and_bounded.
        {
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

        // Phase: local_verbs_run_only_on_the_pinned_supervised_runtime.
        {
            let pinned = "a".repeat(40);
            let supervised = |revision: Option<&str>| {
                let mut profile = profile(
                    EndpointLocation::ThisDevice,
                    agent_provider_config::SUPERVISED_BASE_URL,
                    revision,
                );
                profile.id = agent_provider_config::LOCAL_PROFILE_ID.into();
                profile.custody = EndpointCustody::Supervised {
                    socket: "/run/user/1000/tilo/models/m/api.sock".into(),
                };
                profile
            };
            for (capability, purpose) in VERBS {
                let run = |profile| local_profile(profile, "m", capability, purpose);
                assert!(run(supervised(Some(&pinned))).is_ok());
                for refused in [
                    profile(
                        EndpointLocation::Hosted,
                        "https://models.example.test/v1",
                        Some(&pinned),
                    ),
                    profile(
                        EndpointLocation::PrivateNetwork,
                        "http://192.168.1.20:8123/v1",
                        Some(&pinned),
                    ),
                    // A typed-in loopback URL could forward what it is sent.
                    profile(
                        EndpointLocation::ThisDevice,
                        "http://127.0.0.1:8123/v1",
                        Some(&pinned),
                    ),
                    profile(
                        EndpointLocation::ThisDevice,
                        "http://localhost.evil.test:8123/v1",
                        Some(&pinned),
                    ),
                    supervised(Some("main")),
                    supervised(Some(&"A".repeat(40))),
                    supervised(None),
                ] {
                    assert!(run(refused.clone()).is_err(), "{purpose}: {refused:?}");
                }
                let mut undeclared = supervised(Some(&pinned));
                undeclared.models = vec![ModelDescriptor {
                    revision: Some(pinned.clone()),
                    ..ModelDescriptor::new("m")
                }];
                assert!(run(undeclared).is_err(), "{purpose} must be declared");
                assert!(
                    local_profile(supervised(Some(&pinned)), "other", capability, purpose).is_err()
                );
            }
        }

        // Phase: local_vision_prompt_is_fixed_and_contains_no_identity_request.
        {
            let body = local_vision_body("vision-model", &vision_image());
            let encoded = body.to_string();
            assert!(encoded.contains("device-local image indexing component"));
            assert!(encoded.contains("Do not name people"));
            assert!(!encoded.contains("ignore previous"));
            assert_eq!(body["model"], "vision-model");
        }

        // Phase: local_face_input_is_bounded_and_prompt_never_requests_identity.
        {
            let face = FaceImage {
                photo_id: "photos.photo:one".into(),
                media_type: "image/jpeg".into(),
                data_base64: base64::engine::general_purpose::STANDARD.encode(b"preview"),
                source_content_hash: "a".repeat(64),
            };
            assert!(validate_face_images(std::slice::from_ref(&face)).is_ok());
            let mut duplicate = face.clone();
            duplicate.photo_id = face.photo_id.clone();
            assert!(validate_face_images(&[face.clone(), duplicate]).is_err());
            let body = local_face_body("face-model", &[face]);
            let encoded = body.to_string();
            assert!(encoded.contains("face detection and clustering"));
            assert!(encoded.contains("Never output a person's name"));
            assert!(!encoded.contains("identify the person"));
        }
    }

    fn ask(input: &str) -> TransformAsk {
        TransformAsk {
            content: tasks::PrivacyClass::Open,
            instructions: "Answer with JSON.".into(),
            input: input.into(),
            max_output_tokens: 512,
            timeout: Duration::from_secs(5),
        }
    }

    #[test]
    fn transform_requests_are_bounded_before_any_provider_is_asked() {
        assert!(validate_transform(&ask("text")).is_ok());
        let huge = "x".repeat(tilo_model_gateway::MAX_TRANSFORM_INPUT_BYTES + 1);
        assert!(validate_transform(&ask(&huge)).is_err());
        assert!(validate_transform(&ask("  ")).is_err());
        let mut greedy = ask("text");
        greedy.max_output_tokens = tilo_model_gateway::MAX_TRANSFORM_OUTPUT_TOKENS + 1;
        assert!(validate_transform(&greedy).is_err());
        let mut rushed = ask("text");
        rushed.timeout = Duration::ZERO;
        assert!(validate_transform(&rushed).is_err());
    }

    #[test]
    fn transform_bodies_carry_no_tools_in_either_dialect() {
        for protocol in [
            ProviderProtocol::Anthropic,
            ProviderProtocol::OpenAiCompatible,
        ] {
            let body = transform_body(protocol, "model-x", &ask("the input"));
            assert_eq!(body["model"], "model-x");
            assert_eq!(body["max_tokens"], 512);
            assert!(body.get("tools").is_none());
            assert!(body.to_string().contains("the input"));
        }
    }

    #[test]
    fn transform_answers_are_the_text_and_never_a_refusal_or_a_fragment() {
        let anthropic =
            r#"{"stop_reason":"end_turn","content":[{"type":"text","text":"{\"result\":1}"}]}"#;
        assert_eq!(
            transform_answer(ProviderProtocol::Anthropic, anthropic).unwrap(),
            r#"{"result":1}"#
        );
        let refused = r#"{"stop_reason":"refusal","content":[]}"#;
        assert!(transform_answer(ProviderProtocol::Anthropic, refused).is_err());
        let cut = r#"{"stop_reason":"max_tokens","content":[{"type":"text","text":"{"}]}"#;
        assert!(transform_answer(ProviderProtocol::Anthropic, cut).is_err());
        let openai = r#"{"choices":[{"finish_reason":"stop","message":{"content":"{}"}}]}"#;
        assert_eq!(
            transform_answer(ProviderProtocol::OpenAiCompatible, openai).unwrap(),
            "{}"
        );
        let truncated = r#"{"choices":[{"finish_reason":"length","message":{"content":"{"}}]}"#;
        assert!(transform_answer(ProviderProtocol::OpenAiCompatible, truncated).is_err());
    }

    fn transform_request(task: &str) -> Request {
        Request::Transform {
            task: task.into(),
            agent: None,
            content: tasks::PrivacyClass::Open,
            instructions: "Answer.".into(),
            input: "text".into(),
            max_output_tokens: 16,
            timeout_ms: 1000,
        }
    }

    #[test]
    fn only_the_agent_may_transform() {
        let callers = [
            Principal::App {
                app_id: PHOTOS_APP.into(),
            },
            Principal::App {
                app_id: SETTINGS_APP.into(),
            },
            Principal::Session,
        ];
        for caller in callers {
            let response = handled(&caller, transform_request(tasks::AGENT_TRANSFORM.id));
            assert!(!response.ok, "{caller:?}");
            assert_eq!(
                response.error.as_deref(),
                Some("caller may not request this model task"),
                "{caller:?}"
            );
        }
    }

    #[test]
    fn the_transform_verb_serves_only_the_transform_tasks() {
        let agent = Principal::App {
            app_id: AGENT_APP.into(),
        };
        for task in [tasks::AGENT_PLANNING.id, tasks::FEEDBACK_REVIEW.id] {
            let response = handled(&agent, transform_request(task));
            assert!(!response.ok, "{task}");
            assert_eq!(
                response.error.as_deref(),
                Some("this task is not served through the transform operation"),
                "{task}"
            );
        }
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
        let result = tilo_rt::block_on(while_busy(
            || {
                let fake = fake.borrow();
                (!fake.left.is_zero()).then_some(fake.left)
            },
            |duration| {
                let mut fake = fake.borrow_mut();
                fake.waits.push(duration);
                fake.left = fake.left.saturating_sub(duration);
                std::future::ready(())
            },
            |_budget| {
                let mut fake = fake.borrow_mut();
                let index = fake.calls.min(outcomes.len().saturating_sub(1));
                fake.calls = fake.calls.saturating_add(1);
                fake.left = fake.left.saturating_sub(Duration::from_millis(200));
                std::future::ready(match outcomes[index] {
                    Ok(output) => Ok(output.to_string()),
                    Err((status, retry_after)) => Err(ProviderFailure {
                        why: format!("provider returned HTTP {status}"),
                        again: again_after(status, retry_after),
                        outcome_unknown: true,
                        status: Some(status),
                    }),
                })
            },
        ))
        .unwrap();
        (result, fake.into_inner())
    }

    /// Transient provider failures are retried at the gateway, settled ones are not, Retry-After and
    /// transient statuses are classified on the HTTP side, and retries never consume room needed for an
    /// answer.
    #[test]
    fn the_gateway_retries_only_transient_failures_within_the_answer_budget() {
        // Phase: transient_provider_failures_are_retried_at_the_gateway.
        {
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

        // Phase: settled_provider_failures_are_not_retried.
        {
            for status in [400, 401, 403, 404, 422] {
                let (result, fake) = run_retries(Duration::from_secs(20), &[Err((status, None))]);
                assert!(result.unwrap_err().contains(&status.to_string()));
                assert_eq!(fake.calls, 1, "HTTP {status}");
                assert!(fake.waits.is_empty(), "HTTP {status}");
            }
        }

        // Phase: retry_after_and_transient_statuses_are_classified_on_the_http_side.
        {
            for status in [408, 429, 500, 502, 503, 504, 529] {
                assert_eq!(again_after(status, None), Again::Soon(None));
            }
            assert_eq!(
                again_after(429, Some("0.25")),
                Again::Soon(Some(Duration::from_millis(250)))
            );
            assert_eq!(again_after(401, Some("1")), Again::Never);
        }

        // Phase: retries_never_consume_room_needed_for_an_answer.
        {
            let (result, fake) = run_retries(Duration::from_secs(3), &[Err((529, Some("1")))]);
            assert!(result.is_err());
            assert_eq!(fake.calls, 1);
            assert!(fake.waits.is_empty());
        }
    }

    fn hosted_openai() -> ProviderProfile {
        let mut profile = ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible);
        profile.endpoint = "https://models.example.test/api/v1".into();
        profile.models = vec![agent_provider_config::ModelDescriptor::new(
            "vendor/model-a",
        )];
        profile
    }

    fn reasoning(param: ReasoningParam, effort: &str) -> agent_provider_config::ReasoningSetting {
        agent_provider_config::ReasoningSetting {
            param,
            effort: effort.into(),
        }
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "one table of profile shapes and assignment options, each case self-contained"
    )]
    fn outbound_body_is_owned_by_the_gateway() {
        let none = ModelOptions::default();
        let shaped = |profile: &ProviderProfile,
                      model: &str,
                      options: &ModelOptions,
                      body: serde_json::Value,
                      streaming: bool| {
            outbound_body(profile, model, *options, body, streaming).map(|(body, _)| body)
        };
        let caller = serde_json::json!({
            "model": "other/model", "max_completion_tokens": 64, "stream": true,
            "stream_options": {"include_usage": true}, "reasoning_effort": "high", "messages": []
        });
        // An unconfigured profile: only the model, and the caller's stream, change.
        let body = shaped(
            &hosted_openai(),
            "vendor/model-a",
            &none,
            caller.clone(),
            false,
        )
        .unwrap();
        assert_eq!(body["model"], "vendor/model-a");
        assert_eq!(body["max_completion_tokens"], 64);
        assert_eq!(body["reasoning_effort"], "high");
        assert!(body.get("stream").is_none() && body.get("stream_options").is_none());

        let streamed = shaped(
            &hosted_openai(),
            "vendor/model-a",
            &none,
            caller.clone(),
            true,
        )
        .unwrap();
        assert_eq!(streamed["stream"], true);
        assert_eq!(streamed["stream_options"]["include_usage"], true);

        // OpenRouter-style: `max_tokens` and the reasoning object.
        let mut router = hosted_openai();
        router.token_limit_param = Some(agent_provider_config::TokenLimitParam::MaxTokens);
        router.reasoning = Some(reasoning(ReasoningParam::ReasoningObject, "low"));
        let body = shaped(&router, "vendor/model-a", &none, caller.clone(), false).unwrap();
        assert_eq!(body["max_tokens"], 64);
        assert!(body.get("max_completion_tokens").is_none());
        assert_eq!(body["reasoning"], serde_json::json!({"effort": "low"}));
        assert!(body.get("reasoning_effort").is_none());

        // The assignment's effort is the value; the profile still names the
        // field it goes in.
        let high = ModelOptions {
            fast_mode: false,
            reasoning_effort: Some(agent_provider_config::ReasoningEffort::High),
        };
        let body = shaped(&router, "vendor/model-a", &high, caller.clone(), false).unwrap();
        assert_eq!(body["reasoning"], serde_json::json!({"effort": "high"}));
        assert!(body.get("reasoning_effort").is_none());
        // Without a profile setting the options write their own dialect.
        let body = shaped(
            &hosted_openai(),
            "vendor/model-a",
            &high,
            serde_json::json!({"messages": []}),
            false,
        )
        .unwrap();
        assert_eq!(body["reasoning_effort"], "high");

        // OpenAI-style: `max_completion_tokens` and top-level effort.
        let mut openai = hosted_openai();
        openai.token_limit_param =
            Some(agent_provider_config::TokenLimitParam::MaxCompletionTokens);
        openai.reasoning = Some(reasoning(ReasoningParam::ReasoningEffort, "none"));
        let legacy = serde_json::json!({"max_tokens": 9, "reasoning": {"effort": "high"}});
        let body = shaped(&openai, "vendor/model-a", &none, legacy, false).unwrap();
        assert_eq!(body["max_completion_tokens"], 9);
        assert!(body.get("max_tokens").is_none() && body.get("reasoning").is_none());
        assert_eq!(body["reasoning_effort"], "none");

        // Azure OpenAI speaks the same dialect and takes the same shaping.
        let mut azure = hosted_at(
            ProviderProtocol::AzureOpenAi,
            "https://tilo.openai.azure.com/openai/v1/",
        );
        azure.token_limit_param = Some(agent_provider_config::TokenLimitParam::MaxCompletionTokens);
        let body = shaped(
            &azure,
            "deployment",
            &none,
            serde_json::json!({"max_tokens": 9}),
            true,
        )
        .unwrap();
        assert_eq!(body["model"], "deployment");
        assert_eq!(body["max_completion_tokens"], 9);
        assert_eq!(body["stream"], true);

        // Cerebras-style: fields the provider rejects are stripped last,
        // including the gateway's own stream_options.
        let mut strict = hosted_openai();
        strict.omit_params = vec!["stream_options".into(), "parallel_tool_calls".into()];
        let with_extras = serde_json::json!({"parallel_tool_calls": false, "messages": []});
        let body = shaped(&strict, "vendor/model-a", &none, with_extras, true).unwrap();
        assert_eq!(body["stream"], true);
        assert!(body.get("stream_options").is_none() && body.get("parallel_tool_calls").is_none());

        // `none` strips every reasoning field, the assignment's included.
        let mut omit = hosted_openai();
        omit.reasoning = Some(reasoning(ReasoningParam::Omit, ""));
        let body = shaped(&omit, "vendor/model-a", &high, caller, false).unwrap();
        assert!(body.get("reasoning_effort").is_none() && body.get("reasoning").is_none());

        let mut anthropic = ProviderProfile::hosted(ProviderProtocol::Anthropic);
        anthropic.models = vec![agent_provider_config::ModelDescriptor::new("claude-x")];
        anthropic.reasoning = Some(reasoning(ReasoningParam::ReasoningEffort, "low"));
        anthropic.token_limit_param =
            Some(agent_provider_config::TokenLimitParam::MaxCompletionTokens);
        let body = shaped(
            &anthropic,
            "claude-x",
            &none,
            serde_json::json!({"model": "other", "max_tokens": 9, "stream": true}),
            true,
        )
        .unwrap();
        assert_eq!(body["model"], "claude-x");
        assert_eq!(body["max_tokens"], 9);
        assert!(body.get("stream").is_none() && body.get("reasoning_effort").is_none());
        // Anthropic's fast mode needs its beta header, returned beside the body.
        let fast = ModelOptions {
            fast_mode: true,
            reasoning_effort: None,
        };
        let (body, headers) = outbound_body(
            &anthropic,
            "claude-x",
            fast,
            serde_json::json!({"max_tokens": 9}),
            false,
        )
        .unwrap();
        assert_eq!(body["speed"], "fast");
        assert_eq!(headers, [("anthropic-beta", "fast-mode-2026-02-01")]);

        assert!(
            shaped(
                &hosted_openai(),
                "vendor/model-a",
                &none,
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

    /// The real accept loop on its own runtime, serving `listener` (peer
    /// check stubbed: other hosts refuse every peer, and the check itself is
    /// unchanged). `workers` of one proves concurrency comes from serving
    /// clients as tasks, not from a thread each.
    fn serve_in_background(listener: UnixListener, workers: usize) {
        listener.set_nonblocking(true).unwrap();
        let http = Arc::new(provider_client().unwrap());
        std::thread::spawn(move || {
            let runtime = tilo_rt::daemon("gateway-test")
                .workers(workers)
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::UnixListener::from_std(listener).unwrap();
                serve_on(listener, http, MAX_CLIENTS, |_| true).await
            })
        });
    }

    fn serve_test_socket() -> PathBuf {
        let nonce = tilo_clock::unix_now_ns();
        let path = std::env::temp_dir().join(format!("gw-{}-{nonce}.sock", std::process::id()));
        serve_in_background(UnixListener::bind(&path).unwrap(), 2);
        path
    }

    fn settings_client(path: &Path) -> UnixStream {
        let mut stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        write_frame(
            &mut stream,
            &serde_json::json!({"protocol": 2, "principal": "app", "app_id": AGENT_APP}),
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
        let response: Response = read_frame(stream).unwrap();
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
    fn a_stalled_client_does_not_hold_up_another() {
        use std::io::Write as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gateway.sock");
        serve_in_background(UnixListener::bind(&path).unwrap(), 1);

        // Connected first, then silent partway through its authentication.
        let mut stalled = UnixStream::connect(&path).unwrap();
        stalled.write_all(&[0x20]).unwrap();

        let mut client = UnixStream::connect(&path).unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let handoff = Principal::App {
            app_id: AGENT_APP.into(),
        }
        .handoff()
        .unwrap();
        client
            .write_all(&u32::try_from(handoff.len()).unwrap().to_le_bytes())
            .unwrap();
        client.write_all(&handoff).unwrap();
        write_frame(
            &mut client,
            &Frame {
                version: RPC_VERSION,
                value: Request::SecretStatus {
                    profile_id: "hosted".into(),
                },
            },
        )
        .unwrap();
        let response: Response = read_frame(&mut client).unwrap();
        assert!(!response.ok);
        assert_eq!(
            response.error.as_deref(),
            Some("caller is not authorized for this gateway operation")
        );
        drop(stalled);
    }
}

#[cfg(test)]
mod plan_tests;

#[cfg(test)]
mod selection_tests;
