//! Bounded HTTPS transport only. No native authentication or owner is installed here.

use crate::host_bytes::{ByteCredit, HostBytes};
use crate::https_assets::{AssetFile, AssetRead, AssetRoot};
use bytes::{Bytes, BytesMut};
use http_body_util::BodyExt;
use hyper::{
    Method, Request, Response, StatusCode, Version,
    body::{Body, Frame, SizeHint},
    http::request::Parts,
};
use hyper_util::{
    rt::{TokioExecutor, TokioIo, TokioTimer},
    server::conn::auto,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::{
    convert::Infallible,
    future::Future,
    io,
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};
use tilo_relay::delivery_v3::http_proof;
use tokio::{
    net::TcpListener,
    sync::{OwnedSemaphorePermit, Semaphore, mpsc, oneshot, watch},
    task::JoinSet,
    time::{Instant, timeout, timeout_at},
};

const GLOBAL_BYTES: usize = 256 * 1024 * 1024;
const HEADER_BYTES: usize = 32 * 1024;
const BODY_BYTES: usize = 1024 * 1024;
const AUTH_BYTES: usize = 8 * 1024;
const IO_BYTES: usize = crate::https_assets::READ_BYTES;
const DEADLINE: Duration = Duration::from_secs(5);
const POLL_DEADLINE: Duration = Duration::from_secs(20);
const CONNECTION_LIFETIME: Duration = Duration::from_secs(300);
/// Bounded drain after the lifetime: long enough for a multi-megabyte asset on
/// a ~256 kbit/s link, still an absolute bound on connection resources.
const CONNECTION_GRACE: Duration = Duration::from_secs(180);
const CONNECTION_BYTES: usize = 2 * 1024 * 1024;
const PROOF_HEADER: &str = "x-tilo-delivery-proof";

/// Transport route classification, never an authorization or installation witness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Route {
    Descriptor,
    Bootstrap,
    Challenge,
    Sessions,
    Onboarding(tilo_relay::delivery_v3::onboarding_wire::OnboardingOperation),
    OnboardingChunk,
    Private(tilo_relay::delivery_v3::private_wire::PrivateOperation),
    PrivatePoll,
    State,
    EventsPoll,
    Acks,
    Detach,
    Snapshot,
    SnapshotChunk,
    Media(MediaRoute),
    /// Proof-required LIVE_V1 grant issue/renewal.
    LiveGrant,
}

/// Closed inert media route registry; route selection alone grants no authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaRoute {
    GrantChallenge,
    GrantComplete,
    RelayChallenge,
    RefreshChallenge,
    RefreshComplete,
    RelayClose,
    Presence,
}
impl MediaRoute {
    pub fn path(self) -> &'static str {
        match self {
            Self::GrantChallenge => "/peer/v3/media/grant/challenge",
            Self::GrantComplete => "/peer/v3/media/grant/complete",
            Self::RelayChallenge => "/peer/v3/media/relay/challenge",
            Self::RefreshChallenge => "/peer/v3/media/relay/refresh/challenge",
            Self::RefreshComplete => "/peer/v3/media/relay/refresh/complete",
            Self::RelayClose => "/peer/v3/media/relay/close",
            Self::Presence => "/peer/v3/media/presence",
        }
    }
    fn from_path(path: &str) -> Option<Self> {
        [
            Self::GrantChallenge,
            Self::GrantComplete,
            Self::RelayChallenge,
            Self::RefreshChallenge,
            Self::RefreshComplete,
            Self::RelayClose,
            Self::Presence,
        ]
        .into_iter()
        .find(|route| route.path() == path)
    }
}
impl Route {
    fn body_limit(self) -> usize {
        match self {
            Self::State => BODY_BYTES,
            Self::Onboarding(_) | Self::OnboardingChunk | Self::Private(_) => 256 * 1024,
            Self::Bootstrap => 4096,
            Self::Challenge | Self::Sessions | Self::Acks | Self::Snapshot | Self::Media(_) => {
                AUTH_BYTES
            }
            Self::LiveGrant => tilo_relay::delivery_v3::live_wire::MAX_GRANT_BYTES,
            _ => 0,
        }
    }
    fn control(self) -> bool {
        !matches!(
            self,
            Self::State
                | Self::EventsPoll
                | Self::Media(_)
                | Self::Onboarding(_)
                | Self::OnboardingChunk
                | Self::Private(_)
                | Self::PrivatePoll
                | Self::Snapshot
                | Self::SnapshotChunk
        )
    }
}

#[derive(Debug)]
struct Origin {
    exact: String,
    authority: String,
    port: u16,
    /// `Alt-Svc` advertised on HTTP/1.1 and HTTP/2 responses once the same
    /// port also serves these routes over HTTP/3 (UDP). Clients that cannot
    /// reach UDP keep using HTTP/2.
    alt_svc: Option<hyper::header::HeaderValue>,
}
impl Origin {
    fn parse(value: &str) -> io::Result<Self> {
        let uri: hyper::Uri = value
            .parse()
            .map_err(|_| io::Error::other("invalid HTTPS origin"))?;
        let authority = uri
            .authority()
            .ok_or_else(|| io::Error::other("missing HTTPS authority"))?;
        if uri.scheme_str() != Some("https")
            || value != format!("https://{authority}")
            || !matches!(authority.host(), "localhost" | "127.0.0.1" | "[::1]")
        {
            return Err(io::Error::other("local HTTPS origin required"));
        }
        Ok(Self {
            exact: value.into(),
            authority: authority.to_string(),
            port: authority.port_u16().unwrap_or(443),
            alt_svc: None,
        })
    }
}

#[derive(Debug)]
struct Rejection {
    status: StatusCode,
    code: &'static str,
}
impl Rejection {
    fn invalid() -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            code: "invalid-frame",
        }
    }
    fn denied() -> Self {
        Self {
            status: StatusCode::FORBIDDEN,
            code: "permission-denied",
        }
    }
    fn unavailable() -> Self {
        Self {
            status: StatusCode::SERVICE_UNAVAILABLE,
            code: "temporarily-unavailable",
        }
    }
    fn unsupported() -> Self {
        Self {
            status: StatusCode::UPGRADE_REQUIRED,
            code: "unsupported-version",
        }
    }
}

#[derive(Debug)]
struct Parsed {
    route: Route,
    target: String,
    proof: Option<Vec<u8>>,
}

fn check_origin(parts: &Parts, origin: &Origin) -> Result<(), Rejection> {
    if parts.headers.contains_key("cookie") {
        return Err(Rejection::invalid());
    }
    check_origin_base(parts, origin)
}
fn check_origin_base(parts: &Parts, origin: &Origin) -> Result<(), Rejection> {
    let headers = &parts.headers;
    if headers.len() > 32
        || headers
            .iter()
            .map(|(name, value)| name.as_str().len() + value.len() + 4)
            .sum::<usize>()
            > HEADER_BYTES
    {
        return Err(Rejection::invalid());
    }
    for name in [
        "host",
        "origin",
        "content-length",
        "content-type",
        "cookie",
        "transfer-encoding",
        PROOF_HEADER,
        "sec-fetch-site",
    ] {
        // Locked Hyper normalizes identical repeated Content-Length fields
        // before this boundary; that wire normalization is explicitly allowed.
        if headers.get_all(name).iter().count() > 1 {
            return Err(Rejection::invalid());
        }
    }
    let host = headers
        .get("host")
        .map(|v| v.to_str().map_err(|_| Rejection::invalid()))
        .transpose()?;
    if parts.version == Version::HTTP_2 || parts.version == Version::HTTP_3 {
        if parts.uri.scheme_str() != Some("https")
            || parts.uri.authority().map(|a| a.as_str()) != Some(origin.authority.as_str())
        {
            return Err(Rejection::denied());
        }
        if host.is_some_and(|host| host != origin.authority) {
            return Err(Rejection::denied());
        }
    } else if parts.uri.scheme().is_some()
        || parts.uri.authority().is_some()
        || host != Some(origin.authority.as_str())
    {
        return Err(Rejection::denied());
    }
    if headers
        .get("origin")
        .is_some_and(|v| v.as_bytes() != origin.exact.as_bytes())
        || headers
            .get("sec-fetch-site")
            .is_some_and(|v| !matches!(v.as_bytes(), b"same-origin" | b"none"))
    {
        return Err(Rejection::denied());
    }
    if headers.contains_key("upgrade")
        || headers.contains_key("expect")
        || headers.contains_key("trailer")
        || headers.contains_key("content-encoding")
        || headers.contains_key("authorization")
        // Hyper can discard Content-Length after Transfer-Encoding. Reject all
        // transfer codings, including bare chunked, rather than inspect lost wire evidence.
        || headers.contains_key("transfer-encoding")
    {
        return Err(Rejection::invalid());
    }
    Ok(())
}

fn parse_head(parts: &Parts, origin: &Origin) -> Result<Parsed, Rejection> {
    check_origin(parts, origin)?;
    parse_head_checked(parts)
}
fn parse_head_checked(parts: &Parts) -> Result<Parsed, Rejection> {
    let target = parts
        .uri
        .path_and_query()
        .ok_or_else(Rejection::invalid)?
        .as_str();
    http_proof::Request::new(parts.method.as_str(), target, &[])
        .map_err(|_| Rejection::invalid())?;
    let route = match (parts.method.as_str(), parts.uri.path()) {
        ("GET", "/peer/v3/descriptor") if parts.uri.query().is_none() => Route::Descriptor,
        ("POST", "/peer/v3/bootstrap") if parts.uri.query().is_none() => Route::Bootstrap,
        ("POST", "/peer/v3/challenge") if parts.uri.query().is_none() => Route::Challenge,
        ("POST", "/peer/v3/sessions") if parts.uri.query().is_none() => Route::Sessions,
        ("POST", "/peer/v3/state") if parts.uri.query().is_none() => Route::State,
        ("POST", "/peer/v3/acks") if parts.uri.query().is_none() => Route::Acks,
        ("POST", "/peer/v3/onboarding/chunks") if parts.uri.query().is_none() => {
            Route::OnboardingChunk
        }
        ("POST", "/peer/v3/snapshots") if parts.uri.query().is_none() => Route::Snapshot,
        ("POST", tilo_relay::delivery_v3::live_wire::LIVE_GRANT_TARGET)
            if parts.uri.query().is_none() =>
        {
            Route::LiveGrant
        }
        ("GET", path) if parts.uri.query().is_none() && snapshot_chunk_target(path).is_some() => {
            Route::SnapshotChunk
        }
        ("GET", "/peer/v3/private/events")
            if target == tilo_relay::delivery_v3::private_wire::PRIVATE_EVENTS_TARGET =>
        {
            Route::PrivatePoll
        }
        ("GET", "/peer/v3/events") => {
            let query = parts.uri.query().ok_or_else(Rejection::invalid)?;
            let mode = if let Some(cursor) = query.strip_prefix("cursor=") {
                let (cursor, mode) = cursor.split_once('&').ok_or_else(Rejection::invalid)?;
                if cursor.is_empty()
                    || cursor.len() > tilo_relay::delivery_v3::state::MAX_CURSOR_URL_BYTES
                    || cursor.len() % 2 != 0
                    || !cursor
                        .bytes()
                        .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
                {
                    return Err(Rejection::invalid());
                }
                mode
            } else {
                query
            };
            if mode != "mode=poll" {
                return Err(Rejection::unsupported());
            }
            Route::EventsPoll
        }
        ("DELETE", path)
            if path.strip_prefix("/peer/v3/sessions/").is_some_and(|id| {
                id.len() == 32
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            }) && parts.uri.query().is_none() =>
        {
            Route::Detach
        }
        ("POST", path) if parts.uri.query().is_none() => {
            if let Ok(operation) =
                tilo_relay::delivery_v3::onboarding_wire::OnboardingOperation::from_target(
                    "POST", target,
                )
            {
                Route::Onboarding(operation)
            } else if let Ok(operation) =
                tilo_relay::delivery_v3::private_wire::PrivateOperation::from_target("POST", target)
            {
                Route::Private(operation)
            } else {
                Route::Media(MediaRoute::from_path(path).ok_or_else(Rejection::unsupported)?)
            }
        }
        _ => return Err(Rejection::unsupported()),
    };
    let proof = parts
        .headers
        .get(PROOF_HEADER)
        .map(|value| {
            let bytes = value.as_bytes();
            if bytes.is_empty()
                || bytes.len() > 2 * http_proof::MAX_PROOF_BYTES
                || bytes.len() % 2 != 0
                || !bytes
                    .iter()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(c))
            {
                return Err(Rejection::invalid());
            }
            hex::decode(bytes).map_err(|_| Rejection::invalid())
        })
        .transpose()?;
    if matches!(
        route,
        Route::Descriptor
            | Route::Bootstrap
            | Route::Challenge
            | Route::Sessions
            | Route::Onboarding(_)
            | Route::OnboardingChunk
    ) {
        if proof.is_some() {
            return Err(Rejection::invalid());
        }
    } else if proof.is_none() {
        return Err(Rejection {
            status: StatusCode::UNAUTHORIZED,
            code: "unauthenticated",
        });
    }
    Ok(Parsed {
        route,
        target: target.into(),
        proof,
    })
}

/// Exact `/peer/v3/snapshots/{pin}/chunks/{index}`: a lowercase 16-byte pin
/// and a canonical decimal index below the protocol chunk bound.
pub(crate) fn snapshot_chunk_target(path: &str) -> Option<([u8; 16], u32)> {
    let (pin, index) = path
        .strip_prefix("/peer/v3/snapshots/")?
        .split_once("/chunks/")?;
    if pin.len() != 32
        || !pin
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        || index.is_empty()
        || index.len() > 4
        || (index.len() > 1 && index.starts_with('0'))
        || !index.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let index: u32 = index.parse().ok()?;
    if index >= tilo_relay::delivery_v3::state::MAX_SNAPSHOT_CHUNKS {
        return None;
    }
    Some((hex::decode(pin).ok()?.try_into().ok()?, index))
}

/// A bounded untrusted request. Only the real actor may authenticate its contents.
/// Dropping it rejects the request; response construction grants no durability.
pub struct BufferedRequest {
    method: Method,
    target: String,
    body: Bytes,
    proof: Option<Vec<u8>>,
    route: Route,
    deadline: Instant,
    cancelled: watch::Receiver<bool>,
    response: oneshot::Sender<Reply>,
    lease: Arc<Lease>,
}
impl BufferedRequest {
    pub fn method(&self) -> &Method {
        &self.method
    }
    pub fn target(&self) -> &str {
        &self.target
    }
    pub fn body(&self) -> &[u8] {
        &self.body
    }
    pub fn proof(&self) -> Option<&[u8]> {
        self.proof.as_deref()
    }
    pub fn route(&self) -> Route {
        self.route
    }
    pub fn deadline(&self) -> Instant {
        self.deadline
    }
    pub fn is_cancelled(&self) -> bool {
        *self.cancelled.borrow()
            || self.cancelled.has_changed().is_err()
            || Instant::now() >= self.deadline
    }
    pub fn cancellation(&self) -> watch::Receiver<bool> {
        self.cancelled.clone()
    }
    pub fn respond_with_credits(
        self,
        status: StatusCode,
        body: Vec<u8>,
        credits: ResponseCredits,
    ) -> Result<(), &'static str> {
        let response_limit = if self.route == Route::Descriptor {
            tilo_relay::delivery_v3::descriptor::MAX_HINT_BYTES
        } else if self.route == Route::Bootstrap {
            32 * 1024
        } else if matches!(self.route, Route::Media(_)) {
            AUTH_BYTES
        } else {
            BODY_BYTES
        };
        if body.capacity() > response_limit || status.is_redirection() || status.is_informational()
        {
            drop(body);
            return Err("invalid bounded response");
        }
        if self.is_cancelled() {
            drop(body);
            return Err("request cancelled");
        }
        self.response
            .send(Reply {
                status,
                body: leased_bytes_with_credits(body, self.lease.clone(), credits),
                lease: self.lease,
            })
            .map_err(|_| "request cancelled")
    }
    /// Consume an exclusively owned bounded allocation. Capacity, not a view's
    /// length, is charged; an opaque Bytes slice could retain a much larger owner.
    pub fn respond(self, status: StatusCode, body: Vec<u8>) -> Result<(), &'static str> {
        self.respond_with_credits(status, body, ResponseCredits::default())
    }
}
/// Fixed response custody; this conveys no authorization or owner qualification.
/// Actual bytes remain charged by the request's shared HostBytes reservation.
/// Retain original broker/read custody and current registry flow slots through
/// the last outgoing backing clone. There is no independent capacity pool.
#[derive(Default)]
pub struct ResponseCredits {
    pub private_reply: Option<crate::private_http::PendingPrivate>,
    pub agent_read: Option<qos_action_broker::agent_reads::AgentReadCustody>,
    pub private_slot: Option<OwnedSemaphorePermit>,
    pub download_window: Option<OwnedSemaphorePermit>,
    pub upload_slot: Option<OwnedSemaphorePermit>,
}
struct Reply {
    status: StatusCode,
    body: Bytes,
    lease: Arc<Lease>,
}
pub(crate) struct Lease {
    _bytes: ByteCredit,
    _request: OwnedSemaphorePermit,
}

/// Shares host-level memory and concurrency bounds with the listener.
#[derive(Clone)]
pub struct Ingress {
    control: mpsc::Sender<BufferedRequest>,
    media: mpsc::Sender<BufferedRequest>,
    data: mpsc::Sender<BufferedRequest>,
    bytes: HostBytes,
    work: crate::store::HostWorkInventory,
    data_requests: Arc<Semaphore>,
    control_requests: Arc<Semaphore>,
    media_requests: Arc<Semaphore>,
    asset_requests: Arc<Semaphore>,
    /// LIVE_V1 relay once installed; serves the HTTPS fallback carrier.
    #[cfg(feature = "media")]
    pub(crate) live: Arc<std::sync::OnceLock<qos_moq::live::LiveRelay>>,
}
#[derive(Clone, Copy)]
enum Lane {
    Media,
    Data,
    Control,
    Asset,
}
pub struct IngressReceiver {
    control: mpsc::Receiver<BufferedRequest>,
    media: mpsc::Receiver<BufferedRequest>,
    data: mpsc::Receiver<BufferedRequest>,
    bytes: HostBytes,
    work: crate::store::HostWorkInventory,
    priority_batches: u8,
    #[cfg(test)]
    initial_bytes: usize,
}
/// Control has separate queue capacity and is selected before bulk work.
pub fn try_ingress_channel() -> anyhow::Result<(Ingress, IngressReceiver)> {
    let (control, control_rx) = mpsc::channel(8);
    let (media, media_rx) = mpsc::channel(32);
    let (data, data_rx) = mpsc::channel(32);
    let bytes = HostBytes::from_ingress(Arc::new(Semaphore::new(GLOBAL_BYTES)));
    let work = crate::store::HostWorkInventory::new(&bytes)?;
    #[cfg(test)]
    let initial_bytes = bytes.available_bytes();
    Ok((
        Ingress {
            control,
            media,
            data,
            bytes: bytes.clone(),
            work: work.clone(),
            data_requests: Arc::new(Semaphore::new(32)),
            control_requests: Arc::new(Semaphore::new(8)),
            media_requests: Arc::new(Semaphore::new(32)),
            asset_requests: Arc::new(Semaphore::new(24)),
            #[cfg(feature = "media")]
            live: Arc::default(),
        },
        IngressReceiver {
            control: control_rx,
            media: media_rx,
            data: data_rx,
            bytes,
            work,
            priority_batches: 0,
            #[cfg(test)]
            initial_bytes,
        },
    ))
}
#[cfg(test)]
pub(crate) fn ingress_channel() -> (Ingress, IngressReceiver) {
    try_ingress_channel().expect("test ingress inventory allocation")
}
impl IngressReceiver {
    #[cfg(test)]
    pub(crate) fn idle_available_bytes(&self) -> usize {
        self.initial_bytes
    }
    pub(crate) fn host_bytes(&self) -> HostBytes {
        self.bytes.clone()
    }
    pub(crate) fn host_work(&self) -> crate::store::HostWorkInventory {
        self.work.clone()
    }
    fn record(&mut self, request: Option<BufferedRequest>) -> Option<BufferedRequest> {
        if let Some(request) = &request {
            if request.route().control() || matches!(request.route(), Route::Media(_)) {
                self.priority_batches = self.priority_batches.saturating_add(1).min(8);
            } else {
                self.priority_batches = 0;
            }
        }
        request
    }
    pub async fn recv(&mut self) -> Option<BufferedRequest> {
        let request = if self.priority_batches >= 8 {
            tokio::select! {
                biased;
                request = self.data.recv(), if !self.data.is_closed() || !self.data.is_empty() => request,
                request = self.control.recv(), if !self.control.is_closed() || !self.control.is_empty() => request,
                request = self.media.recv(), if !self.media.is_closed() || !self.media.is_empty() => request,
                else => None,
            }
        } else {
            tokio::select! {
                biased;
                request = self.control.recv(), if !self.control.is_closed() || !self.control.is_empty() => request,
                request = self.media.recv(), if !self.media.is_closed() || !self.media.is_empty() => request,
                request = self.data.recv(), if !self.data.is_closed() || !self.data.is_empty() => request,
                else => None,
            }
        };
        self.record(request)
    }
    pub fn try_recv(&mut self) -> Option<BufferedRequest> {
        if self.priority_batches >= 8 {
            if let Ok(request) = self.data.try_recv() {
                return self.record(Some(request));
            }
        }
        let request = self
            .control
            .try_recv()
            .ok()
            .or_else(|| self.media.try_recv().ok())
            .or_else(|| self.data.try_recv().ok());
        self.record(request)
    }
    /// Accounting only, not service health or readiness.
    pub fn available_bytes(&self) -> usize {
        self.bytes.available_bytes()
    }
}
impl Ingress {
    pub(crate) fn host_work(&self) -> crate::store::HostWorkInventory {
        self.work.clone()
    }
    pub(crate) fn host_bytes(&self) -> HostBytes {
        self.bytes.clone()
    }
    #[cfg(feature = "media")]
    pub(crate) fn reserve_media_transport(&self) -> Option<ByteCredit> {
        self.bytes.try_acquire_bulk(CONNECTION_BYTES)
    }
    fn reserve(&self, body_limit: usize, lane: Lane) -> Result<Arc<Lease>, Rejection> {
        // Covers input, an actor decode copy, bounded response and proof/header metadata.
        let bytes = if matches!(lane, Lane::Asset) {
            // Conservatively covers one worker's bounded read allocation and
            // pending owned output chunks, including detached filesystem work.
            4 * IO_BYTES + HEADER_BYTES
        } else if matches!(lane, Lane::Media) {
            // Media has bounded 8KiB input/decode/reply, not a 1MiB data reply.
            3 * AUTH_BYTES + HEADER_BYTES
        } else {
            2 * body_limit + BODY_BYTES + HEADER_BYTES
        };
        let requests = match lane {
            Lane::Media => &self.media_requests,
            Lane::Data => &self.data_requests,
            Lane::Control => &self.control_requests,
            Lane::Asset => &self.asset_requests,
        };
        let request = requests
            .clone()
            .try_acquire_owned()
            .map_err(|_| Rejection::unavailable())?;
        let permit = if matches!(lane, Lane::Control) {
            self.bytes.try_acquire_control(bytes)
        } else {
            self.bytes.try_acquire_bulk(bytes)
        }
        .ok_or_else(Rejection::unavailable)?;
        Ok(Arc::new(Lease {
            _bytes: permit,
            _request: request,
        }))
    }
}

/// Listener capability is deliberately separate from descriptor/product readiness.
/// No host loop calls this module until the real authorization adapter is installed.
pub struct HttpsAdapter {
    listener: TcpListener,
    acceptor: tokio_rustls::TlsAcceptor,
    origin: Arc<Origin>,
    assets: AssetRoot,
    ingress: Ingress,
    local_enrollment: Option<Arc<LocalEnrollment>>,
}
impl HttpsAdapter {
    pub fn new(
        listener: TcpListener,
        origin: &str,
        assets: AssetRoot,
        certificates: Vec<CertificateDer<'static>>,
        key: PrivateKeyDer<'static>,
        ingress: Ingress,
    ) -> io::Result<Self> {
        let origin = Origin::parse(origin)?;
        let address = listener.local_addr()?;
        if !address.ip().is_loopback()
            || address.port() != origin.port
            || certificates.is_empty()
            || certificates.iter().map(|cert| cert.len()).sum::<usize>() > 64 * 1024
            || key.secret_der().len() > 64 * 1024
        {
            return Err(io::Error::other("invalid local TLS listener configuration"));
        }
        let mut config = rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .map_err(io::Error::other)?;
        config.alpn_protocols = vec![b"h2".to_vec(), b"http/1.1".to_vec()];
        config.max_early_data_size = 0;
        Ok(Self {
            listener,
            acceptor: tokio_rustls::TlsAcceptor::from(Arc::new(config)),
            origin: Arc::new(origin),
            assets,
            ingress,
            local_enrollment: None,
        })
    }
    /// Advertise HTTP/3 on this port (`Alt-Svc`), after the caller installed
    /// the HTTP/3 service on the same port's QUIC endpoint.
    #[cfg(feature = "media")]
    pub(crate) fn with_alt_svc(mut self) -> Self {
        let value = format!("h3=\":{}\"; ma=3600", self.origin.port);
        if let Ok(value) = hyper::header::HeaderValue::from_str(&value) {
            self.origin = Arc::new(Origin {
                exact: self.origin.exact.clone(),
                authority: self.origin.authority.clone(),
                port: self.origin.port,
                alt_svc: Some(value),
            });
        }
        self
    }
    /// The same route handler for HTTP/3 request streams.
    #[cfg(feature = "media")]
    pub(crate) fn h3_service(&self) -> h3::H3Service {
        h3::H3Service {
            origin: self.origin.clone(),
            assets: self.assets.clone(),
            ingress: self.ingress.clone(),
            local_enrollment: self.local_enrollment.clone(),
        }
    }
    pub(crate) fn with_local_enrollment(
        mut self,
        gateway: Arc<LocalEnrollment>,
    ) -> io::Result<Self> {
        if gateway.origin() != self.origin.exact {
            return Err(io::Error::other("local enrollment origin mismatch"));
        }
        self.local_enrollment = Some(gateway);
        Ok(self)
    }

    pub async fn serve(self, mut shutdown: watch::Receiver<bool>) -> io::Result<()> {
        let connections = Arc::new(Semaphore::new(64));
        let handshakes = Arc::new(Semaphore::new(16));
        let mut tasks = JoinSet::new();
        loop {
            if *shutdown.borrow() {
                break;
            }
            tokio::select! {
                biased;
                _ = shutdown.changed() => break,
                _ = tasks.join_next(), if !tasks.is_empty() => {},
                accepted = self.listener.accept() => {
                    let (socket, _) = accepted?;
                    let Ok(connection) = connections.clone().try_acquire_owned() else { tracing::debug!("https connection dropped: connection limit"); continue };
                    let Ok(handshake) = handshakes.clone().try_acquire_owned() else { tracing::debug!("https connection dropped: handshake limit"); continue };
                    let Some(connection_bytes) = self.ingress.bytes.try_acquire_bulk(CONNECTION_BYTES) else { tracing::debug!(available = self.ingress.bytes.available_bytes(), "https connection dropped: ingress bytes"); continue };
                    let acceptor = self.acceptor.clone();
                    let origin = self.origin.clone();
                    let assets = self.assets.clone();
                    let ingress = self.ingress.clone();
                    let local_enrollment = self.local_enrollment.clone();
                    tasks.spawn(async move {
                        let (_connection, _connection_bytes) = (connection, connection_bytes);
                        let Ok(Ok(tls)) = timeout(DEADLINE, acceptor.accept(socket)).await else { tracing::debug!("https connection dropped: TLS handshake"); return };
                        drop(handshake);
                        let mut builder = auto::Builder::new(TokioExecutor::new());
                        builder.http1().max_headers(32).max_buf_size(HEADER_BYTES).timer(TokioTimer::new()).header_read_timeout(DEADLINE);
                        builder.http2().max_concurrent_streams(16).max_header_list_size(HEADER_BYTES as u32)
                            .initial_stream_window_size(IO_BYTES as u32).initial_connection_window_size((256 * 1024) as u32)
                            .max_frame_size(16384).max_send_buf_size(IO_BYTES)
                            .max_pending_accept_reset_streams(16).max_local_error_reset_streams(16);
                        let (first_header, mut first_received) = watch::channel(false);
                        let service = hyper::service::service_fn(move |request| {
                            let _ = first_header.send(true);
                            let origin = origin.clone();
                            let response = handle(request, origin.clone(), assets.clone(), ingress.clone(), local_enrollment.clone());
                            async move {
                                let mut response = response.await?;
                                if let Some(value) = &origin.alt_svc {
                                    response.headers_mut().insert("alt-svc", value.clone());
                                }
                                Ok::<_, Infallible>(response)
                            }
                        });
                        // Protocol errors are closed locally; diagnostics never include request bytes.
                        // Hyper exposes no subsequent H2 header-block timer. Locked h2
                        // bounds continuation/header resources; the entire connection
                        // has an absolute lifetime, including active incomplete headers.
                        let connection = builder.serve_connection(TokioIo::new(tls), service);
                        tokio::pin!(connection);
                        let expires = Instant::now() + CONNECTION_LIFETIME;
                        tokio::select! {
                            result = &mut connection => { if let Err(error) = result { tracing::debug!(%error, "https connection ended with error"); } },
                            initial = timeout(DEADLINE, first_received.changed()) => {
                                if initial.is_ok() && *first_received.borrow() {
                                    match timeout_at(expires, connection.as_mut()).await {
                                        Err(_) => {
                                            // At the absolute lifetime stop accepting new requests
                                            // (HTTP/2 GOAWAY, HTTP/1 close after the response) and
                                            // let in-flight transfers finish within a bounded grace,
                                            // instead of cutting a slow download mid-body.
                                            connection.as_mut().graceful_shutdown();
                                            match timeout(CONNECTION_GRACE, connection.as_mut()).await {
                                                Err(_) => tracing::debug!("https connection closed after its lifetime grace"),
                                                Ok(Err(error)) => tracing::debug!(%error, "https connection ended with error"),
                                                Ok(Ok(())) => tracing::debug!("https connection drained at its absolute lifetime"),
                                            }
                                        }
                                        Ok(Err(error)) => tracing::debug!(%error, "https connection ended with error"),
                                        Ok(Ok(())) => {}
                                    }
                                } else {
                                    tracing::debug!("https connection closed: no request header before deadline");
                                }
                            }
                        }
                    });
                }
            }
        }
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        Ok(())
    }
}

struct CancelOnDrop(watch::Sender<bool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        let _ = self.0.send(true);
    }
}

async fn handle<B>(
    request: Request<B>,
    origin: Arc<Origin>,
    assets: AssetRoot,
    ingress: Ingress,
    local_enrollment: Option<Arc<LocalEnrollment>>,
) -> Result<Response<ResponseBody>, Infallible>
where
    B: Body<Data = Bytes> + Unpin,
{
    let response = match handle_inner(
        request,
        &origin,
        &assets,
        &ingress,
        local_enrollment.as_deref(),
    )
    .await
    {
        Ok(response) => response,
        Err(rejection) => {
            tracing::debug!(status = %rejection.status, code = rejection.code, "https request rejected");
            reject(rejection)
        }
    };
    Ok(response)
}

async fn handle_inner<B>(
    request: Request<B>,
    origin: &Origin,
    assets: &AssetRoot,
    ingress: &Ingress,
    local_enrollment: Option<&LocalEnrollment>,
) -> Result<Response<ResponseBody>, Rejection>
where
    B: Body<Data = Bytes> + Unpin,
{
    let (parts, mut body) = request.into_parts();
    check_origin_base(&parts, origin)?;
    if parts.headers.contains_key("cookie") {
        local_enrollment
            .ok_or_else(Rejection::invalid)?
            .cookie(&parts)?;
    }
    let path = parts.uri.path();
    if LocalEnrollment::recognizes(path) {
        return local_enrollment
            .ok_or_else(Rejection::denied)?
            .serve(parts, body, ingress)
            .await;
    }
    #[cfg(feature = "media")]
    if live_fallback::recognizes(path) {
        return live_fallback::serve(parts, body, ingress).await;
    }
    if !path.starts_with("/peer/")
        && path != "/peer"
        && !path.starts_with("/qos-peer/")
        && path != "/qos-peer"
    {
        if parts.method != Method::GET && parts.method != Method::HEAD
            || parts.headers.contains_key(PROOF_HEADER)
        {
            return Err(Rejection::invalid());
        }
        if parts
            .headers
            .get("content-length")
            .is_some_and(|v| v != "0")
            || parts.headers.contains_key("transfer-encoding")
            || !body.is_end_stream()
        {
            return Err(Rejection::invalid());
        }
        let lease = ingress.reserve(0, Lane::Asset)?;
        let navigation = parts
            .headers
            .get("accept")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(',')
                    .any(|v| v.trim().split(';').next() == Some("text/html"))
            });
        let asset = assets
            .resolve(path, navigation, lease.clone())
            .await
            .map_err(|_| Rejection {
                status: StatusCode::NOT_FOUND,
                code: "not-found",
            })?;
        if crate::action_pump_cadence::latency_baseline() {
            let length = asset.length;
            let body = if parts.method == Method::HEAD {
                ResponseBody::full(Bytes::new(), Some(lease))
            } else {
                ResponseBody::file(asset.file, length, lease)
            };
            let mut response = response(StatusCode::OK, body, asset.content_type);
            response
                .headers_mut()
                .insert("content-length", length.into());
            return Ok(response);
        }
        return Ok(asset_response(&parts, asset, lease));
    }
    let parsed = parse_head_checked(&parts)?;
    let limit = parsed.route.body_limit();
    if parts.headers.get("content-length").is_some_and(|v| {
        v.to_str()
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .is_none_or(|size| size > limit)
    }) {
        return Err(Rejection::invalid());
    }
    let lease = ingress.reserve(
        limit,
        if matches!(parsed.route, Route::Media(_)) {
            Lane::Media
        } else if parsed.route.control() {
            Lane::Control
        } else {
            Lane::Data
        },
    )?;
    let mut bytes = BytesMut::with_capacity(limit);
    timeout(DEADLINE, async {
        while let Some(frame) = body.frame().await {
            let frame = frame.map_err(|_| Rejection::invalid())?;
            let chunk = frame.into_data().map_err(|_| Rejection::invalid())?;
            if chunk.len() > limit - bytes.len() {
                return Err(Rejection::invalid());
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok::<_, Rejection>(())
    })
    .await
    .map_err(|_| Rejection::invalid())??;
    let deadline = Instant::now()
        + if matches!(parsed.route, Route::EventsPoll | Route::PrivatePoll) {
            POLL_DEADLINE
        } else {
            DEADLINE
        };
    let (cancel, cancelled) = watch::channel(false);
    let _cancel = CancelOnDrop(cancel);
    let (reply, response_rx) = oneshot::channel();
    let request = BufferedRequest {
        method: parts.method,
        target: parsed.target,
        body: bytes.freeze(),
        proof: parsed.proof,
        route: parsed.route,
        deadline,
        cancelled,
        response: reply,
        lease,
    };
    let queue = if matches!(parsed.route, Route::Media(_)) {
        &ingress.media
    } else if parsed.route.control() {
        &ingress.control
    } else {
        &ingress.data
    };
    queue
        .try_send(request)
        .map_err(|_| Rejection::unavailable())?;
    let reply = timeout_at(deadline, response_rx)
        .await
        .map_err(|_| Rejection::unavailable())?
        .map_err(|_| Rejection::unavailable())?;
    Ok(response(
        reply.status,
        ResponseBody::full(reply.body, Some(reply.lease)),
        "application/octet-stream",
    ))
}

/// Immutable assets: revalidated (`no-cache`) rather than never stored, with a
/// strong content validator and a precompressed brotli variant when available.
/// Revalidation keeps every load consistent with the served version while a
/// reload answers 304 without re-sending the runtime.
fn asset_response(
    parts: &Parts,
    asset: crate::https_assets::Asset,
    lease: Arc<Lease>,
) -> Response<ResponseBody> {
    let brotli = asset
        .encoded
        .as_ref()
        .and_then(|encoded| encoded.brotli.clone())
        .filter(|_| accepts_brotli(parts));
    let tag = asset.encoded.as_ref().map(|encoded| {
        if brotli.is_some() {
            format!("\"{}-br\"", encoded.tag)
        } else {
            format!("\"{}\"", encoded.tag)
        }
    });
    let not_modified = tag.as_ref().is_some_and(|tag| {
        parts
            .headers
            .get_all("if-none-match")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .any(|candidate| candidate.trim().trim_start_matches("W/") == tag.as_str())
    });
    let (status, body, length) = if not_modified {
        (
            StatusCode::NOT_MODIFIED,
            ResponseBody::full(Bytes::new(), Some(lease)),
            None,
        )
    } else if let Some(bytes) = brotli.clone() {
        let length = bytes.len() as u64;
        let body = if parts.method == Method::HEAD {
            Bytes::new()
        } else {
            bytes
        };
        (
            StatusCode::OK,
            ResponseBody::full(body, Some(lease)),
            Some(length),
        )
    } else if parts.method == Method::HEAD {
        (
            StatusCode::OK,
            ResponseBody::full(Bytes::new(), Some(lease)),
            Some(asset.length),
        )
    } else {
        (
            StatusCode::OK,
            ResponseBody::file(asset.file, asset.length, lease),
            Some(asset.length),
        )
    };
    let mut response = response(status, body, asset.content_type);
    let headers = response.headers_mut();
    headers.insert(
        "cache-control",
        if asset.immutable {
            "public, max-age=31536000, immutable"
        } else {
            "no-cache"
        }
        .parse()
        .expect("constant header"),
    );
    headers.insert("vary", "accept-encoding".parse().expect("constant header"));
    if let Some(tag) = tag.and_then(|tag| tag.parse().ok()) {
        headers.insert("etag", tag);
    }
    if brotli.is_some() && !not_modified {
        headers.insert("content-encoding", "br".parse().expect("constant header"));
    }
    if let Some(length) = length {
        headers.insert("content-length", length.into());
    }
    response
}

#[cfg(test)]
pub(crate) fn asset_response_head(
    parts: &Parts,
    asset: crate::https_assets::Asset,
    lease: Arc<Lease>,
) -> hyper::http::response::Parts {
    asset_response(parts, asset, lease).into_parts().0
}

fn accepts_brotli(parts: &Parts) -> bool {
    parts
        .headers
        .get_all("accept-encoding")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .any(|coding| {
            let mut fields = coding.split(';').map(str::trim);
            fields.next() == Some("br")
                && fields.all(|field| {
                    field
                        .strip_prefix("q=")
                        .is_none_or(|q| q.parse::<f32>().is_ok_and(|q| q > 0.0))
                })
        })
}

fn response(
    status: StatusCode,
    body: ResponseBody,
    content_type: &'static str,
) -> Response<ResponseBody> {
    let mut response = Response::new(body);
    *response.status_mut() = status;
    response.headers_mut().insert(
        "content-type",
        content_type.parse().expect("constant header"),
    );
    response.headers_mut().insert(
        "cache-control",
        "no-store".parse().expect("constant header"),
    );
    response.headers_mut().insert(
        "x-content-type-options",
        "nosniff".parse().expect("constant header"),
    );
    response
}
fn reject(rejection: Rejection) -> Response<ResponseBody> {
    let mut response = response(
        rejection.status,
        ResponseBody::full(Bytes::from_static(rejection.code.as_bytes()), None),
        "text/plain; charset=utf-8",
    );
    // Closing HTTP/1 prevents unread rejected bodies from becoming another request.
    response
        .headers_mut()
        .insert("connection", "close".parse().expect("constant header"));
    if rejection.status == StatusCode::SERVICE_UNAVAILABLE {
        response
            .headers_mut()
            .insert("retry-after", "1".parse().expect("constant header"));
    }
    response
}

struct ResponseBody {
    data: Data,
    lease: Option<Arc<Lease>>,
}

// Bytes::from_owner retains this owner across every clone/slice in Hyper/h2.
// The frame backing, not the shorter-lived Body future, owns its reservation.
struct LeasedBuffer {
    bytes: Vec<u8>,
    // Owned broker evidence must drop before the request memory lease.
    _credits: ResponseCredits,
    _lease: Arc<Lease>,
}
impl AsRef<[u8]> for LeasedBuffer {
    fn as_ref(&self) -> &[u8] {
        &self.bytes
    }
}
fn leased_bytes(bytes: Vec<u8>, lease: Arc<Lease>) -> Bytes {
    leased_bytes_with_credits(bytes, lease, ResponseCredits::default())
}
fn leased_bytes_with_credits(bytes: Vec<u8>, lease: Arc<Lease>, credits: ResponseCredits) -> Bytes {
    Bytes::from_owner(LeasedBuffer {
        bytes,
        _lease: lease,
        _credits: credits,
    })
}
enum Data {
    Full(Option<Bytes>),
    File {
        file: Option<AssetFile>,
        pending: Option<AssetRead>,
        remaining: u64,
    },
    /// LIVE_V1 fallback server frames, streamed until the session or stream ends.
    #[cfg(feature = "media")]
    Live {
        stream: qos_moq::live::fallback::FallbackStream,
        _credit: crate::host_bytes::ByteCredit,
    },
}
impl ResponseBody {
    fn file(file: AssetFile, remaining: u64, lease: Arc<Lease>) -> Self {
        Self {
            data: Data::File {
                file: Some(file),
                pending: None,
                remaining,
            },
            lease: Some(lease),
        }
    }
    fn full(bytes: Bytes, lease: Option<Arc<Lease>>) -> Self {
        Self {
            data: Data::Full(Some(bytes)),
            lease,
        }
    }
}
impl Body for ResponseBody {
    type Data = Bytes;
    type Error = io::Error;
    fn poll_frame(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, io::Error>>> {
        let this = self.get_mut();
        match &mut this.data {
            Data::Full(bytes) => Poll::Ready(bytes.take().map(|bytes| Ok(Frame::data(bytes)))),
            #[cfg(feature = "media")]
            Data::Live { stream, .. } => stream
                .poll_next(cx)
                .map(|chunk| chunk.map(|bytes| Ok(Frame::data(bytes)))),
            Data::File {
                file,
                pending,
                remaining,
            } => {
                if *remaining == 0 {
                    this.lease.take();
                    return Poll::Ready(None);
                }
                if pending.is_none() {
                    *pending = Some(
                        file.take()
                            .expect("one asset read at a time")
                            .read_chunk((*remaining).min(IO_BYTES as u64) as usize),
                    );
                }
                let completed = match Pin::new(pending.as_mut().expect("pending read")).poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(result) => {
                        result.map_err(io::Error::other).and_then(|result| result)
                    }
                };
                pending.take();
                match completed {
                    Err(error) => {
                        tracing::debug!(%error, "asset response body failed");
                        *remaining = 0;
                        Poll::Ready(Some(Err(error)))
                    }
                    Ok((bytes, returned_file)) => {
                        *file = Some(returned_file);
                        if bytes.is_empty() {
                            *remaining = 0;
                            return Poll::Ready(Some(Err(io::Error::new(
                                io::ErrorKind::UnexpectedEof,
                                "asset changed during response",
                            ))));
                        }
                        *remaining -= bytes.len() as u64;
                        Poll::Ready(Some(Ok(Frame::data(leased_bytes(
                            bytes,
                            this.lease.as_ref().expect("file body owns a lease").clone(),
                        )))))
                    }
                }
            }
        }
    }
    fn is_end_stream(&self) -> bool {
        match &self.data {
            Data::Full(bytes) => bytes.as_ref().is_none_or(Bytes::is_empty),
            Data::File { remaining, .. } => *remaining == 0,
            #[cfg(feature = "media")]
            Data::Live { .. } => false,
        }
    }
    fn size_hint(&self) -> SizeHint {
        SizeHint::with_exact(match &self.data {
            Data::Full(bytes) => bytes.as_ref().map_or(0, |bytes| bytes.len() as u64),
            Data::File { remaining, .. } => *remaining,
            #[cfg(feature = "media")]
            Data::Live { .. } => return SizeHint::new(),
        })
    }
}

#[cfg(test)]
pub(crate) mod tests;

mod local_enrollment;
pub(crate) use local_enrollment::LocalEnrollment;

/// LIVE_V1 HTTPS fallback carrier routes.
#[cfg(feature = "media")]
mod live_fallback;

/// Plain HTTP/3 request streams on the shared QUIC endpoint.
#[cfg(feature = "media")]
pub(crate) mod h3;

#[cfg(test)]
mod data_routes_tests;
#[cfg(test)]
pub(crate) mod onboarding_tests;
