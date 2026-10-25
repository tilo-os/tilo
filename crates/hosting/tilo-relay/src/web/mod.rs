//! The host's browser listener (feature `web`): PR 541's carriers, served by
//! the relay itself.
//!
//! - **HTTPS** (TCP, HTTP/2 and HTTP/1.1): device certification, the session
//!   handshake, and the durable plane — `/send` and the long-polled
//!   `/events` of `sessions`. Routes are listed in
//!   [`tilo_relay_protocol::web`].
//! - **WebTransport** (UDP, HTTP/3, same port number): the live plane as
//!   `moq-lite` at [`LIVE_PATH`](tilo_relay_protocol::web::LIVE_PATH), the
//!   [`tilo_moq::live::LiveRelay`] ported from PR 541, admitted by
//!   `live::Authority`. Browsers without WebTransport use its HTTPS
//!   fallback routes.
//!
//! Browser sessions are ordinary relay peers (see `sessions`), and live
//! frames from iroh peers are bridged into the live relay, so browser and
//! native peers share one host order and one live room.
//!
//! No owner loop runs here: every handler is async and awaits the relay's
//! own locks; workspace I/O already runs on the blocking pool. The only
//! periodic work is the 50 ms live permission observation and the session
//! janitor, both on tokio intervals.

mod envelopes;
mod fetch;
mod here;
pub mod install;
mod live;
mod sessions;
#[cfg(test)]
mod tests;
mod tls;

pub use fetch::FetchProxySetup;
pub use tls::TlsSource;
// The legacy-window bridge decodes the desktop's messages, so it lives with
// the shell payload adapter.
use tilo_relay_shell::legacy;

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
use tilo_relay_protocol::legacy_window::{CodecsRequest, InputRequest};
use tilo_relay_protocol::live::{LiveKind, SealedLiveFrame};
pub(crate) use tilo_relay_protocol::web as protocol;
use tilo_relay_protocol::web::{
    API_PREFIX, AgentLiveGrantRequest, EventsRequest, HostInfo, MAX_AGENT_ID_BYTES, MAX_POLL_MS,
    MAX_REQUEST_BYTES, NodeInfo, SESSION_HEADER, SessionFinish, live_room,
};
use tilo_user_identity::CertificationRequest;

use crate::daemon::{Relay, RelayError};
use sessions::{SessionError, Sessions};
use tilo_lock::RwLockRecover as _;

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
    /// The `https://` origin a TLS-terminating proxy publishes the plain
    /// listener at. When set, the plain listener may bind a non-loopback
    /// address (the proxy is the way in), and this origin is what pages
    /// and links are told.
    pub public_origin: Option<String>,
    /// The `https://` origin browsers dial WebTransport (and voice) at, when
    /// a proxy in front of the page passes HTTP only (a CDN): QUIC reaches
    /// `listen` directly under this name, with the certificate from `tls`,
    /// while pages and the HTTPS stream fallback stay on the page origin.
    pub live_origin: Option<String>,
    /// How many browsers the listener serves at once.
    pub limits: WebLimits,
    /// Where the proxy in front of the host names a client's country. The
    /// host shows it to a person approving a new device and looks nothing up
    /// itself.
    pub country: Option<CountryHeader>,
    /// The fetch proxy this host serves its members' pages at
    /// [`protocol::FETCH_PATH`], if any.
    pub fetch_proxy: Option<FetchProxySetup>,
}

/// The request header a proxy in front of the host names the client's
/// country in (such as a CDN's `CF-IPCountry`), and the peers it is
/// believed from: that proxy, never the open internet.
#[derive(Clone, Debug)]
pub struct CountryHeader {
    pub name: String,
    /// The peers whose header is believed.
    pub proxies: tilo_relay_protocol::proxies::TrustedProxies,
}

/// Caps on concurrent browser state. A public room expects hundreds of
/// visitors, so its host raises them; the defaults suit a personal host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebLimits {
    /// Open browser sessions.
    pub max_sessions: usize,
    /// Session handshakes in flight.
    pub max_handshakes: usize,
    /// Live-plane grants outstanding.
    pub max_live_grants: usize,
}

impl Default for WebLimits {
    fn default() -> Self {
        Self {
            max_sessions: 256,
            max_handshakes: 64,
            max_live_grants: 512,
        }
    }
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
    locator_origin: Arc<std::sync::RwLock<Option<Arc<String>>>>,
    published_origin: Option<Arc<String>>,
    sessions: Arc<Sessions>,
}

impl WebHandle {
    /// Open browser sessions per person (user root) now, for a public
    /// host's presence count and idle-guest pruning. One person on two
    /// devices is one entry; a shared spectator person is one entry whose
    /// count is its sessions, seated or still waiting for a seat (how many
    /// watch is [`crate::rooms::Rooms::seated_spectators`]).
    #[must_use]
    pub fn online_users(&self) -> std::collections::BTreeMap<[u8; 32], usize> {
        self.sessions.online_users()
    }
}

impl Drop for WebHandle {
    fn drop(&mut self) {
        self.legacy.close();
        if let Ok(mut published) = self.locator_origin.write()
            && self.published_origin.as_ref().is_some_and(|ours| {
                published
                    .as_ref()
                    .is_some_and(|current| Arc::ptr_eq(ours, current))
            })
        {
            *published = None;
        }
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
    /// The rooms this relay governs, for `/rooms` and `/join`.
    rooms: Option<crate::rooms::Rooms>,
    /// Their governor's private door, for `/private`: the one a native
    /// device reaches over iroh.
    private: Option<crate::rooms::PrivateProtocol>,
    /// Members' root envelopes, for `/envelope`.
    envelopes: envelopes::Envelopes,
    /// Members' profiles, for `/profile`.
    profiles: envelopes::Envelopes,
    /// Who is here and what they are doing, for `/here`.
    here: Arc<here::Here>,
    /// [`WebConfig::country`], its header name parsed.
    country: Option<(
        hyper::header::HeaderName,
        tilo_relay_protocol::proxies::TrustedProxies,
    )>,
    /// The fetch proxy, when [`WebConfig::fetch_proxy`] asked for one.
    fetch: Option<tilo_net::proxy::host::FetchProxy>,
}

/// Checks the published origins against the listeners they front, and
/// returns the live origin without a trailing slash.
fn checked_origins(config: &WebConfig) -> Result<Option<String>, String> {
    if config.public_origin.is_none()
        && config
            .plain_listen
            .is_some_and(|plain| !plain.ip().is_loopback())
    {
        return Err(
            "the plain HTTP listener must be a loopback address unless a proxy publishes it".into(),
        );
    }
    if config
        .public_origin
        .as_ref()
        .is_some_and(|origin| !origin.starts_with("https://") || config.plain_listen.is_none())
    {
        return Err(
            "a public origin is an https:// origin published for the plain listener".into(),
        );
    }
    let live_origin = config
        .live_origin
        .as_deref()
        .map(|origin| origin.trim_end_matches('/').to_owned());
    if live_origin
        .as_deref()
        .is_some_and(|origin| !origin.starts_with("https://") || origin.len() <= 8)
    {
        return Err("a live origin is an https:// origin".into());
    }
    Ok(live_origin)
}

/// An unspecified listener is not a usable browser destination. Only its
/// configured certificate host can provide one; no caller URL is accepted.
fn browser_locator_origin(tls: &TlsSource, addr: SocketAddr) -> Result<Option<String>, RelayError> {
    let host = match tls {
        TlsSource::Files { host, .. } => host
            .clone()
            .or_else(|| (!addr.ip().is_unspecified()).then(|| addr.ip().to_string())),
        TlsSource::Development { hosts, .. } => {
            let bound = addr.ip().to_string();
            if !addr.ip().is_unspecified() && hosts.contains(&bound) {
                Some(bound)
            } else {
                hosts
                    .iter()
                    .find(|host| {
                        !host
                            .parse::<std::net::IpAddr>()
                            .is_ok_and(|ip| ip.is_unspecified())
                    })
                    .cloned()
            }
        }
        TlsSource::Managed { domain } => Some(domain.clone()),
    };
    let Some(host) = host else {
        return Ok(None);
    };
    if host
        .parse::<std::net::IpAddr>()
        .is_ok_and(|ip| ip.is_unspecified())
    {
        return Ok(None);
    }
    let authority = if host.contains(':') {
        format!("[{host}]:{}", addr.port())
    } else {
        format!("{host}:{}", addr.port())
    };
    let origin = format!("https://{authority}");
    let uri: hyper::Uri = origin
        .parse()
        .map_err(|_| RelayError::Identity("invalid configured browser locator host".into()))?;
    if uri
        .authority()
        .is_none_or(|value| value.as_str() != authority || value.as_str().contains('@'))
    {
        return Err(RelayError::Identity(
            "invalid configured browser locator host".into(),
        ));
    }
    Ok(Some(if addr.port() == 443 {
        origin.strip_suffix(":443").unwrap_or(&origin).to_owned()
    } else {
        origin
    }))
}

impl Relay {
    /// Make `origin` the one browser locators name, and return it shared.
    fn publish_browser_origin(
        &self,
        origin: Option<String>,
    ) -> Result<Option<Arc<String>>, RelayError> {
        let published = origin.map(Arc::new);
        self.browser_origin
            .write()
            .map_err(|_| RelayError::Iroh("web listener: browser locator lock poisoned".into()))?
            .clone_from(&published);
        Ok(published)
    }

    /// Serve browsers: HTTPS and WebTransport on `config.listen`.
    ///
    /// # Errors
    ///
    /// Returns [`RelayError::Iroh`] (the transport error kind) when the
    /// certificate cannot be loaded or an address cannot be bound.
    pub async fn serve_web(&self, config: WebConfig) -> Result<WebHandle, RelayError> {
        let failed = |message: String| RelayError::Iroh(format!("web listener: {message}"));
        let live_origin = checked_origins(&config).map_err(failed)?;
        let identity = tls::Identity::load(&config.tls).map_err(failed)?;
        let (tcp, udp) =
            bind_port_pair(config.listen).map_err(|error| failed(error.to_string()))?;
        let addr = tcp
            .local_addr()
            .map_err(|error| failed(error.to_string()))?;
        let public_origin = match config.public_origin.clone() {
            Some(origin) => Some(origin),
            None => browser_locator_origin(&config.tls, addr)?,
        };
        let certificate = identity.certificate().map_err(failed)?;
        let endpoint = tilo_moq::carrier::listener_on(udp, certificate.clone()).map_err(failed)?;
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(
            tilo_moq::tls::https_server_config(certificate).map_err(failed)?,
        ));
        let fetch = fetch::Serving::start(config.fetch_proxy.as_ref()).map_err(failed)?;
        let context = self.web_context().clone();
        let sessions = Sessions::with_limits(context.clone(), config.limits);
        let grants = live::LiveGrants::with_limit(config.limits.max_live_grants);
        let live = LiveRelay::new(Arc::new(live::Authority(grants.clone())));
        let legacy =
            legacy::LegacyWindows::new(live.clone(), context.relay.identity.cert.device_public_key);
        let _ = context.relay.live_bridge.set(Arc::new(Bridge {
            live: live.clone(),
            legacy: legacy.clone(),
        }));
        let tasks_bridge = bridge_browser_presence(&live, &grants, &context);
        let tasks_legacy = tokio::spawn(maintain_legacy(
            legacy.clone(),
            grants.clone(),
            context.clone(),
        ));
        let web = Arc::new(Web {
            sessions: sessions.clone(),
            grants: grants.clone(),
            legacy: legacy.clone(),
            live,
            host_info: HostInfo {
                host_cert: context.relay.identity.cert,
                // Behind a TLS-terminating proxy the page only reaches the
                // proxy's HTTPS; WebTransport (UDP) does not pass through it,
                // so it is announced only with a direct origin of its own.
                webtransport_port: (config.public_origin.is_none() || live_origin.is_some())
                    .then_some(addr.port()),
                certificate_sha256: identity.pinned_sha256.clone(),
                webtransport_origin: live_origin.clone(),
            },
            allowed_origins: config.allowed_origins,
            features: self.web_features() | fetch.features(),
            rooms: self.rooms().cloned(),
            private: self
                .rooms()
                .cloned()
                .map(crate::rooms::PrivateProtocol::new),
            envelopes: envelopes::Envelopes::new(self.state_root().join("envelopes")),
            profiles: envelopes::Envelopes::of(
                envelopes::Kind::Profile,
                self.state_root().join("profiles"),
            ),
            here: Arc::new(here::Here::new(here::from_env())),
            country: config.country.and_then(country_header),
            fetch: fetch.proxy,
        });
        let mut tasks = vec![tasks_bridge, tasks_legacy];
        tasks.extend(fetch.metrics);
        let live = web.live.clone();
        tasks.push(
            match self.voice_carrier(
                &config.tls,
                addr,
                live_origin.as_deref(),
                identity.pinned_sha256.clone(),
            ) {
                Some(voice) => tokio::spawn(live.serve_with_voice(endpoint, voice)),
                None => tokio::spawn(live.serve(endpoint)),
            },
        );
        tasks.push(tokio::spawn(grants.follow(sessions.clone())));
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
        let published_origin = self.publish_browser_origin(public_origin.clone())?;
        Ok(WebHandle {
            addr,
            origin: public_origin.unwrap_or_else(|| format!("https://{addr}")),
            certificate_sha256: identity.pinned_sha256,
            plain_origin,
            tasks,
            legacy,
            locator_origin: Arc::clone(&self.browser_origin),
            published_origin,
            sessions,
        })
    }
}

impl Relay {
    /// What `/session/finish` advertises: calls and room administration
    /// only where rooms are governed.
    fn web_features(&self) -> u64 {
        let rooms = if self.rooms().is_some() {
            protocol::FEATURE_VOICE | protocol::FEATURE_ROOMS
        } else {
            0
        };
        protocol::FEATURE_RESUME | protocol::FEATURE_COMPACTION | rooms
    }

    /// The voice carrier of the calls in this relay's rooms, served beside
    /// the live carrier on the listener at `addr`; `None` when it governs no
    /// rooms.
    fn voice_carrier(
        &self,
        tls: &TlsSource,
        addr: SocketAddr,
        live_origin: Option<&str>,
        pinned_sha256: Option<String>,
    ) -> Option<tilo_moq::relay::Relay> {
        let rooms = self.rooms()?;
        if let Some(origin) = live_origin {
            return Some(rooms.voice_carrier(
                format!("{origin}{}", tilo_moq::relay::VOICE_PATH),
                pinned_sha256.into_iter().collect(),
            ));
        }
        // Grants name the host a device dials: the certificate's name when
        // the listener is bound to every address.
        let host = match tls {
            TlsSource::Development { hosts, .. } => hosts
                .iter()
                .find(|host| {
                    host.parse::<std::net::IpAddr>()
                        .map_or(true, |ip| !ip.is_unspecified())
                })
                .cloned(),
            TlsSource::Managed { domain } => Some(domain.clone()),
            TlsSource::Files { host, .. } => host.clone(),
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
        // Only a sealed frame crosses, by its clear kind and record.
        match SealedLiveFrame::decode(frame) {
            Ok(sealed) if sealed.kind == LiveKind::Presence => {
                self.live.inject_presence(&room, &publisher, frame);
            }
            Ok(sealed) => {
                let record = hex::encode(sealed.record);
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
async fn maintain_legacy(
    legacy: Arc<legacy::LegacyWindows>,
    grants: Arc<live::LiveGrants>,
    context: crate::daemon::WebContext,
) {
    let mut tick = tokio::time::interval(legacy::MAINTAIN_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tick.tick().await;
        advertise_legacy_codecs(&legacy, &grants, &context).await;
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
                .identity
                .revocations
                .read_recover()
                .is_revoked(&device);
            let writable = match authority {
                Some(authority) => match authority.rooms() {
                    Some(rooms) => rooms
                        .legacy_writer_role(authority.room(), device)
                        .await
                        .is_some_and(crate::room_access::role_writes),
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

/// Tell the sessions of every room with members in its live plane what
/// those members decode ([`legacy::LegacyWindows::advertise`]), so a session
/// picks a codec for this host's windows that every member's page plays, and
/// ask for the keyframes streams that lost a unit need
/// ([`legacy::LegacyWindows::keyframe_requests`]).
/// Lossy like presence: a full queue misses one advert, and the next restates it.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the guard only lives for the lookup; the values kept are clones"
)]
async fn advertise_legacy_codecs(
    legacy: &legacy::LegacyWindows,
    grants: &live::LiveGrants,
    context: &crate::daemon::WebContext,
) {
    let mut rooms = Vec::new();
    let mut queues = std::collections::HashMap::new();
    for (workspace_id, members) in grants.viewers() {
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
        let runtime = workspace.lock().await;
        let sessions = runtime.subscribers.keys().copied().collect::<Vec<_>>();
        for device in &sessions {
            if let Some(queue) = runtime.subscriber_queue(device) {
                queues.insert((workspace_id, *device), queue);
            }
        }
        drop(runtime);
        rooms.push(legacy::RoomViewers {
            workspace_id,
            members,
            sessions,
        });
    }
    let now = std::time::Instant::now();
    let mut adverts = legacy.advertise(&rooms, &context.relay.identity.cert, now);
    // A stream that lost a unit on its way here asks its session for a
    // keyframe, so members watching it recover at once.
    adverts.extend(legacy.keyframe_requests(&context.relay.identity.cert, now));
    for advert in adverts {
        let Some(queue) = queues.get(&(advert.workspace_id, advert.device)) else {
            continue;
        };
        for payload in advert.payloads {
            let envelope = crate::Envelope::Presence {
                workspace_id: advert.workspace_id,
                payload,
            };
            if queue.try_send(envelope).is_err() {
                break;
            }
        }
    }
}

/// How long one input request waits for room in the session's queue.
const LEGACY_INPUT_DELIVERY: std::time::Duration = std::time::Duration::from_secs(5);

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
    if !live::admitted(&web.sessions, member, workspace_id).await {
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
        Some((rooms, room)) => rooms
            .legacy_writer_role(&room, member.device_public_key)
            .await
            .is_some_and(crate::room_access::role_writes),
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
            &relay.identity.cert,
            std::time::Instant::now(),
        )
        .map_err(|_| status(StatusCode::NOT_FOUND, "no such window"))?;
    let unreachable = || {
        status(
            StatusCode::SERVICE_UNAVAILABLE,
            "the window's session is not reachable",
        )
    };
    let queue = workspace
        .lock()
        .await
        .subscriber_queue(&publisher)
        .ok_or_else(unreachable)?;
    // Every event, in order: a key-up lost or a batch cut short leaves keys
    // held or typed text missing letters. A busy session is waited for.
    let delivered = tokio::time::timeout(LEGACY_INPUT_DELIVERY, async {
        for payload in payloads {
            let envelope = crate::Envelope::Presence {
                workspace_id,
                payload,
            };
            queue.send(envelope).await.ok()?;
        }
        Some(())
    })
    .await;
    match delivered {
        Ok(Some(())) => Ok(reply(StatusCode::NO_CONTENT, Vec::new())),
        Ok(None) | Err(_) => Err(unreachable()),
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

/// Ephemeral ports tried for a TCP and UDP pair before giving up.
const PORT_PAIR_ATTEMPTS: usize = 64;

/// HTTPS (TCP) and WebTransport (UDP) sockets on one port of `listen`. For
/// port 0 the kernel picks the TCP port, which another socket may hold for
/// UDP; the next pick is tried then.
fn bind_port_pair(
    listen: SocketAddr,
) -> std::io::Result<(tokio::net::TcpListener, std::net::UdpSocket)> {
    let mut attempts = 0;
    loop {
        let socket = if listen.is_ipv4() {
            tokio::net::TcpSocket::new_v4()?
        } else {
            tokio::net::TcpSocket::new_v6()?
        };
        // As `TcpListener::bind`: a restarted host takes its port back while
        // its old connections linger in TIME_WAIT.
        socket.set_reuseaddr(true)?;
        socket.bind(listen)?;
        let tcp = socket.listen(1024)?;
        match std::net::UdpSocket::bind(tcp.local_addr()?) {
            Ok(udp) => return Ok((tcp, udp)),
            Err(error)
                if listen.port() == 0
                    && error.kind() == std::io::ErrorKind::AddrInUse
                    && attempts < PORT_PAIR_ATTEMPTS =>
            {
                attempts = attempts.saturating_add(1);
            }
            Err(error) => return Err(error),
        }
    }
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
        let Ok((stream, peer)) = tcp.accept().await else {
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
            serve_connection(tls, web, peer).await;
        });
    }
}

/// The same routes as plain HTTP on a loopback listener.
async fn plain_accept_loop(tcp: tokio::net::TcpListener, web: Arc<Web>) {
    let mut connections = tokio::task::JoinSet::new();
    loop {
        let Ok((stream, peer)) = tcp.accept().await else {
            continue;
        };
        while connections.try_join_next().is_some() {}
        connections.spawn(serve_connection(stream, web.clone(), peer));
    }
}

/// The TCP peer a request arrived from.
#[derive(Clone, Copy)]
struct Peer(SocketAddr);

async fn serve_connection<Io>(io: Io, web: Arc<Web>, peer: SocketAddr)
where
    Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let service = hyper::service::service_fn(move |mut request: Request<hyper::body::Incoming>| {
        let web = web.clone();
        request.extensions_mut().insert(Peer(peer));
        async move { Ok::<_, Infallible>(handle(&web, request).await) }
    });
    let _ = hyper_util::server::conn::auto::Builder::new(hyper_util::rt::TokioExecutor::new())
        .serve_connection(hyper_util::rt::TokioIo::new(io), service)
        .await;
}

/// A response body: whole bytes, a live fallback stream, or what the fetch
/// proxy relays.
pub(crate) enum Body {
    Full(Option<Bytes>),
    Stream(FallbackStream),
    Proxied(tilo_net::proxy::host::ProxyBody),
}

impl http_body::Body for Body {
    type Data = Bytes;
    /// Only a relayed body fails: it breaks off at the proxy's bounds, so
    /// the page sees an error rather than a short body that reads as whole.
    type Error = tilo_net::proxy::host::ProxyBodyError;

    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Bytes>, Self::Error>>> {
        match self.get_mut() {
            Self::Full(bytes) => {
                Poll::Ready(bytes.take().map(|bytes| Ok(http_body::Frame::data(bytes))))
            }
            Self::Stream(stream) => stream
                .poll_next(cx)
                .map(|chunk| chunk.map(|bytes| Ok(http_body::Frame::data(bytes)))),
            Self::Proxied(body) => Pin::new(body).poll_frame(cx),
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

fn json_status<T: serde::Serialize>(code: StatusCode, value: &T) -> Response<Body> {
    match serde_json::to_vec(value) {
        Ok(json) => {
            let mut response = reply(code, json);
            response.headers_mut().insert(
                header::CONTENT_TYPE,
                header::HeaderValue::from_static("application/json"),
            );
            response
        }
        Err(_) => reply(StatusCode::INTERNAL_SERVER_ERROR, Vec::new()),
    }
}

fn json<T: serde::Serialize>(value: &T) -> Response<Body> {
    json_status(StatusCode::OK, value)
}

/// A public workspace's door (`/directory/<room>/join` or `…/report`):
/// JSON in, JSON out, a refusal as `{error}` the person reads.
#[allow(
    clippy::result_large_err,
    reason = "the error is the response the handler sends as it is"
)]
async fn directory_door(
    web: &Web,
    route: &str,
    bytes: &[u8],
) -> Result<Response<Body>, Response<Body>> {
    use tilo_relay_protocol::directory::{DoorAnswer, DoorJoin, DoorReport, door_route};
    let (room, step) =
        door_route(route).ok_or_else(|| status(StatusCode::NOT_FOUND, "not found"))?;
    // A relay that governs no rooms refuses before the request is parsed.
    governed_rooms(web)?;
    let refused = |error: crate::rooms::CollaborationError| {
        json_status(
            StatusCode::FORBIDDEN,
            &serde_json::json!({ "error": error.to_string() }),
        )
    };
    match step {
        "join" => {
            let join: DoorJoin = serde_json::from_slice(bytes)
                .map_err(|_| status(StatusCode::BAD_REQUEST, "not a join request"))?;
            let (issued, token) = governed_rooms(web)?
                .admit_listed(room, &join)
                .await
                .map_err(refused)?;
            Ok(json(&DoorAnswer {
                invitation: base64::Engine::encode(
                    &base64::engine::general_purpose::URL_SAFE_NO_PAD,
                    issued.signed_invitation,
                ),
                admission_token: token,
                origin: None,
            }))
        }
        "report" => {
            let report: DoorReport = serde_json::from_slice(bytes)
                .map_err(|_| status(StatusCode::BAD_REQUEST, "not a report"))?;
            governed_rooms(web)?
                .report_listed(room, &report)
                .await
                .map_err(refused)?;
            Ok(reply(StatusCode::NO_CONTENT, Vec::new()))
        }
        _ => Err(status(StatusCode::NOT_FOUND, "not found")),
    }
}

/// The rooms this relay governs, or the answer that it governs none.
#[allow(
    clippy::result_large_err,
    reason = "the error is the response the handler sends as it is"
)]
fn governed_rooms(web: &Web) -> Result<&crate::rooms::Rooms, Response<Body>> {
    web.rooms
        .as_ref()
        .ok_or_else(|| status(StatusCode::NOT_FOUND, "this relay governs no rooms"))
}

fn session_status(error: &SessionError) -> Response<Body> {
    match error {
        SessionError::Gone => status(StatusCode::UNAUTHORIZED, "session ended"),
        // Typed, so the device acts on the host's reason (see
        // `protocol::SessionRefusal`).
        SessionError::Denied(refusal) => match postcard::to_allocvec(refusal) {
            Ok(body) => reply(StatusCode::FORBIDDEN, body),
            Err(_) => reply(StatusCode::INTERNAL_SERVER_ERROR, Vec::new()),
        },
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

/// The device a workspace join request over HTTPS is bound to: the one it
/// names, whose signature its proof carries. The iroh exchange binds the
/// device its transport authenticated; an inspection binds none.
fn joining_device(request: &tilo_peer_state::join::JoinTransportRequest) -> [u8; 32] {
    use tilo_peer_state::join::JoinTransportRequest;
    match request {
        JoinTransportRequest::Inspect { .. } => [0; 32],
        JoinTransportRequest::Challenge(request) => request.device_cert.device_public_key,
        JoinTransportRequest::Complete(request) => request.proof.challenge.device_public_key,
        JoinTransportRequest::MembershipChallenge(request) => request.device_cert.device_public_key,
        JoinTransportRequest::MembershipComplete(request) => {
            request.proof.challenge.device_public_key
        }
    }
}

/// The client's country as the proxy in front of the host names it
/// ([`WebConfig::country`]): from a trusted peer only, and only a two-letter
/// code.
fn country(web: &Web, request: &Request<hyper::body::Incoming>) -> Option<String> {
    let (header, proxies) = web.country.as_ref()?;
    let peer = request.extensions().get::<Peer>()?;
    if !proxies.contains(peer.0.ip()) {
        return None;
    }
    let value = request.headers().get(header)?.to_str().ok()?.trim();
    (value.len() == 2 && value.bytes().all(|byte| byte.is_ascii_uppercase()) && value != "XX")
        .then(|| value.to_owned())
}

fn token(request: &Request<hyper::body::Incoming>) -> Option<[u8; 32]> {
    let value = request.headers().get(SESSION_HEADER)?.to_str().ok()?;
    hex::decode(value).ok()?.try_into().ok()
}

/// Headers a page may send to the browser routes.
const ALLOWED_HEADERS: &str = "content-type, x-tilo-session, x-tilo-live-session";

fn cors(
    allowed_origins: &[String],
    allowed_headers: &str,
    origin: Option<&header::HeaderValue>,
    response: &mut Response<Body>,
) {
    let Some(origin) = origin.and_then(|value| value.to_str().ok()) else {
        return;
    };
    if !allowed_origins
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
    if let Ok(allowed) = header::HeaderValue::from_str(allowed_headers) {
        headers.insert(header::ACCESS_CONTROL_ALLOW_HEADERS, allowed);
    }
    headers.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        header::HeaderValue::from_static("x-tilo-live-session"),
    );
}

async fn handle(web: &Web, request: Request<hyper::body::Incoming>) -> Response<Body> {
    // The fetch proxy takes the method of the request it relays.
    if request
        .uri()
        .path()
        .strip_prefix(API_PREFIX)
        .is_some_and(|route| route == protocol::FETCH_PATH)
    {
        return fetch::handle(web, request).await;
    }
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
    cors(
        &web.allowed_origins,
        ALLOWED_HEADERS,
        origin.as_ref(),
        &mut response,
    );
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

/// Whether `user` is admitted to any workspace this relay serves: whose
/// envelope it keeps.
async fn admitted_anywhere(web: &Web, user: [u8; 32]) -> bool {
    let workspaces: Vec<_> = web
        .sessions
        .context()
        .relay
        .workspaces
        .lock()
        .await
        .values()
        .cloned()
        .collect();
    for workspace in workspaces {
        let workspace = workspace.lock().await;
        // A governed room names its members' roots apart from
        // the roots its signed authority admitted.
        if workspace.admitted_users.contains(&user) || workspace.governed_user_roots.contains(&user)
        {
            return true;
        }
    }
    false
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
    let country = country(web, &request);
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
    let certification = &web.sessions.context().certification;
    #[allow(
        clippy::result_large_err,
        reason = "the error is the response the handler sends as it is"
    )]
    let authed = || {
        session
            .ok_or_else(|| status(StatusCode::UNAUTHORIZED, "no session"))
            .and_then(|token| {
                web.sessions
                    .get(&token)
                    .map_err(|error| session_status(&error))
            })
    };
    match route {
        "/host" => Ok(encoded(&web.host_info)),
        "/node" => {
            authed()?;
            Ok(encoded(&NodeInfo {
                session: web.sessions.context().node_session.get().copied(),
            }))
        }
        "/certify" => {
            let request: CertificationRequest = decode(&bytes)?;
            // A member of a workspace here pairs their devices through it;
            // the member's own devices answer (`/certify/approve`).
            let member = admitted_anywhere(web, request.user_public_key).await;
            Ok(encoded(&match certification.enqueue_member(
                request,
                tilo_clock::unix_now_ns(),
                member,
                country,
            ) {
                Ok((_, pairing_code, _)) => CertifyReply::Pending { pairing_code },
                Err(reason) => CertifyReply::Denied { reason },
            }))
        }
        "/certify/approve" => {
            let approval: crate::certify::MemberApproval = decode(&bytes)?;
            if !admitted_anywhere(web, approval.user_public_key).await {
                return Err(status(
                    StatusCode::FORBIDDEN,
                    "only a member approves devices here",
                ));
            }
            Ok(encoded(
                &certification.member_approve(approval, tilo_clock::unix_now_ns()),
            ))
        }
        "/certify/wait" => {
            let digest: [u8; 32] = decode(&bytes)?;
            Ok(encoded(
                &certification.wait(&digest, wait(MAX_POLL_MS)).await,
            ))
        }
        "/session" => {
            // A device at another peer protocol version is told this host's
            // and reads nothing else.
            let hello = match crate::auth::decode_handshake(&bytes) {
                Ok(hello) => hello,
                Err(crate::auth::HandshakeFrameError::Version { theirs }) => {
                    tracing::info!(
                        theirs,
                        "a browser device at another protocol version was told this host's"
                    );
                    return Ok(match crate::auth::version_answer() {
                        Ok(answer) => reply(StatusCode::CONFLICT, answer),
                        Err(_) => reply(StatusCode::INTERNAL_SERVER_ERROR, Vec::new()),
                    });
                }
                Err(crate::auth::HandshakeFrameError::Codec(_)) => {
                    return Err(status(StatusCode::BAD_REQUEST, "malformed body"));
                }
            };
            web.sessions
                .hello(&hello)
                .map(|challenge| encoded(&challenge))
                .map_err(|error| session_status(&error))
        }
        "/session/finish" => {
            let finish: SessionFinish = decode(&bytes)?;
            let grant = web
                .sessions
                .finish(&finish.handshake, &finish.ack2)
                .map_err(|error| session_status(&error))?;
            Ok(match protocol::encode_grant(&grant, web.features) {
                Ok(bytes) => reply(StatusCode::OK, bytes),
                Err(_) => reply(StatusCode::INTERNAL_SERVER_ERROR, Vec::new()),
            })
        }
        // A page leaving says so in a beacon, its token the body (no
        // header rides a beacon): its session ends now, not when its polls
        // stop. Whoever holds a token may end it, as by going quiet.
        tilo_relay_protocol::web::SESSION_END_PATH => {
            if let Some(token) = std::str::from_utf8(&bytes)
                .ok()
                .and_then(|text| hex::decode(text.trim()).ok())
                .and_then(|token| <[u8; 32]>::try_from(token).ok())
            {
                web.sessions.end(&token);
            }
            Ok(reply(StatusCode::NO_CONTENT, Vec::new()))
        }
        "/send" => {
            let session = authed()?;
            let envelopes = decode(&bytes)?;
            web.sessions
                .send(&session, envelopes)
                .await
                .map(|()| reply(StatusCode::NO_CONTENT, Vec::new()))
                .map_err(|error| session_status(&error))
        }
        "/events" => {
            let session = authed()?;
            let request: EventsRequest = decode(&bytes)?;
            web.sessions
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
                .issue(&web.sessions, token, &session.peer, workspace_id, None)
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
                    &web.sessions,
                    token,
                    &session.peer,
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
            legacy_input(web, &session.peer, request).await
        }
        "/live/legacy-codecs" => {
            let session = authed()?;
            let request: CodecsRequest = decode(&bytes)?;
            if !request.valid() {
                return Err(status(StatusCode::BAD_REQUEST, "codecs out of bounds"));
            }
            if !live::admitted(&web.sessions, &session.peer, request.workspace_id).await {
                return Err(status(
                    StatusCode::FORBIDDEN,
                    "not admitted to this workspace",
                ));
            }
            if !web.legacy.member_codecs(
                session.token(),
                request.video_codecs(),
                std::time::Instant::now(),
            ) {
                return Err(status(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "too many members are watching",
                ));
            }
            Ok(reply(StatusCode::NO_CONTENT, Vec::new()))
        }
        protocol::HERE_PATH => {
            let session = authed()?;
            let request: protocol::HereRequest = decode(&bytes)?;
            request
                .validate()
                .map_err(|why| status(StatusCode::BAD_REQUEST, why))?;
            if !live::admitted(&web.sessions, &session.peer, request.workspace_id).await {
                return Err(status(
                    StatusCode::FORBIDDEN,
                    "not admitted to this workspace",
                ));
            }
            let workspace_id = request.workspace_id;
            let (mut reply, job) = web.here.post(
                request,
                session.peer.device_public_key,
                session.peer.user_public_key,
                std::time::Instant::now(),
                tilo_clock::unix_now_ms(),
            );
            here::pair_members(&mut reply, &web.grants.persons_in(workspace_id));
            if let Some(job) = job {
                tokio::spawn(web.here.clone().summarize(job));
            }
            Ok(encoded(&reply))
        }
        protocol::CHANNEL_KEYS_PATH => {
            let session = authed()?;
            let request: protocol::ChannelKeysRequest = decode(&bytes)?;
            let relay = &web.sessions.context().relay;
            let keys = web
                .rooms
                .as_ref()
                .ok_or_else(|| status(StatusCode::NOT_FOUND, "this relay governs no rooms"))?
                .spectator_keys(&request, |runtime| {
                    crate::room_access::decide(
                        runtime,
                        &relay.trust(),
                        &session.peer,
                        crate::room_access::RoomOperation::ChannelReadKeys,
                    )
                })
                .await
                .map_err(|why| status(StatusCode::FORBIDDEN, &why))?;
            let keys = keys
                .to_bytes()
                .map_err(|why| status(StatusCode::INTERNAL_SERVER_ERROR, &why.to_string()))?;
            Ok(encoded(&protocol::ChannelKeys {
                keys: keys.to_vec(),
            }))
        }
        tilo_relay_protocol::rooms::private::PRIVATE_PATH => {
            use tilo_relay_protocol::rooms::private::{MAX_REQUEST_BYTES, PrivateCall};
            let session = authed()?;
            let door = web
                .private
                .as_ref()
                .ok_or_else(|| status(StatusCode::NOT_FOUND, "this relay governs no rooms"))?;
            if bytes.len() > MAX_REQUEST_BYTES {
                return Err(status(StatusCode::PAYLOAD_TOO_LARGE, "body too large"));
            }
            let call: PrivateCall = decode(&bytes)?;
            let device = session.peer.device_public_key;
            let _admitted = door
                .admit(device)
                .ok_or_else(|| status(StatusCode::SERVICE_UNAVAILABLE, "busy"))?;
            // A watch is a long-poll, held as long as `/events` holds one.
            let poll = async {
                tokio::time::sleep(wait(MAX_POLL_MS)).await;
                Ok(())
            };
            let reply = door
                .serve(
                    &session.peer.cert,
                    &device,
                    &call.room,
                    call.operation,
                    poll,
                )
                .await;
            Ok(encoded(&reply))
        }
        "/rooms" => {
            let session = authed()?;
            let rooms = web
                .rooms
                .as_ref()
                .ok_or_else(|| status(StatusCode::NOT_FOUND, "this relay governs no rooms"))?;
            let request = decode(&bytes)?;
            Ok(encoded(
                &rooms.page_rooms(&session.peer.cert, request).await,
            ))
        }
        "/join" => {
            let rooms = web
                .rooms
                .as_ref()
                .ok_or_else(|| status(StatusCode::NOT_FOUND, "this relay governs no rooms"))?;
            let step = decode(&bytes)?;
            Ok(encoded(&rooms.page_join(step).await))
        }
        protocol::WORKSPACE_JOIN_PATH => {
            use tilo_delivery_protocol::workspace_join::{
                MAX_WORKSPACE_JOIN_REQUEST_BYTES, WorkspaceJoinService as _,
            };
            let rooms = web
                .rooms
                .as_ref()
                .ok_or_else(|| status(StatusCode::NOT_FOUND, "this relay governs no rooms"))?;
            if bytes.len() > MAX_WORKSPACE_JOIN_REQUEST_BYTES {
                return Err(status(
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "workspace join request exceeds the wire limit",
                ));
            }
            let request = decode(&bytes)?;
            let device = joining_device(&request);
            Ok(encoded(&rooms.join_service().handle(device, request).await))
        }
        tilo_relay_protocol::directory::DIRECTORY_PATH => {
            let rooms = web
                .rooms
                .as_ref()
                .ok_or_else(|| status(StatusCode::NOT_FOUND, "this relay governs no rooms"))?;
            let online = web.sessions.online_users().into_keys().collect();
            Ok(json(&tilo_relay_protocol::directory::DirectoryPage {
                workspaces: rooms.directory(&online).await,
            }))
        }
        _ if tilo_relay_protocol::directory::door_route(route).is_some() => {
            directory_door(web, route, &bytes).await
        }
        "/envelope" | "/profile" => {
            let now_ms = tilo_clock::unix_now_ms();
            let kept = if route == "/profile" {
                &web.profiles
            } else {
                &web.envelopes
            };
            kept.publish(&bytes, now_ms, |root| admitted_anywhere(web, root))
                .await
                .map(|()| reply(StatusCode::NO_CONTENT, Vec::new()))
                .map_err(|refusal| {
                    let code = match refusal {
                        envelopes::Refusal::Malformed(_) => StatusCode::BAD_REQUEST,
                        envelopes::Refusal::BadSignature | envelopes::Refusal::NotMember => {
                            StatusCode::FORBIDDEN
                        }
                        envelopes::Refusal::Stale => StatusCode::CONFLICT,
                        envelopes::Refusal::Storage => StatusCode::INTERNAL_SERVER_ERROR,
                    };
                    status(code, refusal.message())
                })
        }
        "/envelope/fetch" | "/profile/fetch" => match if route == "/profile/fetch" {
            web.profiles.answer(&bytes)
        } else {
            web.envelopes.answer(&bytes)
        } {
            Some(json) => {
                let mut response = reply(StatusCode::OK, json);
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    header::HeaderValue::from_static("application/json"),
                );
                Ok(response)
            }
            None => Err(status(StatusCode::BAD_REQUEST, "malformed root public key")),
        },
        _ if path == FALLBACK_OPEN_PATH => {
            let id: [u8; 16] = rand::random();
            let stream = web
                .live
                .fallback_open(&bytes, id)
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

/// The configured country header as the listener reads it; a name that is not
/// a valid header name is no header.
fn country_header(
    country: CountryHeader,
) -> Option<(
    hyper::header::HeaderName,
    tilo_relay_protocol::proxies::TrustedProxies,
)> {
    hyper::header::HeaderName::try_from(country.name.as_str())
        .ok()
        .map(|name| (name, country.proxies))
}
