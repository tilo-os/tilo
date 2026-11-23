//! Per-user model gateway.
//!
//! An app's model turn (`Complete`, `CompleteDefault`) is the
//! person's own model on their workspace's server, asked through the session
//! this computer holds with it ([`server_asks`]): this computer keeps no
//! provider key. What stays here runs on this computer alone: local vision
//! and face models, on-device picture segmentation, and the description of
//! the providers Settings shows. A trusted server operator's own provider
//! calls share the provider transport (`host_backend`).

#[cfg(feature = "local-models")]
#[path = "subject.rs"]
mod subject;

use std::fs;
#[cfg(test)]
use std::io::Read;
use std::os::unix::net::UnixListener;
#[cfg(test)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(feature = "host-backend")]
#[path = "host_backend.rs"]
pub mod host_backend;
#[path = "provider_call.rs"]
mod provider_call;
#[path = "road.rs"]
mod road;
#[path = "server_asks.rs"]
pub mod server_asks;
#[path = "signs.rs"]
mod signs;
use signs::Signs;

use crate::{
    EVENT_TOOL_CALL, EVENT_TOOL_CALL_DELTA, FRAMING, FaceImage, Frame, MAX_DELTA_EVENTS,
    MAX_STREAM_EVENTS, PlanTrace, RPC_VERSION, Request, Response, TraceEvent, VisionImage,
    is_pinned_revision, is_supervised_loopback,
};
#[cfg(test)]
use crate::{read_frame, write_frame};
use account_credentials::{Capability, CredentialError, CredentialStore, OAuthIssuer, OAuthUse};
use accounts::AccountId;
use agent_provider_config::{
    EndpointLocation, ProviderConfig, ProviderProfile, ProviderProtocol, SignInIssuer,
    transform as provider_transform,
};
use base64::Engine as _;
use clap::{Parser, Subcommand};
use std::future::Future;
use tilo_lock::MutexRecover as _;
use tokio::io::AsyncReadExt as _;
use tokio::sync::Semaphore;
use zeroize::Zeroizing;

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
}

/// The launcher's handoff: a little-endian `u32` length, then JSON.
const HANDOFF_FRAMING: wire_frame::Framing = wire_frame::Framing::little_endian(4096);
use tilo_app_id::apps::AGENT as AGENT_APP;
use tilo_app_id::apps::CANVAS as CANVAS_APP;
use tilo_app_id::apps::CHAT as CHAT_APP;
use tilo_app_id::apps::PHOTOS as PHOTOS_APP;
#[cfg(test)]
use tilo_app_id::apps::SETTINGS as SETTINGS_APP;
/// The session's own image model service: a platform package, not an
/// installed app, so it has no first-party app constant. It asks the fixed
/// local image and face operations for every app, as Photos once did.
const VISION_APP: &str = "so.tilo.vision";
/// The desktop's report composer: its review asks a model which words of a
/// report are personal, a completion without tools, as Chat's are.
const FEEDBACK_APP: &str = "so.tilo.feedback-composer";
/// The session's history: what its changes were, and the names over the runs
/// of them, asked as one completion without tools, as Chat's are.
use tilo_app_id::apps::ACTIVITY as ACTIVITY_APP;
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

#[derive(serde::Deserialize)]
struct Handoff {
    protocol: u16,
    app_id: String,
}

pub fn main() {
    let cli = Cli::parse();
    // The service follows the settings its session holds.
    crate::follow_session_settings();
    let result = match cli.action {
        Action::Serve { socket } => serve(&socket),
    };
    if let Err(error) = result {
        // Never interpolate a request or provider response here; either could
        // contain a bearer value or user document content.
        eprintln!("tilo-model-gateway: {error}");
        std::process::exit(1);
    }
}

fn serve(socket: &Path) -> Result<(), String> {
    // SAFETY: activation is taken over before the runtime starts any thread.
    let activated = unsafe { tilo_files::native::private::activated_socket(socket) }
        .map_err(|error| error.to_string())?;
    let listener = if let Some(listener) = activated {
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
            serve_on(listener, http, MAX_CLIENTS, HANDOFF_TIMEOUT, same_user).await
        })
        .map_err(|error| error.to_string())?
}

/// The one keep-alive HTTP client every provider request shares, so
/// connections and TLS sessions to a provider are reused across requests and
/// clients, and consecutive model turns skip the handshake.
pub(super) fn provider_client() -> Result<reqwest::Client, String> {
    provider_builder()
        .build()
        .map_err(|_| "gateway HTTP client is unavailable".to_string())
}

/// [`provider_client`] before it is built, for a transport that adds a rule
/// of its own to it.
fn provider_builder() -> reqwest::ClientBuilder {
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
}

/// Serve clients concurrently, each as its own task, at most `limit` at
/// once, so one idle, slow or long-running client (a Plan in flight, a
/// stalled handoff) never delays another. `authenticate` is the same-user
/// peer check in production. Returns only when accepting fails.
async fn serve_on(
    listener: tokio::net::UnixListener,
    http: Arc<reqwest::Client>,
    limit: usize,
    handoff: Duration,
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
            serve_connection_within(stream, &http, handoff).await;
            drop(slot);
        });
    }
}

fn bind_listener(socket: &Path) -> Result<UnixListener, String> {
    if let Some(parent) = socket.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    tilo_files::native::private::bind_socket(socket).map_err(|error| error.to_string())
}

/// One authenticated connection: the launcher handoff, then framed requests
/// in order. Framing, caller authorization and response shapes are unchanged
/// from the single-threaded server.
struct StopReader(tokio::task::JoinHandle<()>);
impl Drop for StopReader {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn negotiate_cancellation(
    request: Request,
    enabled: &mut bool,
    writer: &mut tokio::net::unix::OwnedWriteHalf,
) -> Result<Option<(Option<u64>, Request)>, ()> {
    let selected = match request {
        Request::CancellationSupported => {
            *enabled = true;
            if send_frame(writer, &succeeded(Some("cancel-v1".into()), None))
                .await
                .is_err()
            {
                return Err(());
            }
            return Ok(None);
        }
        Request::Cancel { id } if *enabled => {
            if send_frame(writer, &succeeded(Some(format!("cancelled:{id}")), None))
                .await
                .is_err()
            {
                return Err(());
            }
            return Ok(None);
        }
        Request::Cancellable { id, request }
            if *enabled
                && !matches!(
                    *request,
                    Request::Cancellable { .. }
                        | Request::Cancel { .. }
                        | Request::CancellationSupported
                ) =>
        {
            (Some(id), *request)
        }
        request => (None, request),
    };

    Ok(Some(selected))
}

fn management_mutation(request: &Request) -> bool {
    matches!(request, Request::SetProviderConfig { .. })
}

#[cfg(test)]
async fn serve_connection(stream: tokio::net::UnixStream, http: &reqwest::Client) {
    serve_connection_within(stream, http, HANDOFF_TIMEOUT).await;
}

/// Serve one connection, closing it if its launcher handoff has not arrived
/// within `handoff`.
async fn serve_connection_within(
    mut stream: tokio::net::UnixStream,
    http: &reqwest::Client,
    handoff: Duration,
) {
    let Ok(Ok(handoff)) = tokio::time::timeout(handoff, read_handoff(&mut stream)).await else {
        return;
    };
    let (mut reader, mut writer) = stream.into_split();
    // One reader owns partial frames throughout provider work. Never cancel a
    // read halfway through a prefix/body and then restart it on the same stream.
    let (incoming, mut received) = tokio::sync::mpsc::channel(8);
    let reader = tokio::spawn(async move {
        loop {
            let Ok(body) = read_request_frame(&mut reader).await else {
                break;
            };
            let Ok(frame) = serde_json::from_slice::<Frame<Request>>(&body) else {
                break;
            };
            if frame.version != RPC_VERSION || incoming.send(frame.value).await.is_err() {
                break;
            }
        }
    });
    let _reader = StopReader(reader);
    let mut pending = std::collections::VecDeque::new();
    let mut enabled = false;
    loop {
        let request = match pending.pop_front() {
            Some(request) => request,
            None => match received.recv().await {
                Some(request) => request,
                None => return,
            },
        };
        let (id, request) = match negotiate_cancellation(request, &mut enabled, &mut writer).await {
            Ok(Some(selected)) => selected,
            Ok(None) => continue,
            Err(()) => return,
        };
        // A pipelined cancellation can arrive while an earlier operation owns
        // the provider. Do not start its queued target and then ignore Stop.
        if let Some(id) = id
            && let Some(index) = pending.iter().position(
                |request| matches!(request, Request::Cancel { id: cancel } if *cancel == id),
            )
        {
            pending.remove(index);
            if send_frame(
                &mut writer,
                &succeeded(Some(format!("cancelled:{id}")), None),
            )
            .await
            .is_err()
            {
                return;
            }
            continue;
        }
        // Durable management work cannot be rolled back by dropping its future.
        // Finish it before acknowledging Stop; an abandoned response is not a
        // promise that the write was cancelled.
        let committing = management_mutation(&request);
        // The provider future emits without awaiting, so a whole burst of
        // frames (an answer the provider sent in one read) is queued before
        // the writer runs again. The queue holds every frame one request may
        // carry, which the protocol bounds; a full queue then means only a
        // gateway past that bound, never a writer that is merely behind —
        // which `emit` would report as the agent having left.
        let (events, mut queued) = tokio::sync::mpsc::channel::<Zeroizing<Vec<u8>>>(
            MAX_STREAM_EVENTS + MAX_DELTA_EVENTS + 1,
        );
        let handling = async {
            let mut emit = |event: &Response| {
                serde_json::to_vec(event)
                    .is_ok_and(|body| events.try_send(Zeroizing::new(body)).is_ok())
            };
            handle_streaming(&handoff.app_id, request, http, &mut emit).await
        };
        // Dropping handling immediately drops the provider future. Writes are
        // outside select, so cancellation never truncates a response frame.
        let response = {
            tokio::pin!(handling);
            loop {
                tokio::select! {
                    biased;
                    incoming = received.recv() => {
                        let Some(incoming) = incoming else { return };
                        match incoming {
                            Request::Cancel { id: cancel } if enabled && id == Some(cancel) && !committing => {
                                break succeeded(Some(format!("cancelled:{cancel}")), None);
                            }
                            other => {
                                if pending.len() >= 8 { return; }
                                pending.push_back(other);
                            }
                        }
                    }
                    response = &mut handling => break response,
                    Some(body) = queued.recv() => {
                        if send_body(&mut writer, &body).await.is_err() { return; }
                    }
                }
            }
        };
        // Queued presentation frames precede the terminal/barrier. Cancellation
        // discards these events: they cannot authorize any tool execution.
        drop(events);
        while let Ok(body) = queued.try_recv() {
            if send_body(&mut writer, &body).await.is_err() {
                return;
            }
        }
        if send_frame(&mut writer, &response).await.is_err() {
            return;
        }
    }
}

async fn send_frame<T: serde::Serialize + Sync>(
    stream: &mut (impl tokio::io::AsyncWrite + Unpin),
    value: &T,
) -> Result<(), ()> {
    let body = Zeroizing::new(serde_json::to_vec(value).map_err(|_| ())?);
    send_body(stream, &body).await
}

async fn send_body(
    stream: &mut (impl tokio::io::AsyncWrite + Unpin),
    body: &[u8],
) -> Result<(), ()> {
    match tokio::time::timeout(WRITE_TIMEOUT, FRAMING.write_async(stream, body)).await {
        Ok(Ok(())) => Ok(()),
        _ => Err(()),
    }
}

/// Read one length-prefixed request frame. Waiting for its first byte has no
/// limit (see [`FRAME_TIMEOUT`]); the rest must follow promptly. The buffer
/// is zeroized on drop because a frame can carry a person's words.
async fn read_request_frame(
    stream: &mut (impl tokio::io::AsyncRead + Unpin),
) -> Result<Zeroizing<Vec<u8>>, ()> {
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

async fn read_handoff(stream: &mut tokio::net::UnixStream) -> Result<Handoff, String> {
    let body = match HANDOFF_FRAMING.read_async(stream).await {
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
        || (!crate::caller::provider_admin_app(&handoff.app_id)
            && handoff.app_id != AGENT_APP
            && handoff.app_id != CHAT_APP
            && handoff.app_id != FEEDBACK_APP
            && handoff.app_id != ACTIVITY_APP
            && handoff.app_id != PHOTOS_APP
            && handoff.app_id != CANVAS_APP
            && handoff.app_id != VISION_APP)
    {
        return Err("launcher is not authorized for the model gateway".into());
    }
    Ok(handoff)
}

#[cfg(test)]
async fn handle(app_id: &str, request: Request, http: &reqwest::Client) -> Response {
    handle_streaming(app_id, request, http, &mut |_| true).await
}

/// A successful final response.
const fn succeeded(output: Option<String>, configured: Option<bool>) -> Response {
    Response {
        ok: true,
        output,
        configured,
        error: None,
        model_error: None,
        event: None,
    }
}

/// Authorize and serve one request. `emit` writes an intermediate streaming
/// frame and reports whether the client is still there; only a streaming
/// Plan calls it. Configuration and vault work, which reads and writes
/// files, runs on the runtime's bounded blocking pool.
pub(super) async fn handle_streaming(
    app_id: &str,
    request: Request,
    http: &reqwest::Client,
    emit: Emit<'_>,
) -> Response {
    let output = |output| succeeded(Some(output), None);
    let done = |()| succeeded(None, None);
    let result = match request {
        request @ (Request::Complete { .. } | Request::CompleteDefault { .. })
            if app_id == AGENT_APP
                || ((app_id == CHAT_APP || app_id == FEEDBACK_APP || app_id == ACTIVITY_APP)
                    && crate::caller::chat_completion(&request)) =>
        {
            return complete_request(request, emit)
                .await
                .map_or_else(Response::failure, output);
        }
        Request::Complete { .. } | Request::CompleteDefault { .. } => return permission_denied(),
        Request::GetProviderConfig | Request::SetProviderConfig { .. }
            if !crate::caller::provider_admin_app(app_id) =>
        {
            return permission_denied();
        }
        Request::Trace {
            trace,
            event,
            at_unix_ms,
        } if app_id == AGENT_APP => {
            log_mark(trace, event, at_unix_ms);
            Ok(done(()))
        }
        Request::AnalyzeImage {
            profile_id,
            model_id,
            image,
        } if app_id == PHOTOS_APP || app_id == VISION_APP => {
            analyze_image(http, profile_id, model_id, image)
                .await
                .map(output)
        }
        Request::AnalyzeImage { .. } => {
            Err("only Photos may request fixed local image analysis".into())
        }
        Request::ClusterFaces {
            profile_id,
            model_id,
            images,
        } if app_id == PHOTOS_APP || app_id == VISION_APP => {
            cluster_faces(http, profile_id, model_id, images)
                .await
                .map(output)
        }
        Request::ClusterFaces { .. } => {
            Err("only Photos may request fixed local face clustering".into())
        }
        Request::SegmentSubject { image } if app_id == CANVAS_APP || app_id == PHOTOS_APP => {
            segment_request(image).await.map(output)
        }
        Request::SegmentSubject { .. } => {
            Err("only Canvas and Photos may find a picture's subject".into())
        }
        request if crate::caller::provider_admin_app(app_id) => {
            settings_request(http, request).await
        }
        _ => Err("caller is not authorized for this gateway operation".into()),
    };
    result.unwrap_or_else(|error| rejected(&error))
}

fn segment_request(image: VisionImage) -> impl Future<Output = Result<String, String>> {
    #[cfg(feature = "local-models")]
    {
        blocking(move || subject::segment(&image))
    }
    #[cfg(not(feature = "local-models"))]
    {
        drop(image);
        std::future::ready(Err(
            "on-device subject segmentation is unavailable in this gateway build".into(),
        ))
    }
}

/// Credential operations are reachable only after the Settings caller check.
async fn settings_request(http: &reqwest::Client, request: Request) -> Result<Response, String> {
    let output = |output| succeeded(Some(output), None);
    match request {
        Request::ListModels { profile_id } => Ok(list_models(http, profile_id)
            .await
            .and_then(|models| {
                serde_json::to_string(&models)
                    .map_err(|_| ModelError::new(ErrorKind::InvalidResponse))
            })
            .map_or_else(Response::failure, output)),
        request @ (Request::GetProviderConfig | Request::SetProviderConfig { .. }) => {
            Ok(administration::configuration_request(request).await)
        }
        _ => Err("caller is not authorized for this gateway operation".into()),
    }
}

/// The most of a model list read.
const MAX_MODEL_LIST_BYTES: usize = 2 * 1024 * 1024;
/// How long a model list may take.
const MODEL_LIST_TIMEOUT: Duration = Duration::from_secs(20);

/// The models `profile_id`'s provider lists, asked with a token minted from
/// its sign-in or, for a server on this computer, as itself, enabled or not:
/// answering is also the proof that the endpoint and sign-in work. A provider without a model
/// list answers [`ErrorKind::Unsupported`]; the model is then typed.
async fn list_models(
    http: &reqwest::Client,
    profile_id: String,
) -> Result<Vec<agent_provider_config::ModelDescriptor>, ModelError> {
    let (profile, secret) = blocking_model(move || {
        let profile = credential_profile(&profile_id)
            .map_err(|_| ModelError::new(ErrorKind::InvalidRequest))?;
        let secret = if profile.sign_in.is_some() {
            Some(
                sign_in_token(&profile, tilo_clock::unix_now(), Instant::now())
                    .map_err(|_| ModelError::new(ErrorKind::Authentication))?
                    .0,
            )
        } else if unauthenticated_local_plan(&profile)? {
            None
        } else {
            // This computer keeps no provider key: the person's keyed
            // providers are listed where their workspace keeps the keys.
            return Err(resolution_error(
                ErrorKind::Authentication,
                "this computer keeps no provider keys; your workspace's server keeps them",
            ));
        };
        Ok((profile, secret))
    })
    .await?;
    let url = provider_transform::models_url(&profile)
        .ok_or_else(|| ModelError::new(ErrorKind::Unsupported))?;
    let headers =
        provider_transform::profile_headers(&profile, secret.as_ref().map(|key| key.as_str()))
            .map_err(|_| ModelError::new(ErrorKind::Authentication))?;
    drop(secret);
    let mut request = http.get(url).timeout(MODEL_LIST_TIMEOUT);
    for header in headers {
        let mut value = reqwest::header::HeaderValue::from_str(header.value.as_str())
            .map_err(|_| ModelError::new(ErrorKind::Authentication))?;
        value.set_sensitive(header.secret);
        request = request.header(header.name.as_ref(), value);
    }
    let mut response = request.send().await.map_err(|error| {
        ModelError::new(if error.is_timeout() {
            ErrorKind::TimedOut
        } else {
            ErrorKind::Transport
        })
    })?;
    let status = response.status();
    if !status.is_success() {
        let detail = error_body(response).await;
        return Err(match status.as_u16() {
            404 | 405 => ModelError::new(ErrorKind::Unsupported),
            code => {
                crate::openai::Failure::from_http(code, &String::from_utf8_lossy(&detail), None)
                    .model_error()
            }
        });
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| ModelError::new(ErrorKind::Transport))?
    {
        if body.len().saturating_add(chunk.len()) > MAX_MODEL_LIST_BYTES {
            return Err(ModelError::new(ErrorKind::InvalidResponse));
        }
        body.extend_from_slice(&chunk);
    }
    provider_transform::parse_models(&String::from_utf8_lossy(&body))
        .map_err(|_| ModelError::new(ErrorKind::InvalidResponse))
}

/// Run a provider-neutral request after the launcher caller was authorized:
/// a turn of the person's own model on their workspace's server
/// ([`server_asks`]). The provider and model a request names are this
/// computer's own settings, which keep no key: the server answers with the
/// model the person chose there.
async fn complete_request(
    request: Request,
    emit: Emit<'_>,
) -> Result<String, agent_provider_config::protocol::ModelError> {
    let (Request::Complete {
        request,
        timeout_ms,
        ..
    }
    | Request::CompleteDefault {
        request,
        timeout_ms,
        ..
    }) = request
    else {
        return Err(ModelError::new(ErrorKind::InvalidRequest));
    };
    let answer = server_asks::ask(&request, Duration::from_millis(timeout_ms), emit).await?;
    serde_json::to_string(&answer).map_err(|_| ModelError::new(ErrorKind::InvalidResponse))
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

use agent_provider_config::protocol::{ErrorKind, ModelError};
#[path = "administration.rs"]
mod administration;
#[cfg(test)]
use administration::provider_snapshot;

fn permission_denied() -> Response {
    Response::failure(ModelError::new(ErrorKind::PermissionDenied))
}

fn resolution_error(kind: ErrorKind, message: &str) -> ModelError {
    ModelError {
        kind,
        message: message.into(),
    }
}

async fn blocking_model<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ModelError> + Send + 'static,
) -> Result<T, ModelError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| ModelError::new(ErrorKind::Unavailable))?
}

fn rejected(error: &str) -> Response {
    Response {
        ok: false,
        output: None,
        configured: None,
        error: Some(error.into()),
        model_error: None,
        event: None,
    }
}

fn configured_profile(profile_id: &str) -> Result<ProviderProfile, ModelError> {
    let config = ProviderConfig::load();
    if !config.validation_errors().is_empty() {
        return Err(resolution_error(
            ErrorKind::InvalidRequest,
            "provider configuration is invalid",
        ));
    }
    config
        .profiles
        .into_iter()
        .find(|profile| profile.id == profile_id && profile.enabled)
        .ok_or_else(|| resolution_error(ErrorKind::InvalidRequest, "provider is not enabled"))
}

/// Credential management must also work for disabled profiles.
fn credential_profile(profile_id: &str) -> Result<ProviderProfile, String> {
    ProviderConfig::load()
        .profiles
        .into_iter()
        .find(|profile| profile.id == profile_id)
        .ok_or_else(|| "provider is no longer available".into())
}

/// Vault writes are read-modify-write of one file; requests run on separate
/// blocking-pool threads, so they are serialized.
static VAULT_WRITES: Mutex<()> = Mutex::new(());

// HTTP and descriptor gateways can be separate user services. Serialize their
// description/credential transactions too, not only threads in one process.
struct ManagementWrite {
    _thread: std::sync::MutexGuard<'static, ()>,
    _file: tilo_files::Lock,
}

fn management_write() -> Result<ManagementWrite, String> {
    let thread = VAULT_WRITES.lock_recover();
    let lock = || -> std::io::Result<tilo_files::Lock> {
        let root =
            accounts::config_root().ok_or_else(|| std::io::Error::other("no config root"))?;
        fs::create_dir_all(&root)?;
        let (dir, name) = tilo_files::file_at(&root.join("provider-administration.lock"))
            .map_err(std::io::Error::other)?;
        tilo_files::blocking::lock(&dir, &name, tilo_files::LockMode::Exclusive)
            .map_err(std::io::Error::other)
    };
    let file = lock().map_err(|_| "provider administration storage is unavailable".to_string())?;
    Ok(ManagementWrite {
        _thread: thread,
        _file: file,
    })
}

const MAX_PROVIDER_TIMEOUT: Duration = Duration::from_secs(60);
// Photos retries transient failures in `while_busy`. Plan replays only a
// refusal known not to have processed work, before any output has escaped.
const RETRY_ATTEMPTS: u32 = 4;
/// Bound retries of requests the provider provably did not take.
const PLAN_ATTEMPTS: u32 = 5;
const FIRST_RETRY_WAIT: Duration = Duration::from_millis(500);
const LONGEST_RETRY_WAIT: Duration = Duration::from_secs(4);
/// Shared hosted quotas can remain busy for tens of seconds. Plan retries
/// use a longer backoff, bounded by the caller's remaining answer budget.
const FIRST_PLAN_WAIT: Duration = Duration::from_secs(1);
const LONGEST_PLAN_WAIT: Duration = Duration::from_secs(8);
const ROOM_TO_ANSWER: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Again {
    Never,
    Soon(Option<Duration>),
}

struct ProviderFailure {
    model_error: agent_provider_config::protocol::ModelError,
    why: String,
    again: Again,
    /// False only when request construction proves no dispatch occurred, or
    /// the provider provably did not take the request (`unprocessed`).
    outcome_unknown: bool,
    /// The provider provably did no work for this request: no connection to
    /// it could be made, or it refused the request as rate-limited (HTTP
    /// 429, not for exhausted quota), overloaded (HTTP 529) or unavailable
    /// for now (HTTP 503, not when it says it is switched off). The one
    /// ground on which a Plan is sent again.
    unprocessed: bool,
    /// The provider's HTTP status, when one was received.
    status: Option<u16>,
}

/// One model turn's non-content parameters.
struct PlanCall<'a> {
    profile_id: &'a str,
    model_id: &'a str,
    timeout: Duration,
    stream: bool,
    trace: Option<PlanTrace>,
}

/// What a signed-in provider asks for when its sign-in has to be renewed
/// interactively.
const SIGN_IN_AGAIN: &str = "the sign-in for this provider has ended; sign in again in \
                             Settings, Agent Models";

/// The use `issuer`'s grant of a model provider is refreshed through.
fn models_use(issuer: SignInIssuer) -> OAuthUse {
    let issuer = match issuer {
        SignInIssuer::Google => OAuthIssuer::Google,
        SignInIssuer::ChatGpt => OAuthIssuer::OpenAi,
        SignInIssuer::GitHubCopilot => OAuthIssuer::GitHub,
        SignInIssuer::HuggingFace => OAuthIssuer::HuggingFace,
    };
    OAuthUse::new(issuer, Capability::Models)
}

/// The issuer `profile` signs in with, or why this build cannot use it.
fn sign_in_issuer(profile: &ProviderProfile) -> Result<SignInIssuer, String> {
    profile
        .sign_in_issuer()
        .ok_or_else(|| "provider signs in with an unsupported issuer".into())
}

/// How long before a minted access token's expiry it stops being reused, so
/// a kept token is never one ownership already counts as due for refresh
/// (ownership refreshes within a minute of expiry) and a request still
/// reaches the provider while it is valid.
const TOKEN_MARGIN: u64 = 90;

/// The vault id of `profile_id`'s sign-in grant, after the same check
/// [`vault_id`] makes.
fn grant_id(profile_id: &str) -> Result<AccountId, String> {
    if profile_id.is_empty()
        || profile_id.len() > 128
        || !profile_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("invalid provider profile id".into());
    }
    Ok(AccountId::new(agent_provider_config::sign_in_grant_id(
        profile_id,
    )))
}

/// A current access token for `profile`'s sign-in, refreshed by ownership
/// when due (the refresh token never leaves it), and the instant after
/// which it must not be reused. For a Copilot subscription the refresh is
/// the trade of its GitHub token for a Copilot token.
fn sign_in_token(
    profile: &ProviderProfile,
    now_unix: u64,
    now: Instant,
) -> Result<(Zeroizing<String>, Option<Instant>), String> {
    let issuer = sign_in_issuer(profile)?;
    let grant = grant_id(&profile.id)?;
    // A refresh reseals the grant: one read-modify-write of the vault, like
    // a key set through this gateway.
    let _serial = management_write()?;
    let (token, expires) = CredentialStore::live()
        .oauth_access_token_with_expiry(&grant, models_use(issuer), false)
        .map_err(|error| match error {
            CredentialError::NeedsAuthentication => SIGN_IN_AGAIN.to_string(),
            CredentialError::Offline => format!(
                "{} could not be reached to renew the sign-in",
                issuer.label()
            ),
            other => format!("{} sign-in failed: {other}", issuer.label()),
        })?;
    Ok((token, expires.map(|unix| reuse_until(unix, now_unix, now))))
}

/// The instant a token expiring at `expires_unix` stops being reused.
fn reuse_until(expires_unix: u64, now_unix: u64, now: Instant) -> Instant {
    let left = expires_unix
        .saturating_sub(now_unix)
        .saturating_sub(TOKEN_MARGIN);
    now.checked_add(Duration::from_secs(left)).unwrap_or(now)
}

/// A missing credential means unauthenticated only for a literal loopback
/// endpoint explicitly classified as this device; any other keyless
/// profile is listed nowhere here. The decision itself is
/// [`agent_provider_config::is_unauthenticated_local`], the same rule the
/// request headers read, so a plan the gateway lets through sends no
/// credential on either protocol.
fn unauthenticated_local_plan(profile: &ProviderProfile) -> Result<bool, ModelError> {
    if agent_provider_config::is_unauthenticated_local(profile) {
        return Ok(true);
    }
    if profile.location != EndpointLocation::ThisDevice
        || profile.credential.is_some()
        || profile.sign_in.is_some()
    {
        return Ok(false);
    }
    // Typed as this device with no key and no sign-in, but not at a literal
    // loopback address: report that, rather than quietly asking the vault
    // for a key the profile never named.
    url::Url::parse(&provider_url(profile)).map_err(|_| {
        resolution_error(
            ErrorKind::InvalidRequest,
            "local provider endpoint is invalid",
        )
    })?;
    Err(resolution_error(
        ErrorKind::InvalidRequest,
        "unauthenticated local Plan requires a literal loopback endpoint",
    ))
}

/// The largest provider stream read for one Plan.
const MAX_STREAM_BYTES: usize = 1024 * 1024;

fn stream_failure(why: &str) -> ProviderFailure {
    ProviderFailure {
        model_error: agent_provider_config::protocol::ModelError::new(
            agent_provider_config::protocol::ErrorKind::InvalidResponse,
        ),
        why: why.into(),
        again: Again::Never,
        outcome_unknown: true,
        unprocessed: false,
        status: None,
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
    fn flush(
        &mut self,
        calls: &[StreamCall],
        index: usize,
        now: Instant,
        force: bool,
        emit: Emit<'_>,
    ) -> Result<(), ProviderFailure> {
        let Some(call) = calls.get(index) else {
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
            configured: None,
            error: None,
            model_error: None,
            event: Some(EVENT_TOOL_CALL_DELTA.into()),
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
    calls: &[StreamCall],
    emitted: usize,
    complete: usize,
    emit: Emit<'_>,
    timing: &mut PlanTiming,
    mut coalescer: Option<&mut DeltaCoalescer>,
) -> Result<usize, ProviderFailure> {
    let mut emitted = emitted;
    while emitted < complete {
        let Some(call) = calls.get(emitted) else {
            break;
        };
        let now = timing.started.elapsed();
        timing.first_tool_call.get_or_insert(now);
        if call.name == "plan" {
            timing.plan_call.get_or_insert(now);
        }
        if let Some(coalescer) = coalescer.as_deref_mut() {
            // Every argument byte goes out as a delta before its call.
            coalescer.flush(calls, emitted, Instant::now(), true, emit)?;
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
                model_error: None,
                event: Some(EVENT_TOOL_CALL.into()),
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

/// Presentation state for a stream's tool and reasoning events.
#[derive(Default)]
struct StreamAssembly {
    calls: Vec<StreamCall>,
    reasoning_chunks: u64,
    reasoning_bytes: u64,
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
    /// Every token the provider counted for the call, as it says it.
    total_tokens: Option<u64>,
    reasoning_tokens: Option<u64>,
    /// Prompt tokens the provider served from its prompt cache.
    cached_tokens: Option<u64>,
    /// Prompt tokens written to the prompt cache (Anthropic reports these;
    /// its `input_tokens` is only the remainder neither read nor written).
    cache_write_tokens: Option<u64>,
    tool_calls: usize,
    output_allowance: Option<u64>,
    runtime_context: Option<u32>,
    source_context: Option<u32>,
    timeout_ms: Option<u128>,
    stream_chunks: u64,
    reasoning_chunks: u64,
    reasoning_bytes: u64,
    last_stream: Option<Duration>,
    last_reasoning: Option<Duration>,
    /// When the provider's headers and its first bytes came, noted by the
    /// transport that carries the call. A Plan that is not streamed hands
    /// its transport no timing, so its line names neither.
    signs: Arc<Signs>,
}

impl PlanTiming {
    /// One deadline and content-free policy snapshot for either request shape.
    fn budget(
        &mut self,
        call: &PlanCall<'_>,
        profile: &ProviderProfile,
        output: Option<u64>,
    ) -> Result<Duration, String> {
        let model = profile
            .models
            .iter()
            .find(|model| model.id == call.model_id);
        let limits = model.and_then(|model| model.request_limits.as_ref());
        let ceiling = limits
            .and_then(|limits| limits.plan_timeout_ms)
            .map_or(MAX_PROVIDER_TIMEOUT, |ms| {
                Duration::from_millis(u64::from(ms))
            });
        let timeout = call.timeout.min(ceiling);
        self.stream = call.stream;
        self.timeout_ms = Some(timeout.as_millis());
        if let Some(limits) = limits {
            self.output_allowance = Some(
                output
                    .unwrap_or_else(|| u64::from(limits.max_output_tokens))
                    .min(u64::from(limits.max_output_tokens)),
            );
            self.runtime_context = limits.runtime_context_tokens;
            self.source_context = model.and_then(|model| model.context_window);
        }
        timeout
            .checked_sub(self.started.elapsed())
            .filter(|budget| !budget.is_zero())
            .ok_or_else(|| "provider request timed out".to_string())
    }

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
            total_tokens: None,
            reasoning_tokens: None,
            cached_tokens: None,
            cache_write_tokens: None,
            tool_calls: 0,
            output_allowance: None,
            runtime_context: None,
            source_context: None,
            timeout_ms: None,
            stream_chunks: 0,
            reasoning_chunks: 0,
            reasoning_bytes: 0,
            last_stream: None,
            last_reasoning: None,
            signs: Arc::default(),
        }
    }

    /// One stream event arrived; the stream has carried `reasoning_chunks`
    /// reasoning chunks of `reasoning_bytes` bytes so far.
    fn observe_stream(&mut self, reasoning_chunks: u64, reasoning_bytes: u64) {
        let elapsed = self.started.elapsed();
        self.stream_chunks = self.stream_chunks.saturating_add(1);
        self.last_stream = Some(elapsed);
        if reasoning_chunks > self.reasoning_chunks {
            self.last_reasoning = Some(elapsed);
        }
        self.reasoning_chunks = reasoning_chunks;
        self.reasoning_bytes = reasoning_bytes;
    }

    /// Only counters/times: provider text is never retained in diagnostics.
    fn progress_log_fields(&self) -> String {
        let ms = |value: Option<Duration>| {
            value.map_or_else(|| "-".to_string(), |value| value.as_millis().to_string())
        };
        format!(
            "stream_chunks={} reasoning_chunks={} reasoning_bytes={} last_stream_ms={} last_reasoning_ms={}",
            self.stream_chunks,
            self.reasoning_chunks,
            self.reasoning_bytes,
            ms(self.last_stream),
            ms(self.last_reasoning),
        )
    }

    /// Token counts and tool-call count of a validated answer.
    fn observe_answer(&mut self, answer: &agent_provider_config::protocol::ModelResponse) {
        let usage = &answer.usage;
        let some = |count: u64| (count > 0).then_some(count);
        self.prompt_tokens = Some(usage.input_tokens);
        self.completion_tokens = Some(usage.output_tokens);
        self.total_tokens = some(usage.total_tokens);
        self.reasoning_tokens = some(usage.reasoning_tokens);
        self.cached_tokens = some(usage.cached_input_tokens);
        self.cache_write_tokens = some(usage.cache_write_tokens);
        self.tool_calls = answer
            .message
            .content
            .iter()
            .filter(|content| {
                matches!(
                    content,
                    agent_provider_config::protocol::Content::ToolCall(_)
                )
            })
            .count();
    }

    /// What the tokens this call counted cost by the gateway's own price
    /// table, said as the table's figure: it is no bill. A model the table
    /// lists no price for, a family's newer model among them, costs what
    /// nobody here knows.
    fn cost_fields(&self, model_id: &str) -> String {
        let counted = agent_provider_config::protocol::Usage {
            input_tokens: self.prompt_tokens.unwrap_or_default(),
            output_tokens: self.completion_tokens.unwrap_or_default(),
            total_tokens: 0,
            cached_input_tokens: self.cached_tokens.unwrap_or_default(),
            cache_write_tokens: self.cache_write_tokens.unwrap_or_default(),
            reasoning_tokens: self.reasoning_tokens.unwrap_or_default(),
        };
        crate::pricing::listed_price_of(model_id).map_or_else(
            || "cost_micros=- cost_source=unknown".to_owned(),
            |price| format!("cost_micros={} cost_source=table", price.of(&counted)),
        )
    }

    fn log(&self, call: &PlanCall<'_>, outcome: Result<(), &String>) {
        if std::env::var_os("TILO_MODEL_GATEWAY_TIMING").is_some_and(|value| value == "0") {
            return;
        }
        eprintln!("{}", self.line(call, outcome));
    }

    /// The call's timing line: counts, times and names, never content.
    fn line(&self, call: &PlanCall<'_>, outcome: Result<(), &String>) -> String {
        let ms = |value: Option<Duration>| {
            value.map_or_else(|| "-".to_string(), |value| value.as_millis().to_string())
        };
        let count = |value: Option<u64>| value.map_or_else(|| "-".to_string(), |v| v.to_string());
        let since = |at: Option<Instant>| at.map(|at| at.saturating_duration_since(self.started));
        let trace = call.trace.map_or_else(
            || "-".to_string(),
            |trace| format!("{:016x}/{}", trace.id, trace.turn),
        );
        format!(
            "tilo-model-gateway: timing plan trace={trace} profile={} model={} stream={} status={} \
             start_unix_ms={} headers_ms={} first_event_ms={} first_byte_ms={} first_reasoning_ms={} \
             first_token_ms={} first_tool_call_ms={} \
             plan_call_ms={} done_ms={} prompt_tokens={} completion_tokens={} total_tokens={} \
             reasoning_tokens={} cached_tokens={} cache_write_tokens={} {} tool_calls={} output_allowance={} \
             source_context={} runtime_context={} timeout_ms={} quiet_ms={} {} outcome={}{}",
            log_token(call.profile_id),
            log_token(call.model_id),
            self.stream,
            self.status
                .map_or_else(|| "-".to_string(), |status| status.to_string()),
            self.start_unix_ms,
            ms(since(self.signs.headers())),
            ms(since(self.signs.first_event())),
            ms(self.first_byte),
            ms(self.first_reasoning),
            ms(self.first_token),
            ms(self.first_tool_call),
            ms(self.plan_call),
            self.started.elapsed().as_millis(),
            count(self.prompt_tokens),
            count(self.completion_tokens),
            count(self.total_tokens),
            count(self.reasoning_tokens),
            count(self.cached_tokens),
            count(self.cache_write_tokens),
            self.cost_fields(call.model_id),
            self.tool_calls,
            count(self.output_allowance),
            count(self.source_context.map(u64::from)),
            count(self.runtime_context.map(u64::from)),
            self.timeout_ms
                .map_or_else(|| "-".into(), |value| value.to_string()),
            ms(self.signs.quiet()),
            self.progress_log_fields(),
            if outcome.is_ok() { "ok" } else { "error" },
            // Why an answer was not used, so a failed turn is never only
            // "error": the stream's own failure, as the gateway saw it.
            outcome
                .err()
                .map_or_else(String::new, |why| format!(" reason={why:?}")),
        )
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

/// A configuration identifier, reduced to a log-safe token. A model's name
/// goes whole as its provider lists it (`models/NAME`, `vendor/NAME`): a
/// slash is no secret.
fn log_token(value: &str) -> String {
    if !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'/')
        })
    {
        value.into()
    } else {
        "?".into()
    }
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
    let profile = configured_profile(profile_id).map_err(|error| error.message)?;
    if !is_supervised_loopback(&profile) {
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
    // No invented bearer token or hosted-provider credential lookup.
    // The fixed photo index asks speak chat completions, as a local server does.
    let url = provider_transform::url(profile.protocol, &profile.endpoint);
    unary_with_retries(http, (&profile, url), None, &body, VISION_TIMEOUT).await
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
    let profile = configured_profile(profile_id).map_err(|error| error.message)?;
    if !is_supervised_loopback(&profile) {
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
    // The fixed photo index asks speak chat completions, as a local server does.
    let url = provider_transform::url(profile.protocol, &profile.endpoint);
    unary_with_retries(http, (&profile, url), None, &body, VISION_TIMEOUT).await
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

async fn unary_with_retries(
    client: &reqwest::Client,
    (profile, url): (&ProviderProfile, String),
    secret: Option<&str>,
    body: &serde_json::Value,
    timeout: Duration,
) -> Result<String, String> {
    let began = Instant::now();
    let model = body
        .get("model")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    while_busy(
        || timeout.checked_sub(began.elapsed()),
        tokio::time::sleep,
        |budget| {
            provider_call::post(
                provider_call::Post {
                    client,
                    profile,
                    url: url.clone(),
                    secret,
                    model,
                    budget,
                    resend: false,
                    signs: None,
                },
                body,
            )
        },
    )
    .await
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

/// Use only recognized classifications, never echo arbitrary identifiers: a
/// hostile provider can put a credential in any field, not just `message`.
#[cfg(test)]
fn provider_error_advice(status: u16, body: &[u8]) -> &'static str {
    classified_error_advice(&crate::openai::Failure::from_http(
        status,
        &String::from_utf8_lossy(body),
        None,
    ))
}

fn classified_error_advice(failure: &crate::openai::Failure) -> &'static str {
    use agent_provider_config::protocol::ErrorKind;
    match failure.model_error().kind {
        ErrorKind::CreditsExhausted => {
            "; provider credits are exhausted; check billing or choose another provider"
        }
        ErrorKind::ContextLimit => {
            "; context limit exceeded; shorten the conversation or choose a larger-context model"
        }
        ErrorKind::Authentication => "; authentication failed; replace the provider API key",
        ErrorKind::PermissionDenied => "; provider permission denied; check the key's model access",
        ErrorKind::RateLimited => "; provider rate limit reached; wait before retrying",
        ErrorKind::TimedOut => "; provider request timed out; try a shorter request",
        ErrorKind::Unavailable | ErrorKind::Provider => {
            "; provider is unavailable; retry later or choose another provider"
        }
        _ => "; provider rejected or failed the request; check model and provider settings",
    }
}

#[cfg(test)]
fn again_after(status: u16, retry_after: Option<&str>) -> Again {
    let soon = || Again::Soon(retry_after.and_then(after_seconds));
    match status {
        408 | 429 | 500 | 502 | 503 | 504 | 529 => soon(),
        _ => Again::Never,
    }
}

/// Whether an unsuccessful answer says the provider did no work for the
/// request: rate-limited (429) or overloaded (529), classified as the
/// agent's own OpenAI-compatible client classifies them, so an exhausted
/// quota (an `OpenAI` 429 `insufficient_quota`), which asking again cannot
/// help, is not one.
#[cfg(test)]
fn refused_unprocessed(status: u16, body: &[u8]) -> bool {
    crate::openai::Failure::from_http(status, &String::from_utf8_lossy(body), None)
        .refused_unprocessed(status)
}

#[cfg(test)]
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

/// Plan-level backoff honors provider Retry-After without truncation.
fn plan_retry_wait(retry: u32, asked_for: Option<Duration>) -> Duration {
    if let Some(wait) = asked_for {
        return wait;
    }
    let doubling = 1u32
        .checked_shl(retry.saturating_sub(1))
        .unwrap_or(u32::MAX);
    FIRST_PLAN_WAIT
        .checked_mul(doubling)
        .unwrap_or(LONGEST_PLAN_WAIT)
        .min(LONGEST_PLAN_WAIT)
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

/// Send a Plan's POST with `attempt`, asking again only while the provider
/// provably did not take it ([`ProviderFailure::unprocessed`]), nothing has
/// yet reached the caller (`emitted`), the attempts last ([`PLAN_ATTEMPTS`]),
/// and what is `left` of the Plan's time still holds the wait the provider
/// asked for (`Retry-After`) and room for an answer.
async fn plan_send<T, L, W, WF, A, AF>(
    left: L,
    mut wait: W,
    emitted: &(dyn Fn() -> bool + Sync),
    mut attempt: A,
) -> Result<T, ProviderFailure>
where
    L: Fn() -> Option<Duration>,
    W: FnMut(Duration) -> WF,
    WF: Future<Output = ()>,
    A: FnMut(Duration) -> AF,
    AF: Future<Output = Result<T, ProviderFailure>>,
{
    let mut made = 0_u32;
    let mut last = None;
    loop {
        let Some(budget) = left().filter(|budget| !budget.is_zero()) else {
            // Nothing was sent since `last`, which already settled the outcome.
            return Err(last.unwrap_or_else(|| ProviderFailure {
                model_error: agent_provider_config::protocol::ModelError::new(
                    agent_provider_config::protocol::ErrorKind::TimedOut,
                ),
                why: "provider request timed out".into(),
                again: Again::Never,
                outcome_unknown: false,
                unprocessed: false,
                status: None,
            }));
        };
        made = made.saturating_add(1);
        let mut failure = match attempt(budget).await {
            Ok(answer) => return Ok(answer),
            Err(failure) => failure,
        };
        let waiting = plan_retry_wait(
            made,
            match failure.again {
                Again::Soon(asked_for) => asked_for,
                Again::Never => None,
            },
        );
        let again = failure.unprocessed
            && made < PLAN_ATTEMPTS
            && !emitted()
            && left().is_some_and(|remaining| remaining >= waiting.saturating_add(ROOM_TO_ANSWER));
        if made > 1 {
            failure.why = format!("{} ({made} attempts)", failure.why);
        }
        if !again {
            return Err(failure);
        }
        // Content-free: the status and the wait, never the provider's body.
        eprintln!(
            "tilo-model-gateway: plan provider did not take attempt {made} (status={}); \
             asking again in {} ms",
            failure
                .status
                .map_or_else(|| "connect".to_string(), |status| status.to_string()),
            waiting.as_millis()
        );
        last = Some(failure);
        wait(waiting).await;
    }
}

fn provider_url(profile: &ProviderProfile) -> String {
    agent_provider_config::wire::url(profile)
}

/// Whether the peer's `SO_PEERCRED` uid is this process's effective uid.
#[cfg(target_os = "linux")]
fn same_user(stream: &tokio::net::UnixStream) -> bool {
    let euid = rustix::process::geteuid().as_raw();
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

    use super::*;

    /// The report composer may ask a model which words are personal, and the
    /// session's history may ask what a change was: a completion without
    /// tools, and nothing more. An app the gateway does not know may ask
    /// nothing.
    #[tokio::test]
    async fn the_report_composer_and_the_history_complete_text_and_are_refused_tools() {
        use agent_provider_config::protocol::{
            Message, ModelRequest, Role, ToolChoice, ToolDefinition,
        };
        let http = provider_client().unwrap();
        let ask = |tools: Vec<ToolDefinition>| Request::CompleteDefault {
            request: ModelRequest {
                messages: vec![Message::text(Role::User, "[0] Thanks Ada")],
                tool_choice: if tools.is_empty() {
                    ToolChoice::None
                } else {
                    ToolChoice::Auto
                },
                tools,
                max_output_tokens: 64,
            },
            timeout_ms: 1_000,
            stream: false,
        };
        let tool = ToolDefinition {
            name: "send_mail".into(),
            description: "Send mail".into(),
            input_schema: serde_json::json!({"type": "object"}),
        };
        let refused = handle(FEEDBACK_APP, ask(vec![tool]), &http).await;
        assert_eq!(
            refused.model_error.map(|error| error.kind),
            Some(ErrorKind::PermissionDenied)
        );
        let other = handle("so.tilo.files", ask(Vec::new()), &http).await;
        assert_eq!(
            other.model_error.map(|error| error.kind),
            Some(ErrorKind::PermissionDenied)
        );
        let titled = handle(
            ACTIVITY_APP,
            ask(vec![ToolDefinition {
                name: "send_mail".into(),
                description: "Send mail".into(),
                input_schema: serde_json::json!({"type": "object"}),
            }]),
            &http,
        )
        .await;
        assert_eq!(
            titled.model_error.map(|error| error.kind),
            Some(ErrorKind::PermissionDenied),
            "the history names runs, never calls tools"
        );
    }

    #[tokio::test]
    async fn subject_requests_observe_the_build_capability_and_caller_boundary() {
        let http = provider_client().unwrap();
        for caller in [CANVAS_APP, PHOTOS_APP, AGENT_APP] {
            let response = handle(
                caller,
                Request::SegmentSubject {
                    image: VisionImage {
                        media_type: "image/png".into(),
                        data_base64: base64::engine::general_purpose::STANDARD
                            .encode(b"not a picture"),
                        source_content_hash: "0".repeat(64),
                    },
                },
                &http,
            )
            .await;
            assert!(!response.ok);
            let expected = if caller == AGENT_APP {
                "only Canvas and Photos may find a picture's subject"
            } else if cfg!(feature = "local-models") {
                "the picture cannot be decoded"
            } else {
                "on-device subject segmentation is unavailable in this gateway build"
            };
            assert_eq!(response.error.as_deref(), Some(expected));
        }
    }

    fn vision_image() -> VisionImage {
        VisionImage {
            media_type: "image/jpeg".into(),
            data_base64: base64::engine::general_purpose::STANDARD.encode(b"preview"),
            source_content_hash: "a".repeat(64),
        }
    }

    tilo_test_env::scenario! {
        /// The launcher handoff bytes are pinned; local vision input is closed and bounded, never
        /// routes to a LAN or hosted endpoint, requires an immutable model revision, and uses a
        /// fixed prompt with no identity request; local face input is bounded and never requests
        /// identity.
        fn local_vision_is_pinned_bounded_local_and_never_asks_identity;

        phase launcher_handoff_bytes_are_pinned;
        // The launcher's handoff: a little-endian `u32` length, then JSON.
        {
            use std::io::Write as _;
            let (mut launcher, gateway) = std::os::unix::net::UnixStream::pair().unwrap();
            launcher.write_all(&[39, 0, 0, 0]).unwrap();
            launcher
                .write_all(br#"{"protocol":1,"app_id":"so.tilo.agent"}"#)
                .unwrap();
            gateway.set_nonblocking(true).unwrap();
            let runtime = tilo_rt::daemon("gateway-test").workers(1).build().unwrap();
            let handoff = runtime.block_on(async {
                let mut gateway = tokio::net::UnixStream::from_std(gateway).unwrap();
                read_handoff(&mut gateway).await
            });
            assert_eq!(handoff.unwrap().app_id, AGENT_APP);
        }

        phase local_vision_input_is_closed_and_bounded;
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

        phase local_vision_prompt_is_fixed_and_contains_no_identity_request;
        {
            let body = local_vision_body("vision-model", &vision_image());
            let encoded = body.to_string();
            assert!(encoded.contains("device-local image indexing component"));
            assert!(encoded.contains("Do not name people"));
            assert!(!encoded.contains("ignore previous"));
            assert_eq!(body["model"], "vision-model");
        }

        phase local_face_input_is_bounded_and_prompt_never_requests_identity;
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
                        model_error: crate::openai::Failure::from_http(status, "", retry_after)
                            .model_error(),
                        why: format!("provider returned HTTP {status}"),
                        again: again_after(status, retry_after),
                        outcome_unknown: true,
                        unprocessed: false,
                        status: Some(status),
                    }),
                })
            },
        ))
        .unwrap();
        (result, fake.into_inner())
    }

    tilo_test_env::scenario! {
        /// Transient provider failures are retried at the gateway, settled ones are not,
        /// Retry-After and transient statuses are classified on the HTTP side, and retries never
        /// consume room needed for an answer.
        fn the_gateway_retries_only_transient_failures_within_the_answer_budget;

        phase transient_provider_failures_are_retried_at_the_gateway;
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

        phase settled_provider_failures_are_not_retried;
        {
            for status in [400, 401, 403, 404, 422] {
                let (result, fake) = run_retries(Duration::from_secs(20), &[Err((status, None))]);
                assert!(result.unwrap_err().contains(&status.to_string()));
                assert_eq!(fake.calls, 1, "HTTP {status}");
                assert!(fake.waits.is_empty(), "HTTP {status}");
            }
        }

        phase retry_after_and_transient_statuses_are_classified_on_the_http_side;
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

        phase retries_never_consume_room_needed_for_an_answer;
        {
            let (result, fake) = run_retries(Duration::from_secs(3), &[Err((529, Some("1")))]);
            assert!(result.is_err());
            assert_eq!(fake.calls, 1);
            assert!(fake.waits.is_empty());
        }
    }

    /// The real accept loop on its own runtime, serving `listener` (peer
    /// check stubbed: other hosts refuse every peer, and the check itself is
    /// unchanged). `workers` of one proves concurrency comes from serving
    /// clients as tasks, not from a thread each.
    fn serve_in_background(listener: UnixListener, workers: usize) {
        serve_in_background_within(listener, workers, HANDOFF_TIMEOUT);
    }

    /// [`serve_in_background`], closing connections whose handoff has not
    /// arrived within `handoff`.
    fn serve_in_background_within(listener: UnixListener, workers: usize, handoff: Duration) {
        listener.set_nonblocking(true).unwrap();
        let http = Arc::new(provider_client().unwrap());
        std::thread::spawn(move || {
            let runtime = tilo_rt::daemon("gateway-test")
                .workers(workers)
                .build()
                .unwrap();
            runtime.block_on(async move {
                let listener = tokio::net::UnixListener::from_std(listener).unwrap();
                serve_on(listener, http, MAX_CLIENTS, handoff, |_| true).await
            })
        });
    }

    fn serve_test_socket(handoff: Duration) -> PathBuf {
        let nonce = tilo_clock::unix_now_ns();
        let path = std::env::temp_dir().join(format!("gw-{}-{nonce}.sock", std::process::id()));
        serve_in_background_within(UnixListener::bind(&path).unwrap(), 2, handoff);
        path
    }

    fn settings_client(path: &Path) -> UnixStream {
        let mut stream = UnixStream::connect(path).unwrap();
        stream
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        write_frame(
            &mut stream,
            &serde_json::json!({"protocol": 1, "app_id": AGENT_APP}),
        )
        .unwrap();
        stream
    }

    fn unauthorized_round_trip(stream: &mut UnixStream) {
        // Answered without configuration or network: Agent may not list a
        // provider's models.
        write_frame(
            stream,
            &serde_json::json!({"version": RPC_VERSION, "verb": "list_models", "profile_id": "p"}),
        )
        .unwrap();
        let response: Response = read_frame(stream).unwrap();
        assert!(!response.ok);
        assert_eq!(
            response.error.as_deref(),
            Some("caller is not authorized for this gateway operation")
        );
    }

    tilo_test_env::scenario! {
        /// The accept loop beside a client that says nothing, each case on a socket and server of its own.
        fn a_silent_or_stalled_client_never_holds_up_another;

        phase one_idle_or_stalled_client_never_blocks_another;
        {
            // A handoff no test outlives: the silent connection stays pending
            // for the whole test, so the others are answered beside it.
            let path = serve_test_socket(tilo_test_env::hang_bound().saturating_mul(2));
            // A connection that never sends its handoff, and one that sent it and
            // then went idle (the agent's normal, process-long channel).
            let silent = UnixStream::connect(&path).unwrap();
            let mut idle = settings_client(&path);
            let mut active = settings_client(&path);
            for _ in 0..3 {
                unauthorized_round_trip(&mut active);
            }
            // ...while the idle authenticated channel stays usable.
            unauthorized_round_trip(&mut idle);
            silent.set_nonblocking(true).unwrap();
            assert_eq!(
                (&silent).read(&mut [0_u8; 1]).unwrap_err().kind(),
                std::io::ErrorKind::WouldBlock,
                "the silent connection was still waiting for its handoff"
            );
            let _ = fs::remove_file(&path);
        }

        phase a_stalled_client_does_not_hold_up_another;
        {
            use std::io::Write as _;

            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("gateway.sock");
            serve_in_background(UnixListener::bind(&path).unwrap(), 1);

            // Connected first, then silent partway through its authentication.
            let mut stalled = UnixStream::connect(&path).unwrap();
            stalled.write_all(&[0x20]).unwrap();

            let mut client = UnixStream::connect(&path).unwrap();
            client
                .set_read_timeout(Some(tilo_test_env::hang_bound()))
                .unwrap();
            let handoff =
                serde_json::to_vec(&serde_json::json!({ "protocol": 1, "app_id": AGENT_APP })).unwrap();
            client
                .write_all(&u32::try_from(handoff.len()).unwrap().to_le_bytes())
                .unwrap();
            client.write_all(&handoff).unwrap();
            write_frame(
                &mut client,
                &Frame {
                    version: RPC_VERSION,
                    value: Request::ListModels {
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

    #[test]
    fn a_connection_without_its_handoff_is_closed() {
        let path = serve_test_socket(HANDOFF_TIMEOUT);
        let began = Instant::now();
        let mut silent = UnixStream::connect(&path).unwrap();
        silent
            .set_read_timeout(Some(tilo_test_env::hang_bound()))
            .unwrap();
        let mut byte = [0_u8; 1];
        assert_eq!(silent.read(&mut byte).unwrap(), 0, "closed, not answered");
        assert!(
            began.elapsed() >= HANDOFF_TIMEOUT,
            "closed before its handoff time"
        );
        let _ = fs::remove_file(&path);
    }
}

#[cfg(test)]
#[path = "service_error_tests.rs"]
mod service_error_tests;

#[cfg(test)]
#[path = "road_tests.rs"]
mod road_tests;

#[cfg(test)]
#[path = "stream_policy_tests.rs"]
mod stream_policy_tests;

#[cfg(test)]
#[path = "cancellation_tests.rs"]
mod cancellation_tests;

#[cfg(test)]
#[path = "server_ask_tests.rs"]
mod server_ask_tests;

#[cfg(test)]
#[path = "administration_tests.rs"]
mod administration_tests;
