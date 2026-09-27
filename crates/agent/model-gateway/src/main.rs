//! Per-user hosted model gateway.
//!
//! This is a narrow OpenAI-compatible/Anthropic relay for a profile-selected
//! planning call. It does not include a general `LiteLLM` control plane.

use std::fs;
use std::future::Future;
use std::os::fd::FromRawFd;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use accounts::oauth::ClientSlot;
use accounts::{AccountId, Secret, SecretVault};
use agent_provider_config::assign::{self, Inputs, ModelAssignments, Resolution};
use agent_provider_config::tasks::{self, Consumer, TaskRequest};
use agent_provider_config::{
    AZURE_OPENAI_SCOPE, Credential, EndpointCustody, LocalRuntime, ModelCapability, ProviderConfig,
    ProviderProfile, ProviderProtocol,
};
use base64::Engine as _;
use clap::{Parser, Subcommand};
use tilo_model_gateway::{
    ClientError, CredentialState, FaceImage, Frame, Principal, ProviderHealth, RPC_VERSION,
    Reachability, Request, Response, Route, VisionImage,
};
use tokio::net::UnixStream;
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
/// How many clients the gateway serves at once. Its callers are a few
/// same-user apps; past this, new connections wait in the listen backlog.
const MAX_CLIENTS: usize = 64;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);

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
            serve_on(listener, http, MAX_CLIENTS).await
        })
        .map_err(|error| error.to_string())?
}

/// The one HTTP client every provider request shares, so connections and
/// TLS sessions to a provider are reused across requests and clients.
fn provider_client() -> Result<reqwest::Client, String> {
    tilo_net::http::client()
        .connect_timeout(CONNECT_TIMEOUT)
        .build()
        .map_err(|_| "gateway HTTP client is unavailable".to_string())
}

/// Serve clients concurrently, each as its own task, at most `limit` at
/// once. Returns only when accepting fails.
async fn serve_on(
    listener: tokio::net::UnixListener,
    http: Arc<reqwest::Client>,
    limit: usize,
) -> Result<(), String> {
    let slots = Arc::new(Semaphore::new(limit.max(1)));
    loop {
        let slot = Arc::clone(&slots)
            .acquire_owned()
            .await
            .map_err(|error| error.to_string())?;
        let (stream, _) = listener.accept().await.map_err(|error| error.to_string())?;
        let http = Arc::clone(&http);
        tokio::spawn(async move {
            serve_client(stream, &http).await;
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

async fn serve_client(mut stream: UnixStream, http: &reqwest::Client) {
    if !same_user(&stream) {
        return;
    }
    let Ok(principal) = read_handoff(&mut stream).await else {
        return;
    };
    loop {
        let response = match read_frame::<Frame<Request>>(&mut stream).await {
            Ok(frame) if frame.version == RPC_VERSION => {
                handle(&principal, frame.value, http).await
            }
            Ok(_) => rejected("unsupported gateway RPC version"),
            Err(_) => return,
        };
        if write_frame(&mut stream, &response).await.is_err() {
            return;
        }
    }
}

async fn write_frame<T: serde::Serialize + Sync>(
    stream: &mut UnixStream,
    value: &T,
) -> Result<(), ClientError> {
    let body = Zeroizing::new(
        serde_json::to_vec(value).map_err(|error| ClientError::Protocol(error.to_string()))?,
    );
    tilo_model_gateway::FRAMING
        .write_async(stream, &body)
        .await
        .map_err(|error| match error {
            wire_frame::FrameError::Io(_) => ClientError::Unavailable,
            _ => ClientError::Protocol("response exceeds gateway frame limit".into()),
        })
}

async fn read_frame<T: for<'a> serde::Deserialize<'a>>(
    stream: &mut UnixStream,
) -> Result<T, ClientError> {
    let mut body = Zeroizing::new(Vec::new());
    match tilo_model_gateway::FRAMING
        .read_into_async(stream, &mut body)
        .await
    {
        Ok(true) => {}
        Err(wire_frame::FrameError::TooLarge(_)) => {
            return Err(ClientError::Protocol("gateway frame exceeds limit".into()));
        }
        Ok(false) | Err(_) => return Err(ClientError::Unavailable),
    }
    serde_json::from_slice(&body)
        .map_err(|_| ClientError::Protocol("invalid gateway request".into()))
}

/// Read the connection's [`Principal`]. An app principal is written by the
/// launcher before the app holds the descriptor; the session principal can
/// only come from a process that reaches this socket, which no sandbox does.
async fn read_handoff(stream: &mut UnixStream) -> Result<Principal, String> {
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

async fn handle(principal: &Principal, request: Request, http: &reqwest::Client) -> Response {
    let app_id = principal.app_id().unwrap_or_default();
    let session = *principal == Principal::Session;
    let output = |output| Response {
        ok: true,
        output: Some(output),
        ..Response::default()
    };
    let done = |()| Response {
        ok: true,
        ..Response::default()
    };
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
        Request::Plan {
            task,
            agent,
            protocol,
            body,
            timeout_ms,
        } => plan(
            http,
            consumer,
            task,
            agent,
            protocol,
            body,
            Duration::from_millis(timeout_ms),
        )
        .await
        .map(output),
        Request::Transform {
            task,
            agent,
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
                instructions,
                input,
                max_output_tokens,
                timeout: Duration::from_millis(timeout_ms),
            },
        )
        .await
        .map(output),
        Request::AnalyzeImage { image } => analyze_image(http, consumer, image).await.map(output),
        Request::ClusterFaces { images } => cluster_faces(http, consumer, images).await.map(output),
        Request::ReviewTrace { trace } if session => {
            review::review_trace(http, trace).await.map(output)
        }
        Request::ReviewTrace { .. } => {
            Err("only the desktop session may request feedback review".into())
        }
        Request::SecretStatus { profile_id } if app_id == SETTINGS_APP => {
            blocking(move || secret_status(&profile_id))
                .await
                .map(|configured| Response {
                    ok: true,
                    configured: Some(configured),
                    ..Response::default()
                })
        }
        Request::SetApiKey {
            profile_id,
            api_key,
        } if app_id == SETTINGS_APP => blocking(move || set_key(&profile_id, api_key))
            .await
            .map(done),
        Request::RemoveApiKey { profile_id } if app_id == SETTINGS_APP => {
            blocking(move || remove_key(&profile_id)).await.map(done)
        }
        Request::Probe { profile_id } if app_id == SETTINGS_APP => {
            probe(http, profile_id).await.map(|health| Response {
                ok: true,
                health: Some(health),
                ..Response::default()
            })
        }
        _ => Err("caller is not authorized for this gateway operation".into()),
    };
    result.unwrap_or_else(|error| rejected(&error))
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
    let config = ProviderConfig::load();
    let assignments = ModelAssignments::load();
    let local = LocalRuntime::load();
    assign::resolve(
        &Inputs {
            config: &config,
            assignments: &assignments,
            hardware: local.hardware.as_ref(),
        },
        consumer,
        task,
        agent,
    )
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

fn set_key(profile_id: &str, api_key: String) -> Result<(), String> {
    let _ = configured_profile(profile_id)?;
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
    match &profile.credential {
        None => Ok(Bearer::None),
        Some(Credential::ApiKey { .. }) => SecretVault::open()
            .get(&vault_id(&profile.id)?)
            .map(|secret| Zeroizing::new(secret.password))
            .filter(|secret| !secret.is_empty())
            .map(Bearer::ApiKey)
            .ok_or_else(|| "API key is not configured".to_string()),
        Some(Credential::Account { account_id }) => {
            let client = ClientSlot::Entra
                .registration()
                .ok_or("Microsoft sign-in is not configured on this device")?
                .client(AZURE_OPENAI_SCOPE);
            let mut vault = SecretVault::open();
            accounts::oauth::fresh_access_token(
                &mut vault,
                &AccountId::new(account_id.clone()),
                &client,
            )
            .map(Bearer::Token)
        }
    }
}

const MAX_PROVIDER_TIMEOUT: Duration = Duration::from_secs(60);
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

async fn plan(
    http: &reqwest::Client,
    consumer: Consumer,
    task: String,
    agent: Option<String>,
    protocol: ProviderProtocol,
    mut body: serde_json::Value,
    requested_timeout: Duration,
) -> Result<String, String> {
    let (resolution, bearer) =
        blocking(move || plan_route(&consumer, &task, agent.as_deref(), protocol)).await?;
    // The assignment, not the caller, names the model and its options.
    let Some(object) = body.as_object_mut() else {
        return Err("provider request body must be a JSON object".into());
    };
    object.insert(
        "model".into(),
        serde_json::Value::String(resolution.model.id.clone()),
    );
    let headers = resolution.options.apply(protocol, &mut body);
    let http = &profile_client(http, &resolution.profile)?;
    let endpoint = provider_url(&resolution.profile);
    let timeout = requested_timeout.min(MAX_PROVIDER_TIMEOUT);
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
    result
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

/// Resolve the transform task for `consumer` and read its credential. Only
/// the task served by the transform verb may be named here; a caller-dialect
/// or another fixed task goes through its own verb.
fn transform_route(
    consumer: &Consumer,
    task: &str,
    agent: Option<&str>,
) -> Result<(Resolution, Bearer), String> {
    if task != tasks::AGENT_TRANSFORM.id {
        return Err("this task is not served through the transform operation".into());
    }
    let resolution = resolve_for(consumer, task, agent)?;
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
    let (resolution, bearer) =
        blocking(move || transform_route(&consumer, &task, agent.as_deref())).await?;
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
            });
        }
        (ProviderProtocol::AzureOpenAi, Bearer::ApiKey(key)) => {
            request = request.header("api-key", key.as_str());
        }
        (ProviderProtocol::AzureOpenAi, Bearer::None) => {
            return Err(ProviderFailure {
                why: "Azure OpenAI requests require a key or a signed-in account".into(),
                again: Again::Never,
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
        })?;
    let status = response.status();
    if !status.is_success() {
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok());
        return Err(ProviderFailure {
            why: format!("provider returned HTTP {}", status.as_u16()),
            again: again_after(status.as_u16(), retry_after),
        });
    }
    response.text().await.map_err(|_| ProviderFailure {
        why: "provider response was unreadable".into(),
        again: Again::Never,
    })
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
        let ProviderFailure { why, again } = match attempt(budget).await {
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
fn same_user(stream: &UnixStream) -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    stream.peer_cred().is_ok_and(|peer| peer.uid() == euid)
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use agent_provider_config::{EndpointLocation, ModelDescriptor};

    use super::*;

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
            let mut gateway = UnixStream::from_std(gateway).unwrap();
            read_handoff(&mut gateway).await
        })
    }

    /// The handoff: a little-endian `u32` length, then JSON. The launcher
    /// writes the app form, the desktop session the session form.
    #[test]
    fn handoff_bytes_are_pinned() {
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

    #[test]
    fn local_verbs_run_only_on_the_pinned_supervised_runtime() {
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

    fn ask(input: &str) -> TransformAsk {
        TransformAsk {
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
    fn the_transform_verb_serves_only_the_transform_task() {
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
                    }),
                })
            },
        ))
        .unwrap();
        (result, fake.into_inner())
    }

    #[test]
    fn transient_provider_failures_are_retried_at_the_gateway() {
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

    #[test]
    fn retries_never_consume_room_needed_for_an_answer() {
        let (result, fake) = run_retries(Duration::from_secs(3), &[Err((529, Some("1")))]);
        assert!(result.is_err());
        assert_eq!(fake.calls, 1);
        assert!(fake.waits.is_empty());
    }

    #[test]
    fn a_stalled_client_does_not_hold_up_another() {
        use std::io::Write as _;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gateway.sock");
        let listener = UnixListener::bind(&path).unwrap();
        listener.set_nonblocking(true).unwrap();
        // One worker: concurrency has to come from serving clients as
        // tasks, not from a thread each.
        let runtime = tilo_rt::daemon("gateway-test").workers(1).build().unwrap();
        let http = Arc::new(provider_client().unwrap());
        runtime.spawn(async move {
            let listener = tokio::net::UnixListener::from_std(listener).unwrap();
            serve_on(listener, http, MAX_CLIENTS).await
        });

        // Connected first, then silent partway through its authentication.
        let mut stalled = std::os::unix::net::UnixStream::connect(&path).unwrap();
        stalled.write_all(&[0x20]).unwrap();

        let mut client = std::os::unix::net::UnixStream::connect(&path).unwrap();
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
        tilo_model_gateway::write_frame(
            &mut client,
            &Frame {
                version: RPC_VERSION,
                value: Request::SecretStatus {
                    profile_id: "hosted".into(),
                },
            },
        )
        .unwrap();
        let response: Response = tilo_model_gateway::read_frame(&mut client).unwrap();
        assert!(!response.ok);
        assert_eq!(
            response.error.as_deref(),
            Some("caller is not authorized for this gateway operation")
        );
        drop(stalled);
    }
}
