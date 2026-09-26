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

use accounts::{AccountId, Secret, SecretVault};
use agent_provider_config::{ModelCapability, ProviderConfig, ProviderProfile, ProviderProtocol};
use base64::Engine as _;
use clap::{Parser, Subcommand};
use tilo_model_gateway::{
    ClientError, FaceImage, Frame, Principal, RPC_VERSION, Request, Response, VisionImage,
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
const AGENT_APP: &str = "so.tilo.agent";
const PHOTOS_APP: &str = "so.tilo.photos";
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
        configured: None,
        error: None,
    };
    let done = |()| Response {
        ok: true,
        output: None,
        configured: None,
        error: None,
    };
    let result = match request {
        Request::Plan {
            profile_id,
            model_id,
            body,
            timeout_ms,
        } if app_id == AGENT_APP => plan(
            http,
            profile_id,
            model_id,
            &body,
            Duration::from_millis(timeout_ms),
        )
        .await
        .map(output),
        Request::Plan { .. } => Err("only tilo-agent may plan through the model gateway".into()),
        Request::AnalyzeImage {
            profile_id,
            model_id,
            image,
        } if app_id == PHOTOS_APP => analyze_image(http, profile_id, model_id, image)
            .await
            .map(output),
        Request::AnalyzeImage { .. } => {
            Err("only Photos may request fixed local image analysis".into())
        }
        Request::ClusterFaces {
            profile_id,
            model_id,
            images,
        } if app_id == PHOTOS_APP => cluster_faces(http, profile_id, model_id, images)
            .await
            .map(output),
        Request::ClusterFaces { .. } => {
            Err("only Photos may request fixed local face clustering".into())
        }
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
                    output: None,
                    configured: Some(configured),
                    error: None,
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
    let _ = configured_profile(profile_id)?;
    let mut vault = SecretVault::open();
    vault.remove(&vault_id(profile_id)?);
    vault.save();
    Ok(())
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

/// The configured profile for a planning call and its API key.
fn plan_profile(
    profile_id: &str,
    model_id: &str,
) -> Result<(ProviderProfile, Zeroizing<String>), String> {
    let profile = configured_profile(profile_id)?;
    if !profile.models.iter().any(|model| model.id == model_id) {
        return Err("model is not configured for this provider".into());
    }
    let secret = SecretVault::open()
        .get(&vault_id(profile_id)?)
        .map(|secret| Zeroizing::new(secret.password))
        .filter(|secret| !secret.is_empty())
        .ok_or_else(|| "API key is not configured".to_string())?;
    Ok((profile, secret))
}

async fn plan(
    http: &reqwest::Client,
    profile_id: String,
    model_id: String,
    body: &serde_json::Value,
    requested_timeout: Duration,
) -> Result<String, String> {
    let (profile, mut secret) = blocking(move || plan_profile(&profile_id, &model_id)).await?;
    let endpoint = provider_url(&profile);
    let timeout = requested_timeout.min(MAX_PROVIDER_TIMEOUT);
    let began = Instant::now();
    let result = while_busy(
        || timeout.checked_sub(began.elapsed()),
        tokio::time::sleep,
        |budget| {
            provider_attempt(
                http,
                &profile,
                &endpoint,
                Some(secret.as_str()),
                body,
                budget,
            )
        },
    )
    .await;
    secret.zeroize();
    result
}

/// Validate Photos' one fixed local-only visual-analysis operation and
/// build its request. No arbitrary prompt crosses the app boundary: the
/// schema, token budget, and image URL shape are all constructed here after
/// validating the launcher-authenticated caller and model capability.
fn vision_request(
    profile_id: &str,
    model_id: &str,
    image: &VisionImage,
) -> Result<(ProviderProfile, serde_json::Value), String> {
    validate_vision_image(image)?;
    let profile = local_profile(
        configured_profile(profile_id)?,
        model_id,
        ModelCapability::Vision,
        "image analysis",
    )?;
    let body = local_vision_body(model_id, image);
    Ok((profile, body))
}

/// Run Photos' fixed visual-analysis operation (see [`vision_request`]).
async fn analyze_image(
    http: &reqwest::Client,
    profile_id: String,
    model_id: String,
    image: VisionImage,
) -> Result<String, String> {
    let (profile, body) = blocking(move || vision_request(&profile_id, &model_id, &image)).await?;
    let endpoint = provider_url(&profile);
    let began = Instant::now();
    // A local vLLM endpoint is normally unauthenticated. Do not invent a
    // bearer token, and never look up a hosted-provider secret for it.
    while_busy(
        || VISION_TIMEOUT.checked_sub(began.elapsed()),
        tokio::time::sleep,
        |budget| provider_attempt(http, &profile, &endpoint, None, &body, budget),
    )
    .await
}

/// Validate one fixed face-clustering request against the separate signed
/// local capability and build it.  An OpenAI-compatible local runner can
/// expose its face model behind the normal chat endpoint, but it must
/// explicitly advertise `face_recognition`; generic image description models
/// cannot use this path.
fn face_request(
    profile_id: &str,
    model_id: &str,
    images: &[FaceImage],
) -> Result<(ProviderProfile, serde_json::Value), String> {
    validate_face_images(images)?;
    let profile = local_profile(
        configured_profile(profile_id)?,
        model_id,
        ModelCapability::FaceRecognition,
        "face clustering",
    )?;
    let body = local_face_body(model_id, images);
    Ok((profile, body))
}

/// Run one fixed face-clustering request (see [`face_request`]).
async fn cluster_faces(
    http: &reqwest::Client,
    profile_id: String,
    model_id: String,
    images: Vec<FaceImage>,
) -> Result<String, String> {
    let (profile, body) = blocking(move || face_request(&profile_id, &model_id, &images)).await?;
    let endpoint = provider_url(&profile);
    let began = Instant::now();
    while_busy(
        || VISION_TIMEOUT.checked_sub(began.elapsed()),
        tokio::time::sleep,
        |budget| provider_attempt(http, &profile, &endpoint, None, &body, budget),
    )
    .await
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
    if !profile.is_device_local() {
        return Err(format!(
            "{purpose} requires a supervised loopback OpenAI-compatible model on this device"
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

async fn provider_attempt(
    client: &reqwest::Client,
    profile: &ProviderProfile,
    endpoint: &str,
    secret: Option<&str>,
    body: &serde_json::Value,
    budget: Duration,
) -> Result<String, ProviderFailure> {
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

fn provider_url(profile: &ProviderProfile) -> String {
    let base = profile.endpoint.trim_end_matches('/');
    match profile.protocol {
        ProviderProtocol::Anthropic if base.ends_with("/v1/messages") => base.into(),
        ProviderProtocol::Anthropic => format!("{base}/v1/messages"),
        ProviderProtocol::OpenAiCompatible if base.ends_with("/chat/completions") => base.into(),
        ProviderProtocol::OpenAiCompatible => format!("{base}/chat/completions"),
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
    fn local_verbs_run_only_on_a_pinned_loopback_model() {
        let pinned = "a".repeat(40);
        for (capability, purpose) in VERBS {
            let run = |profile| local_profile(profile, "m", capability, purpose);
            assert!(
                run(profile(
                    EndpointLocation::ThisDevice,
                    "http://127.0.0.1:8123/v1",
                    Some(&pinned)
                ))
                .is_ok()
            );
            assert!(
                run(profile(
                    EndpointLocation::ThisDevice,
                    "http://[::1]:8123/v1",
                    Some(&pinned)
                ))
                .is_ok()
            );
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
                // Claiming to be local does not make a remote endpoint local.
                profile(
                    EndpointLocation::ThisDevice,
                    "https://models.example.test/v1",
                    Some(&pinned),
                ),
                profile(
                    EndpointLocation::ThisDevice,
                    "http://localhost.evil.test:8123/v1",
                    Some(&pinned),
                ),
                profile(
                    EndpointLocation::ThisDevice,
                    "http://127.0.0.1:8123/v1",
                    Some("main"),
                ),
                profile(
                    EndpointLocation::ThisDevice,
                    "http://127.0.0.1:8123/v1",
                    Some(&"A".repeat(40)),
                ),
                profile(
                    EndpointLocation::ThisDevice,
                    "http://127.0.0.1:8123/v1",
                    None,
                ),
            ] {
                assert!(run(refused.clone()).is_err(), "{purpose}: {refused:?}");
            }
            let mut undeclared = profile(
                EndpointLocation::ThisDevice,
                "http://127.0.0.1:8123/v1",
                Some(&pinned),
            );
            undeclared.models = vec![ModelDescriptor {
                revision: Some(pinned.clone()),
                ..ModelDescriptor::new("m")
            }];
            assert!(run(undeclared).is_err(), "{purpose} must be declared");
            assert!(
                local_profile(
                    profile(
                        EndpointLocation::ThisDevice,
                        "http://127.0.0.1:8123/v1",
                        Some(&pinned)
                    ),
                    "other",
                    capability,
                    purpose
                )
                .is_err()
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
                profile_id: "hosted".into(),
                model_id: "m".into(),
                body: serde_json::Value::Null,
                timeout_ms: 1,
            },
            Request::AnalyzeImage {
                profile_id: "local".into(),
                model_id: "m".into(),
                image: vision_image(),
            },
        ];
        for request in refused {
            let response = handled(&Principal::Session, request);
            assert!(!response.ok);
            let error = response.error.unwrap_or_default();
            assert!(
                error.starts_with("only ")
                    || error == "caller is not authorized for this gateway operation",
                "{error}"
            );
        }
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
