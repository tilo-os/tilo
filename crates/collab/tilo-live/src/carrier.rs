//! The Tilo `MoQ` carrier: `moq-lite` over WebTransport, opened by a host
//! admission preface.
//!
//! The carrier is generic over its transport ([`Carrier`], `moq-lite`'s poll
//! session interface). WebTransport ([`WebTransport`]) is the browser's;
//! `native` adds one over a direct iroh connection. The admission preface, the
//! `moq-lite` session and its SETUP are the same code on both.
//!
//! # Session layout
//!
//! 1. The client sends an HTTP/3 extended CONNECT for an exact route (for
//!    example [`VOICE_PATH`](crate::relay::VOICE_PATH)) offering the
//!    WebTransport subprotocol [`PROTOCOL`]. The URL carries routing only.
//! 2. The host answers `200` with [`PROTOCOL`]. The session is now open but
//!    not admitted: the host has attached nothing to it.
//! 3. The client runs the admission preface below on the first bidirectional
//!    stream it opens, and opens no other stream until it reads `ADMITTED`.
//! 4. After `ADMITTED` both sides run `moq-lite` ([`PROTOCOL`]) on the same
//!    session. The admission stream stays open as the host's control channel.
//!
//! # Admission preface (version 1)
//!
//! Client to host, on the admission stream, exactly once:
//!
//! | Bytes | Field | Value |
//! | --- | --- | --- |
//! | 1 | version | [`PREFACE_VERSION`] |
//! | 2 | length | big-endian `u16`, `1..=`[`MAX_GRANT_BYTES`] |
//! | length | grant | opaque grant bytes issued by the host |
//!
//! The host reads the three header bytes and checks version and length before
//! it allocates anything for the grant, then reads exactly `length` bytes. The
//! whole exchange, from the session opening to the last grant byte, is bounded
//! by [`PREFACE_BUDGET`]. The host never reads further bytes from the client on
//! this stream.
//!
//! Host to client, on the same stream:
//!
//! | Byte | Meaning |
//! | --- | --- |
//! | [`STATUS_ADMITTED`] | Admitted. Origins are attached; start `moq-lite` now. |
//! | [`STATUS_DENIED`] | Refused. The host closes the session with [`CLOSE_DENIED`]. |
//! | [`STATUS_REVOKED`] | Sent after `ADMITTED` when authority lapses. The client closes the session; the host closes it itself after its grace period. |
//!
//! A session whose first bidirectional stream is not a well-formed preface
//! within the budget is closed with [`CLOSE_PREFACE`] and never reaches `MoQ`.
//! The end of the admission stream after `ADMITTED` (host shut down, stream
//! reset) means the same as `REVOKED`.
//!
//! # Why the grant is not in the URL
//!
//! URLs end up in proxy and server access logs, browser history and developer
//! tools, and browsers cannot attach request headers to a WebTransport
//! CONNECT. The grant therefore travels only inside the encrypted session, on
//! a stream the host reads once and never logs.

use std::sync::Arc;
use std::time::Duration;

use moq_net::transport::poll::RecvStream as _;
use moq_net::web_transport_trait::poll::SendStream as _;
use moq_net::web_transport_trait::{Error as _, Stats as _, poll as wt};
use rustls::pki_types::CertificateDer;

/// The WebTransport subprotocol and `moq-lite` draft every Tilo `MoQ` route
/// negotiates. No other version is offered or accepted.
pub use crate::preface::PROTOCOL;
pub use crate::preface::{
    CLOSE_DENIED, CLOSE_PREFACE, CLOSE_REVOKED, MAX_GRANT_BYTES, PREFACE_VERSION, STATUS_ADMITTED,
    STATUS_DENIED, STATUS_REVOKED,
};

/// Time from session open to the complete preface.
pub const PREFACE_BUDGET: Duration = Duration::from_secs(5);
pub use crate::preface::{DENIED_GRACE, DENIED_REPORT};

/// Budget for QUIC, TLS and the HTTP/3 CONNECT of one pending session. A
/// satellite path (600 ms RTT) needs about four round trips plus one loss
/// recovery.
pub const HANDSHAKE_BUDGET: Duration = Duration::from_secs(8);
/// Budget for the `moq-lite` SETUP exchange after admission.
pub const SETUP_BUDGET: Duration = Duration::from_secs(5);
/// Handshakes a standalone listener lets run at once; further connection
/// attempts are refused until one completes or times out.
pub const MAX_PENDING_HANDSHAKES: usize = 16;
/// Upper bound on concurrent QUIC streams per direction on a standalone
/// listener.
pub const MAX_STREAMS: u32 = 32;

/// The `moq-lite` version for [`PROTOCOL`].
pub fn version() -> Option<moq_net::Version> {
    moq_net::Version::from_alpn(PROTOCOL)
}

/// A carrier transport: [`moq_net`]'s poll session interface, clonable (each
/// clone makes its own progress), thread-safe, and `'static` so its driver
/// runs on a task of its own. WebTransport and iroh both qualify; nothing in
/// this module names either.
pub trait Carrier: moq_net::transport::poll::Session + Send + Sync + 'static {}

impl<T> Carrier for T where T: moq_net::transport::poll::Session + Send + Sync + 'static {}

/// Where one carrier's wire counters come from: sampled on demand, from
/// anywhere (the session's own task, the relay's downlink).
pub type Wire = Arc<dyn Fn() -> WireStats + Send + Sync>;

/// The counters of `session`, sampled through [`wt::Stats`].
pub fn wire<S: Carrier>(session: &S) -> Wire {
    let session = session.clone();
    Arc::new(move || {
        let stats = session.stats();
        WireStats {
            tx_bytes: stats.bytes_sent().unwrap_or(0),
            tx_datagrams: stats.packets_sent().unwrap_or(0),
            rx_bytes: stats.bytes_received().unwrap_or(0),
            rx_datagrams: stats.packets_received().unwrap_or(0),
            sent_packets: stats.packets_sent().unwrap_or(0),
            lost_packets: stats.packets_lost().unwrap_or(0),
        }
    })
}

/// Close `session` at once (idempotent).
pub(crate) fn close<S: Carrier>(session: &S, code: u32, reason: &str) {
    let mut session = session.clone();
    wt::Session::close(&mut session, code, reason);
}

/// Write every byte of `buf`; `false` if the stream failed or stopped taking
/// bytes.
async fn write_all<S>(stream: &mut S, mut buf: &[u8]) -> bool
where
    S: moq_net::transport::poll::SendStream,
{
    while !buf.is_empty() {
        match stream.write(buf).await {
            Ok(written) if written > 0 => {
                let Some(rest) = buf.get(written..) else {
                    return false;
                };
                buf = rest;
            }
            _ => return false,
        }
    }
    true
}

/// Read exactly `dst.len()` bytes; `false` at the end of the stream or on an
/// error.
async fn read_exact<S>(stream: &mut S, dst: &mut [u8]) -> bool
where
    S: moq_net::transport::poll::RecvStream,
{
    let mut filled = 0;
    while filled < dst.len() {
        let Some(rest) = dst.get_mut(filled..).filter(|rest| !rest.is_empty()) else {
            return false;
        };
        match stream.read(rest).await {
            Ok(Some(read)) if read > 0 => filled = filled.saturating_add(read),
            _ => return false,
        }
    }
    true
}

/// Wait until `session` closes, yielding the transport's error.
async fn closed<S: Carrier>(session: &S) -> S::Error {
    let mut session = session.clone();
    std::future::poll_fn(|cx| session.poll_closed(cx)).await
}

/// Why a preface was not accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum PrefaceError {
    #[error("admission preface timed out")]
    Timeout,
    #[error("malformed admission preface")]
    Malformed,
    #[error("session closed before the admission preface")]
    Closed,
}

/// Host side of an admission stream, held for the session's lifetime.
pub struct Admission<S: Carrier> {
    send: S::SendStream,
    _recv: S::RecvStream,
}

/// Read the admission preface from a freshly opened session.
///
/// Only the first bidirectional stream is accepted. Version and length are
/// checked before the grant buffer is allocated, and the whole read is bounded
/// by [`PREFACE_BUDGET`].
///
/// # Errors
///
/// Returns [`PrefaceError`] when the preface is late, malformed or the session
/// closed. The caller must then close the session with [`refuse`].
pub async fn read_preface<S: Carrier>(
    session: &S,
) -> Result<(Vec<u8>, Admission<S>), PrefaceError> {
    let mut session = session.clone();
    tokio::time::timeout(PREFACE_BUDGET, async {
        let (send, mut recv) = session
            .accept_bi()
            .await
            .map_err(|_| PrefaceError::Closed)?;
        let mut header = [0u8; 3];
        if !read_exact(&mut recv, &mut header).await {
            return Err(PrefaceError::Malformed);
        }
        let length = crate::preface::grant_length(header).ok_or(PrefaceError::Malformed)?;
        let mut grant = vec![0u8; length];
        if !read_exact(&mut recv, &mut grant).await {
            return Err(PrefaceError::Malformed);
        }
        Ok((grant, Admission { send, _recv: recv }))
    })
    .await
    .map_err(|_| PrefaceError::Timeout)?
}

/// Close a session whose preface was missing, malformed or late, telling the
/// client which in the close reason.
pub fn refuse<S: Carrier>(session: &S, error: PrefaceError) {
    close(session, CLOSE_PREFACE, &error.to_string());
}

impl<S: Carrier> Admission<S> {
    /// Tell the client it is admitted. Call only after origins are attached.
    pub async fn admit(&mut self) -> Result<(), PrefaceError> {
        write_all(&mut self.send, &[STATUS_ADMITTED])
            .await
            .then_some(())
            .ok_or(PrefaceError::Closed)
    }

    /// Refuse the grant and close the session.
    pub async fn deny(mut self, session: &S) {
        let _ = tokio::time::timeout(Duration::from_millis(100), async {
            let _ = write_all(&mut self.send, &[STATUS_DENIED]).await;
            let _ = self.send.finish();
        })
        .await;
        // Some browsers (Firefox) drop a stream's unread bytes when the
        // session closes under it, and report no close code either: the
        // client closes the session once it read the refusal, or the host
        // after a moment.
        let _ = tokio::time::timeout(DENIED_GRACE, closed(session)).await;
        close(session, CLOSE_DENIED, "admission denied");
    }

    /// Tell an admitted client that its authority lapsed. Never blocks for
    /// more than a moment; the caller closes the session after its grace.
    pub async fn revoke(&mut self) {
        let _ = tokio::time::timeout(Duration::from_millis(100), async {
            let _ = write_all(&mut self.send, &[STATUS_REVOKED]).await;
            let _ = self.send.finish();
        })
        .await;
    }
}

/// Why a client could not be admitted.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum AdmissionError {
    #[error("grant is empty or exceeds the preface ceiling")]
    Invalid,
    #[error("admission denied")]
    Denied,
    #[error("carrier unavailable")]
    Unavailable,
}

/// Client side of an admitted session's control stream.
pub struct Admitted<S: Carrier> {
    recv: S::RecvStream,
    _send: S::SendStream,
}

impl<S: Carrier> Admitted<S> {
    /// Resolves when the host revokes the session or the control stream ends.
    pub async fn revoked(&mut self) {
        let mut byte = [0u8; 1];
        // Any byte after ADMITTED is a revocation; so is the stream ending.
        let _ = self.recv.read(&mut byte).await;
    }
}

/// Present `grant` on a freshly opened session and wait for the host's answer.
///
/// # Errors
///
/// Returns [`AdmissionError::Denied`] when the host refuses the grant and
/// [`AdmissionError::Unavailable`] when the session fails first.
pub async fn present<S: Carrier>(session: &S, grant: &[u8]) -> Result<Admitted<S>, AdmissionError> {
    let preface = crate::preface::preface(grant).ok_or(AdmissionError::Invalid)?;
    let mut session = session.clone();
    let (mut send, mut recv) = session
        .open_bi()
        .await
        .map_err(|_| AdmissionError::Unavailable)?;
    if !write_all(&mut send, &preface).await {
        return Err(AdmissionError::Unavailable);
    }
    let mut status = [0u8; 1];
    if read_exact(&mut recv, &mut status).await {
        return if status == [STATUS_ADMITTED] {
            Ok(Admitted { recv, _send: send })
        } else {
            Err(AdmissionError::Denied)
        };
    }
    // The refusal byte is lost when the host closed the session under it: the
    // close code then carries the refusal.
    match tokio::time::timeout(DENIED_REPORT, closed(&session)).await {
        Ok(error) if error.session_error().map(|(code, _)| code) == Some(CLOSE_DENIED) => {
            Err(AdmissionError::Denied)
        }
        _ => Err(AdmissionError::Unavailable),
    }
}

/// An admitted native `MoQ` session. Dropping it closes the session.
pub struct Connected {
    pub session: moq_net::Session,
    wire: Wire,
    task: tokio::task::JoinHandle<()>,
}

/// Content-free cumulative counters of one carrier's QUIC connection: what it
/// costs on the wire (UDP payload bytes and datagrams) and the packet loss it
/// measured. `sent_packets` counts UDP datagrams, which is what the transport
/// statistics report as sent packets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WireStats {
    pub tx_bytes: u64,
    pub tx_datagrams: u64,
    pub rx_bytes: u64,
    pub rx_datagrams: u64,
    pub sent_packets: u64,
    pub lost_packets: u64,
}

impl Connected {
    /// Resolves when the session closes, including after a revocation.
    pub async fn closed(&self) -> moq_net::Error {
        self.session.closed().await
    }

    /// This carrier's wire counters so far.
    pub fn wire(&self) -> WireStats {
        (self.wire)()
    }
}

impl Drop for Connected {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Admit an already open session and start `moq-lite` on it.
///
/// `publish` is announced and served to the host; broadcasts the host
/// announces land in `subscribe`. The session's driver runs on the current
/// Tokio runtime until the session closes; a revocation closes it.
pub async fn start<S>(
    transport: S,
    grant: &[u8],
    publish: Option<moq_net::origin::Consumer>,
    subscribe: Option<moq_net::origin::Producer>,
) -> Result<Connected, AdmissionError>
where
    S: Carrier,
    S::SendStream: Send,
    S::RecvStream: Send,
{
    if transport.protocol() != Some(PROTOCOL) {
        close(&transport, CLOSE_PREFACE, "unsupported protocol");
        return Err(AdmissionError::Unavailable);
    }
    let mut admitted = present(&transport, grant).await?;
    let version = version().ok_or(AdmissionError::Unavailable)?;
    let mut client = moq_net::Client::new().with_versions(version.into());
    if let Some(publish) = publish {
        client = client.with_publisher(publish);
    }
    if let Some(subscribe) = subscribe {
        client = client.with_subscriber(subscribe);
    }
    let (session, driver) = tokio::time::timeout(
        SETUP_BUDGET,
        client.connect_lite(std::time::Instant::now(), transport.clone()),
    )
    .await
    .map_err(|_| AdmissionError::Unavailable)?
    .map_err(|_| AdmissionError::Unavailable)?;
    let wire = wire(&transport);
    let watched = session.clone();
    let task = tokio::spawn(async move {
        let run = moq_net::time::run(driver);
        tokio::pin!(run);
        tokio::select! {
            _ = &mut run => {}
            () = admitted.revoked() => {
                // Close first, as the host asked; the driver carries the close.
                watched.abort(moq_net::Error::Cancel);
                let _ = tokio::time::timeout(Duration::from_secs(1), &mut run).await;
            }
        }
        close(&transport, 0, "");
    });
    Ok(Connected {
        session,
        wire,
        task,
    })
}

/// Which server certificates a native client accepts.
#[derive(Clone, Debug)]
pub enum Trust {
    /// The platform's root store.
    System,
    /// Exact SHA-256 certificate fingerprints (lowercase hex).
    Fingerprints(Vec<String>),
    /// Exact certificates.
    Certificates(Vec<CertificateDer<'static>>),
}

/// How a client reaches a carrier: the one transport-specific step of
/// connecting. Everything after the transport opens — the admission preface,
/// `moq-lite` and its SETUP — is shared by every carrier.
pub trait Dial: Send + Sync + 'static {
    /// Open a session, admit `grant` on it, and start `moq-lite`. `timeout`
    /// bounds the transport and the admission together.
    #[allow(
        clippy::type_complexity,
        reason = "one boxed future per attempt: the transport differs, the steps after it do not"
    )]
    fn dial<'a>(
        &'a self,
        grant: &'a [u8],
        publish: Option<moq_net::origin::Consumer>,
        subscribe: Option<moq_net::origin::Producer>,
        timeout: Duration,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Connected, AdmissionError>> + Send + 'a>,
    >;
}

/// The WebTransport carrier: one exact HTTPS route, dialled over QUIC.
#[derive(Clone, Debug)]
pub struct WebTransport {
    pub url: url::Url,
    pub trust: Trust,
}

impl WebTransport {
    /// The carrier for the exact route `endpoint` with `fingerprints` as the
    /// trust (`None` for the platform roots).
    ///
    /// # Errors
    ///
    /// [`AdmissionError::Invalid`] unless `endpoint` is an origin followed by
    /// [`VOICE_PATH`](crate::relay::VOICE_PATH), spelled exactly as
    /// [`url::Url`] serializes it, and every fingerprint is a SHA-256 digest in
    /// lowercase hex.
    pub fn new(endpoint: &str, fingerprints: &[String]) -> Result<Self, AdmissionError> {
        let url = route(endpoint).ok_or(AdmissionError::Invalid)?;
        let trust = if fingerprints.is_empty() {
            Trust::System
        } else {
            if fingerprints.iter().any(|value| !is_fingerprint(value)) {
                return Err(AdmissionError::Invalid);
            }
            Trust::Fingerprints(fingerprints.to_vec())
        };
        Ok(Self { url, trust })
    }
}

impl Dial for WebTransport {
    fn dial<'a>(
        &'a self,
        grant: &'a [u8],
        publish: Option<moq_net::origin::Consumer>,
        subscribe: Option<moq_net::origin::Producer>,
        timeout: Duration,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Connected, AdmissionError>> + Send + 'a>,
    > {
        Box::pin(connect(
            &self.url,
            &self.trust,
            grant,
            publish,
            subscribe,
            timeout,
        ))
    }
}

/// The exact route `endpoint` names: an HTTPS origin plus
/// [`VOICE_PATH`](crate::relay::VOICE_PATH), with no credentials, query or
/// fragment, spelled the one way [`url::Url`] serializes it.
pub fn route(endpoint: &str) -> Option<url::Url> {
    let url = url::Url::parse(endpoint).ok()?;
    (url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.host_str().is_some()
        && url.path() == crate::relay::VOICE_PATH
        && url.query().is_none()
        && url.fragment().is_none()
        && url.as_str() == endpoint
        && endpoint
            == format!(
                "{}{path}",
                url.origin().ascii_serialization(),
                path = crate::relay::VOICE_PATH
            ))
    .then_some(url)
}

/// A SHA-256 digest in lowercase hex, as [`Trust::Fingerprints`] takes them.
fn is_fingerprint(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Dial `url` (an exact route), admit with `grant`, and start `moq-lite`.
///
/// # Errors
///
/// Returns [`AdmissionError`] when the carrier cannot be reached, the grant is
/// refused, or `timeout` elapses first.
pub async fn connect(
    url: &url::Url,
    trust: &Trust,
    grant: &[u8],
    publish: Option<moq_net::origin::Consumer>,
    subscribe: Option<moq_net::origin::Producer>,
    timeout: Duration,
) -> Result<Connected, AdmissionError> {
    let builder = web_transport_quinn::ClientBuilder::new();
    let client = match trust {
        Trust::System => builder.with_system_roots(),
        Trust::Fingerprints(fingerprints) => {
            let hashes = fingerprints
                .iter()
                .map(hex::decode)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|_| AdmissionError::Invalid)?;
            builder.with_server_certificate_hashes(hashes)
        }
        Trust::Certificates(certificates) => builder.with_server_certificates(certificates.clone()),
    }
    .map_err(|_| AdmissionError::Unavailable)?;
    tokio::time::timeout(timeout, async {
        let transport = client
            .connect(
                web_transport_quinn::proto::ConnectRequest::new(url.clone())
                    .with_protocol(PROTOCOL),
            )
            .await
            .map_err(|_| AdmissionError::Unavailable)?;
        start(transport, grant, publish, subscribe).await
    })
    .await
    .map_err(|_| AdmissionError::Unavailable)?
}

/// Host side of one admitted `moq-lite` session.
pub(crate) struct Accepted<S: Carrier> {
    pub session: moq_net::Session,
    pub driver: moq_net::Driver<S>,
    pub admission: Admission<S>,
    pub transport: S,
}

/// Admit `transport` with `admit`, which returns the configured server and the
/// caller's admission state (or `None` to refuse), then run the `moq-lite` SETUP. The client reaches `MoQ`
/// only after `admit` succeeded.
pub(crate) async fn accept<S, F, Fut, T>(
    transport: S,
    admit: F,
) -> Result<(Accepted<S>, T), PrefaceError>
where
    S: Carrier,
    F: FnOnce(Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = Option<(moq_net::Server, T)>>,
{
    if transport.protocol() != Some(PROTOCOL) {
        refuse(&transport, PrefaceError::Malformed);
        return Err(PrefaceError::Malformed);
    }
    let (grant, mut admission) = match read_preface(&transport).await {
        Ok(preface) => preface,
        Err(error) => {
            refuse(&transport, error);
            return Err(error);
        }
    };
    let Some((server, admitted)) = admit(grant).await else {
        admission.deny(&transport).await;
        return Err(PrefaceError::Closed);
    };
    let Some(version) = version() else {
        admission.deny(&transport).await;
        return Err(PrefaceError::Closed);
    };
    if admission.admit().await.is_err() {
        close(&transport, CLOSE_PREFACE, "admission stream closed");
        return Err(PrefaceError::Closed);
    }
    let accepted = tokio::time::timeout(
        SETUP_BUDGET,
        server
            .with_versions(version.into())
            .accept_lite(std::time::Instant::now(), transport.clone()),
    )
    .await;
    let Ok(Ok((session, driver))) = accepted else {
        close(&transport, CLOSE_PREFACE, "setup failed");
        return Err(PrefaceError::Closed);
    };
    Ok((
        Accepted {
            session,
            driver,
            admission,
            transport,
        },
        admitted,
    ))
}

/// A standalone loopback WebTransport listener on `127.0.0.1` with a freshly
/// generated, short-lived certificate. Never binds a non-loopback address.
pub struct Loopback {
    pub endpoint: quinn::Endpoint,
    pub port: u16,
    /// SHA-256 fingerprints (lowercase hex) of the generated certificate.
    pub fingerprints: Vec<String>,
}

/// Bind a loopback listener on an ephemeral port. Clients dial it as
/// `https://127.0.0.1:<port>`.
///
/// # Errors
///
/// Returns a message when the certificate cannot be generated or the port
/// cannot be bound.
pub fn loopback() -> Result<Loopback, String> {
    let identity = dev_identity(vec!["localhost".to_owned(), "127.0.0.1".to_owned()])?;
    let endpoint = listener(
        std::net::SocketAddr::from((std::net::Ipv4Addr::LOCALHOST, 0)),
        identity.chain,
        identity.key,
    )?;
    let port = endpoint
        .local_addr()
        .map_err(|error| error.to_string())?
        .port();
    Ok(Loopback {
        endpoint,
        port,
        fingerprints: vec![identity.fingerprint],
    })
}

/// A development TLS identity browsers can pin with WebTransport
/// `serverCertificateHashes`: ECDSA P-256, valid 13 days (the limit is 14).
pub struct DevIdentity {
    /// The self-signed certificate.
    pub chain: Vec<CertificateDer<'static>>,
    pub key: rustls::pki_types::PrivateKeyDer<'static>,
    /// SHA-256 of the certificate (lowercase hex).
    pub fingerprint: String,
}

/// Generate a [`DevIdentity`] for `hosts`.
///
/// # Errors
///
/// Returns a message when the certificate cannot be generated.
pub fn dev_identity(hosts: Vec<String>) -> Result<DevIdentity, String> {
    let mut params = rcgen::CertificateParams::new(hosts).map_err(|error| error.to_string())?;
    // Browsers accept pinned certificate hashes only for at most 14 days.
    let now = std::time::SystemTime::now();
    params.not_before = now.into();
    params.not_after = now
        .checked_add(Duration::from_secs(13 * 24 * 3600))
        .ok_or("certificate lifetime")?
        .into();
    let key = rcgen::KeyPair::generate().map_err(|error| error.to_string())?;
    let certificate = params
        .self_signed(&key)
        .map_err(|error| error.to_string())?;
    let der = certificate.der().clone();
    let fingerprint = {
        use sha2::Digest as _;
        hex::encode(sha2::Sha256::digest(der.as_ref()))
    };
    Ok(DevIdentity {
        chain: vec![der],
        key: rustls::pki_types::PrivateKeyDer::Pkcs8(key.serialize_der().into()),
        fingerprint,
    })
}

/// A connection with no packets for this long is gone.
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);
/// The host pings every idle connection this often. A browser's page timers
/// are throttled in background tabs (a presence keepalive may not run for a
/// minute), but its network stack still answers QUIC pings, so without the
/// host's pings an idle page lost its session at [`IDLE_TIMEOUT`].
const KEEP_ALIVE: Duration = Duration::from_secs(3);

/// A WebTransport QUIC listener with TLS 1.3, the HTTP/3 ALPN, no 0-RTT, at
/// most [`MAX_STREAMS`] streams per direction and host keepalives.
///
/// # Errors
///
/// Returns a message when the TLS identity is unusable or the address cannot
/// be bound.
pub fn listener(
    address: std::net::SocketAddr,
    chain: Vec<CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
) -> Result<quinn::Endpoint, String> {
    let socket = std::net::UdpSocket::bind(address).map_err(|error| error.to_string())?;
    listener_on(socket, crate::tls::fixed_certificate(chain, key)?)
}

/// [`listener`] on an already bound UDP socket, presenting `certificate`:
/// [`crate::tls::fixed_certificate`], or a [`crate::tls::FileCertificate`]
/// that follows renewals on disk.
///
/// # Errors
///
/// Returns a message when the TLS identity is unusable or the socket cannot
/// be used.
pub fn listener_on(
    socket: std::net::UdpSocket,
    certificate: std::sync::Arc<dyn rustls::server::ResolvesServerCert>,
) -> Result<quinn::Endpoint, String> {
    let mut tls = rustls::ServerConfig::builder_with_provider(crate::tls::provider())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|error| error.to_string())?
        .with_no_client_auth()
        .with_cert_resolver(certificate);
    tls.alpn_protocols = vec![web_transport_quinn::ALPN.as_bytes().to_vec()];
    tls.max_early_data_size = 0;
    let crypto = quinn::crypto::rustls::QuicServerConfig::try_from(tls)
        .map_err(|error| error.to_string())?;
    let mut config = quinn::ServerConfig::with_crypto(std::sync::Arc::new(crypto));
    let mut transport = quinn::TransportConfig::default();
    transport
        .max_concurrent_bidi_streams(MAX_STREAMS.into())
        .max_concurrent_uni_streams(MAX_STREAMS.into())
        .max_idle_timeout(Some(
            quinn::IdleTimeout::try_from(IDLE_TIMEOUT).map_err(|error| error.to_string())?,
        ))
        .keep_alive_interval(Some(KEEP_ALIVE));
    config.transport_config(std::sync::Arc::new(transport));
    let runtime = quinn::default_runtime().ok_or("no async runtime for the QUIC listener")?;
    quinn::Endpoint::new(
        quinn::EndpointConfig::default(),
        Some(config),
        socket,
        runtime,
    )
    .map_err(|error| error.to_string())
}

/// Accept WebTransport sessions for exactly one of `paths` on `endpoint` and
/// hand each to `serve` with the path it asked for. At most
/// [`MAX_PENDING_HANDSHAKES`] handshakes run at once, each bounded by
/// [`HANDSHAKE_BUDGET`]. Dropping the future aborts every task.
pub(crate) async fn serve_endpoint<F, Fut>(
    endpoint: quinn::Endpoint,
    paths: &'static [&'static str],
    serve: F,
) where
    F: Fn(&'static str, web_transport_quinn::Session) -> Fut + Clone + Send + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let pending = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_HANDSHAKES));
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                let Some(incoming) = incoming else { break };
                let Ok(permit) = pending.clone().try_acquire_owned() else {
                    incoming.refuse();
                    continue;
                };
                let serve = serve.clone();
                tasks.spawn(async move {
                    let request = tokio::time::timeout(HANDSHAKE_BUDGET, async {
                        let connection = incoming.await.ok()?;
                        web_transport_quinn::Request::accept(connection).await.ok()
                    })
                    .await
                    .ok()
                    .flatten();
                    let Some(request) = request else { return };
                    // Routing only: an exact path, no query, and our protocol.
                    let path = paths.iter().copied().find(|path| request.url.path() == *path);
                    let Some(path) = path.filter(|_| {
                        request.url.query().is_none()
                            && request.protocols.iter().any(|protocol| protocol == PROTOCOL)
                    }) else {
                        let _ = tokio::time::timeout(
                            Duration::from_millis(100),
                            request.reject(web_transport_quinn::proto::http::StatusCode::NOT_FOUND),
                        )
                        .await;
                        return;
                    };
                    let response = web_transport_quinn::proto::ConnectResponse::OK.with_protocol(PROTOCOL);
                    let session = tokio::time::timeout(HANDSHAKE_BUDGET, request.respond(response)).await;
                    drop(permit);
                    if let Ok(Ok(session)) = session {
                        serve(path, session).await;
                    }
                });
            }
            _ = tasks.join_next(), if !tasks.is_empty() => {}
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    endpoint.close(0u32.into(), b"listener closed");
}

/// Runs an origin's driver on the ambient Tokio runtime for as long as it
/// lives. An origin makes no progress (routing, serving, cache expiry) unless
/// its driver runs.
pub struct OriginTask(Option<tokio::task::JoinHandle<()>>);

impl OriginTask {
    /// Create an origin and spawn its driver. Must be called inside a Tokio
    /// runtime; outside one the origin is closed at once.
    pub fn produce(config: moq_net::origin::Config) -> (moq_net::origin::Producer, Self) {
        let (producer, driver) = moq_net::origin::Producer::new(config);
        let task = tokio::runtime::Handle::try_current().ok().map(|runtime| {
            runtime.spawn(async move {
                let _ = moq_net::time::run(driver).await;
            })
        });
        (producer, Self(task))
    }
}

impl Drop for OriginTask {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

/// An origin configuration with a byte-bounded cache and the given retention.
pub fn origin_config(bytes: u64, retain: Duration) -> moq_net::origin::Config {
    let mut config = moq_net::origin::Config::default();
    config.pool = moq_net::cache::Pool::new(
        moq_net::cache::Config::default()
            .with_capacity(bytes)
            .with_expiry(retain),
    );
    config.cache_duration = retain;
    config.default_max_age = retain;
    config
}

#[cfg(test)]
mod tests;
