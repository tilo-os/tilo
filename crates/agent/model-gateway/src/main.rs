//! Per-user hosted model gateway.
//!
//! This is a narrow OpenAI-compatible/Anthropic relay for a profile-selected
//! planning call. It does not include a general `LiteLLM` control plane.

use std::fs;
use std::os::fd::{AsRawFd, FromRawFd};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use accounts::{AccountId, Secret, SecretVault};
use agent_provider_config::{EndpointLocation, ProviderConfig, ProviderProfile, ProviderProtocol};
use base64::Engine as _;
use clap::{Parser, Subcommand};
use tilo_model_gateway::{
    FaceImage, Frame, RPC_VERSION, Request, Response, VisionImage, read_frame, write_frame,
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
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => serve_client(stream),
            Err(error) => return Err(error.to_string()),
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

fn serve_client(mut stream: UnixStream) {
    if !same_user(&stream) {
        return;
    }
    let Ok(handoff) = read_handoff(&mut stream) else {
        return;
    };
    loop {
        let response = match read_frame::<Frame<Request>>(&mut stream) {
            Ok(frame) if frame.version == RPC_VERSION => handle(&handoff.app_id, frame.value),
            Ok(_) => rejected("unsupported gateway RPC version"),
            Err(_) => return,
        };
        if write_frame(&mut stream, &response).is_err() {
            return;
        }
    }
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

fn handle(app_id: &str, request: Request) -> Response {
    let result = match request {
        Request::Plan {
            profile_id,
            model_id,
            body,
            timeout_ms,
        } if app_id == AGENT_APP => plan(
            &profile_id,
            &model_id,
            &body,
            Duration::from_millis(timeout_ms),
        )
        .map(|output| Response {
            ok: true,
            output: Some(output),
            configured: None,
            error: None,
        }),
        Request::Plan { .. } => Err("only tilo-agent may plan through the model gateway".into()),
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
        }),
        Request::RemoveApiKey { profile_id } if app_id == SETTINGS_APP => remove_key(&profile_id)
            .map(|()| Response {
                ok: true,
                output: None,
                configured: None,
                error: None,
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

fn plan(
    profile_id: &str,
    model_id: &str,
    body: &serde_json::Value,
    requested_timeout: Duration,
) -> Result<String, String> {
    let profile = configured_profile(profile_id)?;
    if !profile.models.iter().any(|model| model.id == model_id) {
        return Err("model is not configured for this provider".into());
    }
    let mut secret = SecretVault::open()
        .get(&vault_id(profile_id)?)
        .map(|secret| Zeroizing::new(secret.password))
        .filter(|secret| !secret.is_empty())
        .ok_or_else(|| "API key is not configured".to_string())?;
    let endpoint = provider_url(&profile);
    let timeout = requested_timeout.min(MAX_PROVIDER_TIMEOUT);
    let began = Instant::now();
    let client = tilo_net::http::blocking_client()
        .connect_timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|_| "gateway HTTP client is unavailable".to_string())?;
    let result = while_busy(
        || timeout.checked_sub(began.elapsed()),
        std::thread::sleep,
        |budget| {
            provider_attempt(
                &client,
                &profile,
                &endpoint,
                Some(secret.as_str()),
                body,
                budget,
            )
        },
    );
    secret.zeroize();
    result
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
    let response = request.json(body).send().map_err(|error| ProviderFailure {
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
    response.text().map_err(|_| ProviderFailure {
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
        let ProviderFailure { why, again } = match attempt(budget) {
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

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
                    }),
                }
            },
        );
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
}
