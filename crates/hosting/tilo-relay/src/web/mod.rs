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

mod legacy;
mod live;
mod sessions;
#[cfg(test)]
mod tests;
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
use tilo_relay_protocol::legacy_window::InputRequest;
use tilo_relay_protocol::live::LiveFrame;
pub(crate) use tilo_relay_protocol::web as protocol;
use tilo_relay_protocol::web::{
    API_PREFIX, AgentLiveGrantRequest, EventsRequest, HostInfo, MAX_AGENT_ID_BYTES, MAX_POLL_MS,
    MAX_REQUEST_BYTES, SESSION_HEADER, SessionFinish, live_room,
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
    /// A loopback address that also serves the HTTPS routes as plain HTTP,
    /// for local review in a browser that should not need a certificate
    /// exception. The live plane then uses the HTTPS stream fallback over
    /// it; WebTransport stays on `listen`.
    pub plain_listen: Option<SocketAddr>,
}

/// A running browser listener. Dropping it stops serving.
pub struct WebHandle {
    /// HTTPS (and WebTransport) address.
    pub addr: SocketAddr,
    /// `https://host:port` of the listener.
    pub origin: String,
    /// SHA-256 of a development certificate, for `serverCertificateHashes`.
    pub certificate_sha256: Option<String>,
    /// `http://localhost:port` of the plain loopback listener, if any.
    pub plain_origin: Option<String>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    legacy: Arc<legacy::LegacyWindows>,
}

impl Drop for WebHandle {
    fn drop(&mut self) {
        self.legacy.close();
        for task in &self.tasks {
            task.abort();
        }
    }
}

struct Web {
    sessions: Arc<Sessions>,
    grants: Arc<live::LiveGrants>,
    legacy: Arc<legacy::LegacyWindows>,
    live: LiveRelay,
    host_info: HostInfo,
    allowed_origins: Vec<String>,
    /// What `/session/finish` advertises.
    features: u64,
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
        if config
            .plain_listen
            .is_some_and(|plain| !plain.ip().is_loopback())
        {
            return Err(failed(
                "the plain HTTP listener must be a loopback address".into(),
            ));
        }
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
        let legacy =
            legacy::LegacyWindows::new(live.clone(), context.relay.own_cert.device_public_key);
        let _ = context.relay.live_bridge.set(Arc::new(Bridge {
            live: live.clone(),
            legacy: legacy.clone(),
        }));
        let tasks_bridge = bridge_browser_presence(&live, &grants, &context);
        let tasks_legacy = tokio::spawn(maintain_legacy(legacy.clone(), context.clone()));
        let web = Arc::new(Web {
            sessions: sessions.clone(),
            grants: grants.clone(),
            legacy: legacy.clone(),
            live,
            host_info: HostInfo {
                host_cert: context.relay.own_cert,
                webtransport_port: addr.port(),
                certificate_sha256: identity.pinned_sha256.clone(),
            },
            allowed_origins: config.allowed_origins,
            features: self.web_features(),
        });
        let mut tasks = vec![tasks_bridge, tasks_legacy];
        let live = web.live.clone();
        tasks.push(
            match self.voice_carrier(&config.tls, addr, identity.pinned_sha256.clone()) {
                Some(voice) => tokio::spawn(live.serve_with_voice(endpoint, voice)),
                None => tokio::spawn(live.serve(endpoint)),
            },
        );
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
        let mut plain_origin = None;
        if let Some(plain) = config.plain_listen {
            let plain = tokio::net::TcpListener::bind(plain)
                .await
                .map_err(|error| failed(error.to_string()))?;
            let port = plain
                .local_addr()
                .map_err(|error| failed(error.to_string()))?
                .port();
            plain_origin = Some(format!("http://localhost:{port}"));
            tasks.push(tokio::spawn(plain_accept_loop(plain, web.clone())));
        }
        tasks.push(tokio::spawn(accept_loop(tcp, acceptor, web)));
        Ok(WebHandle {
            addr,
            origin: format!("https://{addr}"),
            certificate_sha256: identity.pinned_sha256,
            plain_origin,
            tasks,
            legacy,
        })
    }
}

impl Relay {
    /// What `/session/finish` advertises: calls only where rooms are governed.
    fn web_features(&self) -> u64 {
        let voice = if self.rooms().is_some() {
            protocol::FEATURE_VOICE
        } else {
            0
        };
        protocol::FEATURE_RESUME | protocol::FEATURE_COMPACTION | voice
    }

    /// The voice carrier of the calls in this relay's rooms, served beside
    /// the live carrier on the listener at `addr`; `None` when it governs no
    /// rooms.
    fn voice_carrier(
        &self,
        tls: &TlsSource,
        addr: SocketAddr,
        pinned_sha256: Option<String>,
    ) -> Option<tilo_moq::relay::Relay> {
        let rooms = self.rooms()?;
        // Grants name the host a device dials: the certificate's name when
        // the listener is bound to every address.
        let host = match tls {
            TlsSource::Development { hosts, .. } => hosts.first().cloned(),
            TlsSource::Managed { domain } => Some(domain.clone()),
            TlsSource::Files { .. } => None,
        }
        .filter(|_| addr.ip().is_unspecified())
        .unwrap_or_else(|| addr.ip().to_string());
        let host = if host.contains(':') {
            format!("[{host}]")
        } else {
            host
        };
        Some(rooms.voice_carrier(
            format!(
                "https://{host}:{}{}",
                addr.port(),
                tilo_moq::relay::VOICE_PATH
            ),
            pinned_sha256.into_iter().collect(),
        ))
    }
}

/// What the relay's sessions hand the browser plane: native members' live
/// frames, and the legacy windows sessions share with this host.
struct Bridge {
    live: LiveRelay,
    legacy: Arc<legacy::LegacyWindows>,
}

impl crate::peer::BrowserBridge for Bridge {
    fn live(&self, workspace_id: u64, from: [u8; 32], frame: &[u8]) {
        let room = live_room(workspace_id);
        let publisher = hex::encode(from);
        match LiveFrame::decode(frame) {
            Ok(LiveFrame::Presence(_)) => {
                self.live.inject_presence(&room, &publisher, frame);
            }
            Ok(LiveFrame::Ops(ops)) => {
                let record = hex::encode(ops.record().digest());
                self.live.inject_ops(&room, &publisher, &record, frame);
            }
            Err(_) => {}
        }
    }

    fn presence(&self, workspace_id: u64, from: [u8; 32], user: [u8; 32], payload: &[u8]) {
        self.legacy
            .observe_presence(workspace_id, from, user, payload);
    }
}

/// Look after the legacy windows on the live relay for as long as the
/// listener runs: replays, indexes, and the departure of their sessions.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the guard only lives for the lookup; the value kept is a clone"
)]
async fn maintain_legacy(legacy: Arc<legacy::LegacyWindows>, context: crate::daemon::WebContext) {
    let mut tick = tokio::time::interval(legacy::MAINTAIN_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        for (workspace_id, device) in legacy.publishers() {
            let workspace = context
                .relay
                .workspaces
                .lock()
                .await
                .get(&workspace_id)
                .cloned();
            let (here, authority) = match workspace {
                Some(workspace) => {
                    let runtime = workspace.lock().await;
                    (
                        runtime.has_subscriber(&device),
                        runtime.room_authority.clone(),
                    )
                }
                None => (false, None),
            };
            let revoked = context
                .relay
                .revocations
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .is_revoked(&device);
            let writable = match authority {
                Some(authority) => match authority.rooms() {
                    Some(rooms) => matches!(
                        rooms.legacy_writer_role(authority.room(), device).await,
                        Some(collab_types::Role::Owner | collab_types::Role::Editor)
                    ),
                    None => false,
                },
                None => true,
            };
            if revoked || !writable {
                legacy.revoke(workspace_id, device, std::time::Instant::now());
                continue;
            }
            legacy.present(workspace_id, device, here, std::time::Instant::now());
        }
        legacy.maintain(std::time::Instant::now());
    }
}

/// Forward a member's input to the session streaming the window, if the
/// member may drive it: admitted to the room now and, in a room this relay
/// governs, its owner or an editor (a viewer only watches); in a room it
/// does not govern, a device of the publishing session's own user.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the guard only lives for the lookup; the value kept is a clone"
)]
async fn legacy_input(
    web: &Web,
    member: &crate::auth::AuthenticatedPeer,
    request: InputRequest,
) -> Result<Response<Body>, Response<Body>> {
    let refused = || status(StatusCode::FORBIDDEN, "you may not drive this window");
    let relay = &web.sessions.context().relay;
    let InputRequest {
        workspace_id,
        publisher,
        surface,
        events,
    } = request;
    if !live::admitted(
        &web.sessions,
        member.device_public_key,
        member.user_public_key,
        workspace_id,
    )
    .await
    {
        return Err(refused());
    }
    let publisher_user = web
        .legacy
        .publisher_user(workspace_id, publisher, surface)
        .ok_or_else(|| status(StatusCode::NOT_FOUND, "no such window"))?;
    let workspace = relay
        .workspaces
        .lock()
        .await
        .get(&workspace_id)
        .cloned()
        .ok_or_else(refused)?;
    let authority = workspace.lock().await.room_authority.clone();
    let governed = match authority {
        Some(authority) => Some((
            authority.rooms().ok_or_else(refused)?,
            authority.room().to_owned(),
        )),
        None => None,
    };
    let may = match governed {
        Some((rooms, room)) => matches!(
            rooms
                .legacy_writer_role(&room, member.device_public_key)
                .await,
            Some(collab_types::role::Role::Owner | collab_types::role::Role::Editor)
        ),
        None => member.user_public_key == publisher_user,
    };
    if !may {
        return Err(refused());
    }
    if events.is_empty() {
        return Ok(reply(StatusCode::NO_CONTENT, Vec::new()));
    }
    let payloads = web
        .legacy
        .input(
            workspace_id,
            publisher,
            surface,
            events,
            &relay.own_cert,
            std::time::Instant::now(),
        )
        .map_err(|_| status(StatusCode::NOT_FOUND, "no such window"))?;
    let workspace = workspace.lock().await;
    for payload in payloads {
        let delivered = workspace.route_lossy(
            &publisher,
            crate::Envelope::Presence {
                workspace_id,
                payload,
            },
        );
        if !delivered {
            return Err(status(
                StatusCode::SERVICE_UNAVAILABLE,
                "the window's session is not reachable",
            ));
        }
    }
    Ok(reply(StatusCode::NO_CONTENT, Vec::new()))
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
        tokio::sync::mpsc::channel::<(String, String, Vec<u8>)>(BROWSER_PRESENCE_BACKLOG);
    live.set_presence_tap(Arc::new(
        move |room: &str, publisher: &str, frame: &[u8]| {
            let _ = tx.try_send((room.to_owned(), publisher.to_owned(), frame.to_vec()));
        },
    ));
    let grants = grants.clone();
    let context = context.clone();
    tokio::spawn(async move {
        while let Some((room, publisher, frame)) = rx.recv().await {
            // Native peers attribute a live frame to a device: an agent's
            // presence stays on the browser live plane.
            let Some(device) = grants.publisher_device(&room, &publisher) else {
                continue;
            };
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
            let delivered = workspace
                .lock()
                .await
                .relay_bridged_live(&device, &envelope);
            tracing::debug!(
                workspace_id,
                from = %hex::encode(&device[..4]),
                delivered,
                "browser presence bridged to iroh live members"
            );
        }
    })
}

/// Accepted connections belong to their listener's loop: aborting the loop
/// (dropping the [`WebHandle`]) closes every open connection with it, so a
/// browser sees its host go away instead of a kept-alive orphan serving it.
async fn accept_loop(
    tcp: tokio::net::TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    web: Arc<Web>,
) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let Ok((stream, _)) = tcp.accept().await else {
            continue;
        };
        while connections.try_join_next().is_some() {}
        let acceptor = acceptor.clone();
        let web = web.clone();
        connections.spawn(async move {
            let Ok(Ok(tls)) =
                tokio::time::timeout(Duration::from_secs(10), acceptor.accept(stream)).await
            else {
                return;
            };
            serve_connection(tls, web).await;
        });
    }
}

/// The same routes as plain HTTP on a loopback listener.
async fn plain_accept_loop(tcp: tokio::net::TcpListener, web: Arc<Web>) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let Ok((stream, _)) = tcp.accept().await else {
            continue;
        };
        while connections.try_join_next().is_some() {}
        connections.spawn(serve_connection(stream, web.clone()));
    }
}

async fn serve_connection<Io>(io: Io, web: Arc<Web>)
where
    Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = hyper::service::service_fn(move |request| {
        let web = web.clone();
        async move { Ok::<_, Infallible>(handle(&web, request).await) }
    });
    let _ = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
        .serve_connection(hyper_util::rt::TokioIo::new(io), service)
        .await;
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
    // The live fallback keeps its own paths, outside `API_PREFIX`.
    let fallback =
        [FALLBACK_OPEN_PATH, FALLBACK_RESUME_PATH, FALLBACK_SEND_PATH].contains(&path.as_str());
    let Some(route) = path
        .strip_prefix(API_PREFIX)
        .or_else(|| fallback.then_some(""))
    else {
        return Err(status(StatusCode::NOT_FOUND, "not found"));
    };
    let session = token(&request);
    let live_session = request
        .headers()
        .get(FALLBACK_SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(session_id);
    let limit = if path == FALLBACK_SEND_PATH {
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
            let grant = sessions
                .finish(&finish.handshake, &finish.ack2)
                .map_err(|error| session_status(&error))?;
            Ok(match protocol::encode_grant(&grant, web.features) {
                Ok(bytes) => reply(StatusCode::OK, bytes),
                Err(_) => reply(StatusCode::INTERNAL_SERVER_ERROR, Vec::new()),
            })
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
                    None,
                )
                .await
                .map(|grant| encoded(&grant))
                .ok_or_else(|| status(StatusCode::FORBIDDEN, "not admitted to this workspace"))
        }
        "/live/grant/agent" => {
            let session = authed()?;
            let request: AgentLiveGrantRequest = decode(&bytes)?;
            if request.agent_id.is_empty() || request.agent_id.len() > MAX_AGENT_ID_BYTES {
                return Err(status(StatusCode::BAD_REQUEST, "agent id out of bounds"));
            }
            let token = session.token();
            web.grants
                .issue(
                    sessions,
                    token,
                    session.peer.device_public_key,
                    session.peer.user_public_key,
                    request.workspace_id,
                    Some(&request.agent_id),
                )
                .await
                .map(|grant| encoded(&grant))
                .ok_or_else(|| status(StatusCode::FORBIDDEN, "not admitted to this workspace"))
        }
        "/live/legacy-input" => {
            let session = authed()?;
            let request: InputRequest = decode(&bytes)?;
            if !request.valid() {
                return Err(status(StatusCode::BAD_REQUEST, "input out of bounds"));
            }
            if !request.supported() {
                return Err(status(
                    StatusCode::NOT_IMPLEMENTED,
                    "the native peer has no wheel or clipboard input endpoint",
                ));
            }
            legacy_input(web, &session.peer, request).await
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
