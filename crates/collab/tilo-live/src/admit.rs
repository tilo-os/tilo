//! The client half of the carrier: present a grant, wait for the host's
//! answer, and run `moq-lite` on an admitted session.
//!
//! A native device and a browser page run the same code here. How the
//! transport is opened ([`Dial`], an iroh connection) is the caller's step. Nothing in this module needs a
//! thread or a runtime beyond plain future polling, so it builds for the page
//! (no Tokio) as it does for the desktop.

use std::sync::Arc;
use std::time::Duration;

use moq_net::transport::poll::RecvStream as _;
use moq_net::web_transport_trait::poll as wt;
use moq_net::web_transport_trait::{Error as _, Stats as _};

/// The `moq-lite` version for [`PROTOCOL`](crate::preface::PROTOCOL).
#[must_use]
pub fn version() -> Option<moq_net::Version> {
    moq_net::Version::from_alpn(crate::preface::PROTOCOL)
}

/// A carrier transport: [`moq_net`]'s poll session interface, clonable (each
/// clone makes its own progress), thread-safe, and `'static` so its driver
/// runs on a task of its own. An iroh connection qualifies; nothing in
/// this module names it.
pub trait Carrier: moq_net::transport::poll::Session + Send + Sync + 'static {}

impl<T> Carrier for T where T: moq_net::transport::poll::Session + Send + Sync + 'static {}

/// Where one carrier's wire counters come from: sampled on demand, from
/// anywhere (the session's own task, the relay's downlink).
pub type Wire = Arc<dyn Fn() -> WireStats + Send + Sync>;

/// The counters of `session`, sampled through [`wt::Stats`].
#[must_use]
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
pub(crate) async fn write_all<S>(stream: &mut S, mut buf: &[u8]) -> bool
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
pub(crate) async fn read_exact<S>(stream: &mut S, dst: &mut [u8]) -> bool
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
pub(crate) async fn closed<S: Carrier>(session: &S) -> S::Error {
    let mut session = session.clone();
    std::future::poll_fn(|cx| session.poll_closed(cx)).await
}

/// Budget for the `moq-lite` SETUP exchange after admission.
pub const SETUP_BUDGET: Duration = Duration::from_secs(5);

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
        return if status == [crate::preface::STATUS_ADMITTED] {
            Ok(Admitted { recv, _send: send })
        } else {
            Err(AdmissionError::Denied)
        };
    }
    // The refusal byte is lost when the host closed the session under it: the
    // close code then carries the refusal.
    match n0_future::time::timeout(crate::preface::DENIED_REPORT, closed(&session)).await {
        Ok(error)
            if error.session_error().map(|(code, _)| code)
                == Some(crate::preface::CLOSE_DENIED) =>
        {
            Err(AdmissionError::Denied)
        }
        _ => Err(AdmissionError::Unavailable),
    }
}

/// An admitted `MoQ` session. Dropping it closes the session.
pub struct Connected {
    pub session: moq_net::Session,
    wire: Wire,
    task: n0_future::task::JoinHandle<()>,
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
    #[must_use]
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
/// announces land in `subscribe`. The session's driver runs until the session
/// closes; a revocation closes it.
///
/// # Errors
///
/// [`AdmissionError`] when the session does not speak
/// [`PROTOCOL`](crate::preface::PROTOCOL), the grant is refused, or SETUP
/// fails.
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
    if transport.protocol() != Some(crate::preface::PROTOCOL) {
        close(
            &transport,
            crate::preface::CLOSE_PREFACE,
            "unsupported protocol",
        );
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
    let (session, driver) = n0_future::time::timeout(
        SETUP_BUDGET,
        client.connect_lite(moq_net::time::Instant::now(), transport.clone()),
    )
    .await
    .map_err(|_| AdmissionError::Unavailable)?
    .map_err(|_| AdmissionError::Unavailable)?;
    let wire = wire(&transport);
    let watched = session.clone();
    let task_transport = transport.clone();
    let task = n0_future::task::spawn(async move {
        // A revocation closes the session under the driver; the driver then
        // ends, and the transport is closed once.
        let revoker = n0_future::task::spawn(async move {
            admitted.revoked().await;
            watched.abort(moq_net::Error::Cancel);
        });
        let _ = moq_net::time::run(driver).await;
        revoker.abort();
        close(&task_transport, 0, "");
    });
    Ok(Connected {
        session,
        wire,
        task,
    })
}

/// What a [`Dial`] answers with: one attempt's future. A device's transport
/// may move the attempt between threads; a page's cannot, so the boxed
/// future carries the bound its target's executor has.
#[cfg(not(target_arch = "wasm32"))]
pub type Dialling<'a> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Connected, AdmissionError>> + Send + 'a>,
>;

/// See the device's [`Dialling`].
#[cfg(target_arch = "wasm32")]
pub type Dialling<'a> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Connected, AdmissionError>> + 'a>>;

/// How a client reaches a carrier: the one transport-specific step of
/// connecting. Everything after the transport opens — the admission preface,
/// `moq-lite` and its SETUP — is shared by every carrier.
pub trait Dial: Send + Sync + 'static {
    /// Open a session, admit `grant` on it, and start `moq-lite`. `timeout`
    /// bounds the transport and the admission together.
    fn dial<'a>(
        &'a self,
        grant: &'a [u8],
        publish: Option<moq_net::origin::Consumer>,
        subscribe: Option<moq_net::origin::Producer>,
        timeout: Duration,
    ) -> Dialling<'a>;
}

/// Runs an origin's driver for as long as it lives. An origin makes no
/// progress (routing, serving, cache expiry) unless its driver runs.
pub struct OriginTask(Option<n0_future::task::JoinHandle<()>>);

impl OriginTask {
    /// Create an origin and spawn its driver on the ambient executor.
    #[must_use]
    pub fn produce(config: moq_net::origin::Config) -> (moq_net::origin::Producer, Self) {
        let (producer, driver) = moq_net::origin::Producer::new(config);
        let task = n0_future::task::spawn(async move {
            let _ = moq_net::time::run(driver).await;
        });
        (producer, Self(Some(task)))
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
#[must_use]
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
