//! The host's browser listener (feature `web`): PR 541's carriers, served by
//! the relay itself.
//!
//! - **HTTPS** (TCP, HTTP/2 and HTTP/1.1): device certification, the session
//!   handshake, and the durable plane — `/send` and the long-polled
//!   `/events` of [`sessions`]. Routes are listed in
//!   [`tilo_relay_protocol::web`].
//! - **WebTransport** (UDP, HTTP/3, same port number): the live plane as
//!   `moq-lite` at [`LIVE_PATH`](tilo_relay_protocol::web::LIVE_PATH), the
//!   [`tilo_moq::live::LiveRelay`] ported from PR 541, admitted by
//!   [`live::Authority`]. Browsers without WebTransport use its HTTPS
//!   fallback routes.
//!
//! Browser sessions are ordinary relay peers (see [`sessions`]), and live
//! frames from iroh peers are bridged into the live relay, so browser and
//! native peers share one host order and one live room.
//!
//! No owner loop runs here: every handler is async and awaits the relay's
//! own locks; workspace I/O already runs on the blocking pool. The only
//! periodic work is the 50 ms live permission observation and the session
//! janitor, both on tokio intervals.

mod live;
mod sessions;
mod tls;

pub use tls::TlsSource;

use std::convert::Infallible;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::BodyExt as _;
use hyper::{Method, Request, Response, StatusCode, header};
use tilo_moq::live::LiveRelay;
use tilo_moq::live::fallback::{
    FALLBACK_OPEN_PATH, FALLBACK_RESUME_PATH, FALLBACK_SEND_PATH, FALLBACK_SESSION_HEADER,
    FallbackError, FallbackStream, MAX_SEND_BYTES, session_id,
};
use tilo_relay_protocol::certify::CertifyReply;
use tilo_relay_protocol::live::LiveFrame;
pub(crate) use tilo_relay_protocol::web as protocol;
use tilo_relay_protocol::web::{
    API_PREFIX, EventsRequest, HostInfo, MAX_POLL_MS, MAX_REQUEST_BYTES, SESSION_HEADER,
    SessionFinish, live_room,
};
use tilo_user_identity::CertificationRequest;

use crate::daemon::{Relay, RelayError};
use sessions::{SessionError, Sessions};

/// Configuration of the browser listener.
#[derive(Clone, Debug)]
pub struct WebConfig {
    /// TCP address for HTTPS; WebTransport binds the same address over UDP.
    /// Port 0 picks one free for both.
    pub listen: SocketAddr,
    /// Certificate source.
    pub tls: TlsSource,
    /// Page origins allowed to call the host cross-origin (the Cloud console
    /// origin, a dev server). Empty allows same-origin pages only.
    pub allowed_origins: Vec<String>,
}

/// A running browser listener. Dropping it stops serving.
pub struct WebHandle {
    /// HTTPS (and WebTransport) address.
    pub addr: SocketAddr,
    /// `https://host:port` of the listener.
    pub origin: String,
    /// SHA-256 of a development certificate, for `serverCertificateHashes`.
    pub certificate_sha256: Option<String>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
}

impl Drop for WebHandle {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

struct Web {
    sessions: Arc<Sessions>,
    grants: Arc<live::LiveGrants>,
    live: LiveRelay,
    host_info: HostInfo,
    allowed_origins: Vec<String>,
}

impl Relay {
    /// Serve browsers: HTTPS and WebTransport on `config.listen`.
    ///
    /// # Errors
    ///
    /// Returns [`RelayError::Iroh`] (the transport error kind) when the
    /// certificate cannot be loaded or an address cannot be bound.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the guard only lives for the lookup; the value kept is a clone"
    )]
    pub async fn serve_web(&self, config: WebConfig) -> Result<WebHandle, RelayError> {
        let failed = |message: String| RelayError::Iroh(format!("web listener: {message}"));
        let identity = tls::Identity::load(&config.tls).map_err(failed)?;
        let tcp = tokio::net::TcpListener::bind(config.listen)
            .await
            .map_err(|error| failed(error.to_string()))?;
        let addr = tcp
            .local_addr()
            .map_err(|error| failed(error.to_string()))?;
        let endpoint =
            tilo_moq::carrier::listener(addr, identity.chain.clone(), identity.key.clone_key())
                .map_err(failed)?;
        let acceptor =
            tokio_rustls::TlsAcceptor::from(Arc::new(identity.https_config().map_err(failed)?));
        let context = self.web_context().clone();
        let sessions = Sessions::new(context.clone());
        let grants = live::LiveGrants::new();
        let live = LiveRelay::new(Arc::new(live::Authority(grants.clone())));
        let bridge_relay = live.clone();
        let _ = context
            .relay
            .live_bridge
            .set(Arc::new(move |workspace, from, frame| {
                let room = live_room(workspace);
                let publisher = hex::encode(from);
                match LiveFrame::decode(frame) {
                    Ok(LiveFrame::Presence(_)) => {
                        bridge_relay.inject_presence(&room, &publisher, frame);
                    }
                    Ok(LiveFrame::Ops(ops)) => {
                        let record = hex::encode(ops.record().digest());
                        bridge_relay.inject_ops(&room, &publisher, &record, frame);
                    }
                    Err(_) => {}
                }
            }));
        let tasks_bridge = bridge_browser_presence(&live, &grants, &context);
        let web = Arc::new(Web {
            sessions: sessions.clone(),
            grants: grants.clone(),
            live,
            host_info: HostInfo {
                host_cert: context.relay.own_cert,
                webtransport_port: addr.port(),
                certificate_sha256: identity.pinned_sha256.clone(),
            },
            allowed_origins: config.allowed_origins,
        });
        let mut tasks = vec![tasks_bridge];
        tasks.push(tokio::spawn(web.live.clone().serve(endpoint)));
        tasks.push(tokio::spawn({
            async move {
                let mut observe = tokio::time::interval(live::OBSERVATION);
                observe.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut sweep = tokio::time::interval(Duration::from_secs(5));
                loop {
                    tokio::select! {
                        _ = observe.tick() => grants.observe(&sessions).await,
                        _ = sweep.tick() => sessions.sweep(),
                    }
                }
            }
        }));
        tasks.push(tokio::spawn(accept_loop(tcp, acceptor, web)));
        Ok(WebHandle {
            addr,
            origin: format!("https://{addr}"),
            certificate_sha256: identity.pinned_sha256,
            tasks,
        })
    }
}

/// Browser presence frames the live relay forwarded, waiting to reach iroh
/// live members. Lossy like the rest of the live plane.
const BROWSER_PRESENCE_BACKLOG: usize = 1024;

/// Carry presence that browsers publish over WebTransport (or its HTTPS
/// fallback) to the workspace's iroh live members as ordinary
/// [`Envelope::Live`](crate::Envelope::Live) frames — the other half of the
/// iroh-to-browser bridge installed as `live_bridge`. Frames the host
/// injected are never tapped, so nothing loops.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the guard only lives for the lookup; the value kept is a clone"
)]
fn bridge_browser_presence(
    live: &LiveRelay,
    grants: &Arc<live::LiveGrants>,
    context: &crate::daemon::WebContext,
) -> tokio::task::JoinHandle<()> {
    let (tx, mut rx) =
        tokio::sync::mpsc::channel::<(String, [u8; 32], Vec<u8>)>(BROWSER_PRESENCE_BACKLOG);
    live.set_presence_tap(Arc::new(
        move |room: &str, publisher: &str, frame: &[u8]| {
            let mut device = [0u8; 32];
            if hex::decode_to_slice(publisher, &mut device).is_ok() {
                let _ = tx.try_send((room.to_owned(), device, frame.to_vec()));
            }
        },
    ));
    let grants = grants.clone();
    let context = context.clone();
    tokio::spawn(async move {
        while let Some((room, device, frame)) = rx.recv().await {
            let Some(workspace_id) = grants.workspace_of(&room) else {
                continue;
            };
            let workspace = context
                .relay
                .workspaces
                .lock()
                .await
                .get(&workspace_id)
                .cloned();
            let Some(workspace) = workspace else {
                continue;
            };
            let envelope = crate::Envelope::Live {
                workspace_id,
                from_device_public_key: device,
                frame,
            };
            workspace
                .lock()
                .await
                .relay_bridged_live(&device, &envelope);
        }
    })
}

async fn accept_loop(
    tcp: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    web: Arc<Web>,
) {
    loop {
        let Ok((stream, _)) = tcp.accept().await else {
            continue;
        };
        let acceptor = acceptor.clone();
        let web = web.clone();
        tokio::spawn(async move {
            let Ok(Ok(tls)) =
                tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream)).await
            else {
                return;
            };
            let service = hyper::service::service_fn(move |request| {
                let web = web.clone();
                async move { Ok::<_, Infallible>(handle(&web, request).await) }
            });
            let _ =
                hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
                    .serve_connection(hyper_util::rt::TokioIo::new(tls), service)
                    .await;
        });
    }
}

/// A response body: whole bytes or a live fallback stream.
pub(crate) enum Body {
    Full(Option<Bytes>),
    Stream(FallbackStream),
}

impl http_body::Body for Body {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Infallible>>> {
        match self.get_mut() {
            Self::Full(bytes) => {
                Poll::Ready(bytes.take().map(|bytes| Ok(http_body::Frame::data(bytes))))
            }
            Self::Stream(stream) => stream
                .poll_next(cx)
                .map(|chunk| chunk.map(|bytes| Ok(http_body::Frame::data(bytes)))),
        }
    }
}

fn reply(status: StatusCode, body: Vec<u8>) -> Response<Body> {
    let mut response = Response::new(Body::Full(Some(Bytes::from(body))));
    *response.status_mut() = status;
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        header::HeaderValue::from_static("application/octet-stream"),
    );
    response
}

fn encoded<T: serde::Serialize>(value: &T) -> Response<Body> {
    match postcard::to_allocvec(value) {
        Ok(bytes) => reply(StatusCode::OK, bytes),
        Err(_) => reply(StatusCode::INTERNAL_SERVER_ERROR, Vec::new()),
    }
}

fn status(status: StatusCode, message: &str) -> Response<Body> {
    reply(status, message.as_bytes().to_vec())
}

fn session_status(error: &SessionError) -> Response<Body> {
    match error {
        SessionError::Gone => status(StatusCode::UNAUTHORIZED, "session ended"),
        SessionError::Refused(reason) => status(StatusCode::FORBIDDEN, reason),
        SessionError::Busy => status(StatusCode::SERVICE_UNAVAILABLE, "busy"),
    }
}

fn fallback_status(error: FallbackError) -> Response<Body> {
    match error {
        FallbackError::Denied => status(StatusCode::FORBIDDEN, "denied"),
        FallbackError::Gone => status(StatusCode::NOT_FOUND, "gone"),
        FallbackError::Busy => status(StatusCode::SERVICE_UNAVAILABLE, "busy"),
        FallbackError::Invalid => status(StatusCode::BAD_REQUEST, "invalid"),
    }
}

#[allow(
    clippy::result_large_err,
    reason = "the error is the response the handler sends as it is"
)]
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Response<Body>> {
    postcard::from_bytes(bytes).map_err(|_| status(StatusCode::BAD_REQUEST, "malformed body"))
}

fn token(request: &Request<hyper::body::Incoming>) -> Option<[u8; 32]> {
    let value = request.headers().get(SESSION_HEADER)?.to_str().ok()?;
    hex::decode(value).ok()?.try_into().ok()
}

fn cors(web: &Web, origin: Option<&header::HeaderValue>, response: &mut Response<Body>) {
    let Some(origin) = origin.and_then(|value| value.to_str().ok()) else {
        return;
    };
    if !web
        .allowed_origins
        .iter()
        .any(|allowed| allowed == origin || allowed == "*")
    {
        return;
    }
    let headers = response.headers_mut();
    if let Ok(origin) = header::HeaderValue::from_str(origin) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    }
    headers.insert(header::VARY, header::HeaderValue::from_static("origin"));
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        header::HeaderValue::from_static("POST, OPTIONS"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        header::HeaderValue::from_static("content-type, x-tilo-session, x-tilo-live-session"),
    );
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        header::HeaderValue::from_static("x-tilo-live-session"),
    );
}

async fn handle(web: &Web, request: Request<hyper::body::Incoming>) -> Response<Body> {
    let origin = request.headers().get(header::ORIGIN).cloned();
    let mut response = if request.method() == Method::OPTIONS {
        reply(StatusCode::NO_CONTENT, Vec::new())
    } else if request.method() != Method::POST {
        status(StatusCode::METHOD_NOT_ALLOWED, "POST only")
    } else {
        route(web, request)
            .await
            .unwrap_or_else(|response| response)
    };
    cors(web, origin.as_ref(), &mut response);
    response
}

async fn body(
    request: Request<hyper::body::Incoming>,
    limit: usize,
) -> Result<(hyper::http::request::Parts, Bytes), Response<Body>> {
    let (parts, body) = request.into_parts();
    let bytes = http_body_util::Limited::new(body, limit)
        .collect()
        .await
        .map_err(|_| status(StatusCode::PAYLOAD_TOO_LARGE, "body too large"))?
        .to_bytes();
    Ok((parts, bytes))
}

fn wait(ms: u32) -> Duration {
    Duration::from_millis(u64::from(ms.min(MAX_POLL_MS)))
}

#[allow(
    clippy::too_many_lines,
    reason = "one routing table: every browser route and its body type in one place"
)]
async fn route(
    web: &Web,
    request: Request<hyper::body::Incoming>,
) -> Result<Response<Body>, Response<Body>> {
    let path = request.uri().path().to_owned();
    let Some(route) = path.strip_prefix(API_PREFIX) else {
        return Err(status(StatusCode::NOT_FOUND, "not found"));
    };
    let session = token(&request);
    let live_session = request
        .headers()
        .get(FALLBACK_SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(session_id);
    let limit = if route == "/live/fallback/send" {
        MAX_SEND_BYTES
    } else {
        MAX_REQUEST_BYTES
    };
    let (_, bytes) = body(request, limit).await?;
    let sessions = &web.sessions;
    let certification = &sessions.context().certification;
    #[allow(
        clippy::result_large_err,
        reason = "the error is the response the handler sends as it is"
    )]
    let authed = || {
        session
            .ok_or_else(|| status(StatusCode::UNAUTHORIZED, "no session"))
            .and_then(|token| sessions.get(&token).map_err(|error| session_status(&error)))
    };
    match route {
        "/host" => Ok(encoded(&web.host_info)),
        "/certify" => {
            let request: CertificationRequest = decode(&bytes)?;
            Ok(encoded(
                &match certification.enqueue(request, tilo_clock::unix_now_ns()) {
                    Ok((_, pairing_code, _)) => CertifyReply::Pending { pairing_code },
                    Err(reason) => CertifyReply::Denied { reason },
                },
            ))
        }
        "/certify/wait" => {
            let digest: [u8; 32] = decode(&bytes)?;
            Ok(encoded(
                &certification.wait(&digest, wait(MAX_POLL_MS)).await,
            ))
        }
        "/session" => {
            let hello = decode(&bytes)?;
            sessions
                .hello(&hello)
                .map(|challenge| encoded(&challenge))
                .map_err(|error| session_status(&error))
        }
        "/session/finish" => {
            let finish: SessionFinish = decode(&bytes)?;
            sessions
                .finish(&finish.handshake, &finish.ack2)
                .map(|grant| encoded(&grant))
                .map_err(|error| session_status(&error))
        }
        "/send" => {
            let session = authed()?;
            let envelopes = decode(&bytes)?;
            sessions
                .send(&session, envelopes)
                .await
                .map(|()| reply(StatusCode::NO_CONTENT, Vec::new()))
                .map_err(|error| session_status(&error))
        }
        "/events" => {
            let session = authed()?;
            let request: EventsRequest = decode(&bytes)?;
            sessions
                .events(&session, request.ack, wait(request.wait_ms))
                .await
                .map(|page| encoded(&page))
                .map_err(|error| session_status(&error))
        }
        "/live/grant" => {
            let session = authed()?;
            let workspace_id: u64 = decode(&bytes)?;
            let token = session.token();
            web.grants
                .issue(
                    sessions,
                    token,
                    session.peer.device_public_key,
                    session.peer.user_public_key,
                    workspace_id,
                )
                .await
                .map(|grant| encoded(&grant))
                .ok_or_else(|| status(StatusCode::FORBIDDEN, "not admitted to this workspace"))
        }
        _ if path == FALLBACK_OPEN_PATH => {
            let id: [u8; 16] = rand::random();
            let stream = web
                .live
                .fallback_open(&bytes, id)
                .await
                .map_err(fallback_status)?;
            let mut response = Response::new(Body::Stream(stream));
            if let Ok(value) = header::HeaderValue::from_str(&hex::encode(id)) {
                response
                    .headers_mut()
                    .insert(FALLBACK_SESSION_HEADER, value);
            }
            Ok(response)
        }
        _ if path == FALLBACK_RESUME_PATH => {
            let id =
                live_session.ok_or_else(|| status(StatusCode::BAD_REQUEST, "no live session"))?;
            let stream = web.live.fallback_resume(&id).map_err(fallback_status)?;
            Ok(Response::new(Body::Stream(stream)))
        }
        _ if path == FALLBACK_SEND_PATH => {
            let id =
                live_session.ok_or_else(|| status(StatusCode::BAD_REQUEST, "no live session"))?;
            web.live
                .fallback_send(&id, &bytes)
                .map_err(fallback_status)?;
            Ok(reply(StatusCode::NO_CONTENT, Vec::new()))
        }
        _ => Err(status(StatusCode::NOT_FOUND, "not found")),
    }
}
