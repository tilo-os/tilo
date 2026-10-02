//! The browser carriers to the host (PR 541's model): HTTPS for the durable
//! plane and certification. The live plane runs over WebTransport in the
//! page and only needs [`live_grant`] from here.
//!
//! A [`HttpsLink`] is the same session an iroh peer has: the
//! [`auth`](tilo_relay_protocol::auth) challenge (the device signs only the
//! host's challenge digest), then [`Envelope`]s out through batched `/send`
//! and in through the long-polled `/events`, acknowledged by host cursor.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tilo_relay_client::DeviceSigner;
use tilo_relay_protocol::Envelope;
use tilo_relay_protocol::auth::{self, AuthError, Hello, HelloAck2};
use tilo_relay_protocol::certify::CertifyReply;
use tilo_relay_protocol::web::{
    API_PREFIX, AgentLiveGrantRequest, EventsPage, EventsRequest, FEATURE_RESUME, LiveGrant,
    MAX_POLL_MS, MAX_REQUEST_BYTES, SessionChallenge, SessionFinish, decode_grant,
};
use tilo_user_identity::{CertificationRequest, DeviceCert, RevocationList};
use tokio::sync::{mpsc, watch};

/// A pending HTTP answer: status and body.
#[cfg(not(target_family = "wasm"))]
pub type HttpFuture<'a> = Pin<Box<dyn Future<Output = Result<(u16, Vec<u8>), String>> + Send + 'a>>;
/// A pending HTTP answer: status and body.
#[cfg(target_family = "wasm")]
pub type HttpFuture<'a> = Pin<Box<dyn Future<Output = Result<(u16, Vec<u8>), String>> + 'a>>;

/// `POST` to the host origin. The page implements it with `fetch`; native
/// peers and tests with a Rust client. `Err` means the host was not reached.
pub trait HttpClient: tilo_relay_client::MaybeSendSync {
    /// Post `body` to `path` (below the host origin), with the session token
    /// header when `session` is set.
    fn post(&self, path: &str, session: Option<&str>, body: Vec<u8>) -> HttpFuture<'_>;
}

/// Why a link or request failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum LinkError {
    /// The host was not reached, or answered with a server error.
    #[error("host unavailable: {0}")]
    Unavailable(String),
    /// The host refused this device (revoked, unknown, not admitted).
    #[error("host refused: {0}")]
    Refused(String),
    /// A body did not decode.
    #[error("malformed answer: {0}")]
    Malformed(String),
    /// The signer failed.
    #[error("signer: {0}")]
    Sign(String),
}

fn path(route: &str) -> String {
    format!("{API_PREFIX}{route}")
}

async fn call<T: serde::de::DeserializeOwned>(
    client: &dyn HttpClient,
    route: &str,
    session: Option<&str>,
    body: Vec<u8>,
) -> Result<T, LinkError> {
    let bytes = call_raw(client, route, session, body).await?;
    postcard::from_bytes(&bytes).map_err(|error| LinkError::Malformed(error.to_string()))
}

async fn call_raw(
    client: &dyn HttpClient,
    route: &str,
    session: Option<&str>,
    body: Vec<u8>,
) -> Result<Vec<u8>, LinkError> {
    let (status, bytes) = client
        .post(&path(route), session, body)
        .await
        .map_err(LinkError::Unavailable)?;
    match status {
        200 => Ok(bytes),
        401 | 403 => Err(LinkError::Refused(
            String::from_utf8_lossy(&bytes).into_owned(),
        )),
        _ => Err(LinkError::Unavailable(format!("status {status}"))),
    }
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, LinkError> {
    postcard::to_allocvec(value).map_err(|error| LinkError::Malformed(error.to_string()))
}

/// Ask the host to queue `request` for its root holder. Returns the pairing
/// code, or the final answer when the host refused at once.
///
/// # Errors
///
/// Returns [`LinkError`] when the host is not reached.
pub async fn certify_request(
    client: &dyn HttpClient,
    request: &CertificationRequest,
) -> Result<CertifyReply, LinkError> {
    call(client, "/certify", None, encode(request)?).await
}

/// Long-poll the answer to a queued request. `Pending` means poll again.
///
/// # Errors
///
/// Returns [`LinkError`] when the host is not reached.
pub async fn certify_wait(
    client: &dyn HttpClient,
    digest: [u8; 32],
) -> Result<CertifyReply, LinkError> {
    call(client, "/certify/wait", None, encode(&digest)?).await
}

/// A live-plane grant for `workspace_id`, for the WebTransport admission
/// preface (or the HTTPS fallback `open`).
///
/// # Errors
///
/// Returns [`LinkError::Refused`] when this device is not admitted there.
pub async fn live_grant(
    client: &dyn HttpClient,
    session: &str,
    workspace_id: u64,
) -> Result<LiveGrant, LinkError> {
    call(client, "/live/grant", Some(session), encode(&workspace_id)?).await
}

/// A live-plane grant for agent `agent_id` this device runs in
/// `workspace_id`: it publishes under the agent's own namespace.
///
/// # Errors
///
/// Returns [`LinkError::Refused`] when this device is not admitted there.
pub async fn agent_live_grant(
    client: &dyn HttpClient,
    session: &str,
    workspace_id: u64,
    agent_id: String,
) -> Result<LiveGrant, LinkError> {
    let request = AgentLiveGrantRequest {
        workspace_id,
        agent_id,
    };
    call(
        client,
        "/live/grant/agent",
        Some(session),
        encode(&request)?,
    )
    .await
}

/// Send a member's input for a legacy window
/// ([`tilo_relay_protocol::legacy_window`]); with no events, ask whether
/// this device may drive it. `Ok(false)`: it may not.
///
/// # Errors
///
/// Returns [`LinkError`] when the host is not reached, the window is gone
/// or its session is not reachable.
pub async fn legacy_input(
    client: &dyn HttpClient,
    session: &str,
    request: &tilo_relay_protocol::legacy_window::InputRequest,
) -> Result<bool, LinkError> {
    let (status, bytes) = client
        .post(
            &path(tilo_relay_protocol::legacy_window::INPUT_ROUTE),
            Some(session),
            encode(request)?,
        )
        .await
        .map_err(LinkError::Unavailable)?;
    match status {
        204 => Ok(true),
        403 => Ok(false),
        401 => Err(LinkError::Refused(
            String::from_utf8_lossy(&bytes).into_owned(),
        )),
        _ => Err(LinkError::Unavailable(format!(
            "status {status}: {}",
            String::from_utf8_lossy(&bytes)
        ))),
    }
}

/// Administer the host's rooms as this session's device (`/rooms`).
///
/// # Errors
///
/// Returns [`LinkError`] when the host is not reached or governs no rooms.
pub async fn rooms(
    client: &dyn HttpClient,
    session: &str,
    request: &tilo_relay_protocol::rooms::RoomsRequest,
) -> Result<tilo_relay_protocol::rooms::RoomsReply, LinkError> {
    call(client, "/rooms", Some(session), encode(request)?).await
}

/// One step of joining a room with no session (`/join`).
///
/// # Errors
///
/// Returns [`LinkError`] when the host is not reached or governs no rooms.
pub async fn join(
    client: &dyn HttpClient,
    step: &tilo_relay_protocol::rooms::JoinStep,
) -> Result<tilo_relay_protocol::rooms::JoinReply, LinkError> {
    call(client, "/join", None, encode(step)?).await
}

/// An authenticated HTTPS session with the host.
pub struct HttpsLink {
    outbound: mpsc::UnboundedSender<Envelope>,
    inbound: mpsc::Receiver<Envelope>,
    session: String,
    client: Arc<dyn HttpClient>,
    end: watch::Sender<bool>,
    features: u64,
}

impl HttpsLink {
    /// Run the handshake as the device behind `signer` and start the send
    /// and poll pumps. The host must present `host_device`, certified under
    /// any person's root.
    ///
    /// # Errors
    ///
    /// Returns [`LinkError`] when the host is unreachable or either side's
    /// authentication fails.
    pub async fn connect(
        client: Arc<dyn HttpClient>,
        cert: &DeviceCert,
        host_device: [u8; 32],
        revocations: &RevocationList,
        signer: &dyn DeviceSigner,
    ) -> Result<Self, LinkError> {
        let client_nonce: [u8; 32] = rand::random();
        let challenge: SessionChallenge = call(
            client.as_ref(),
            "/session",
            None,
            encode(&Hello {
                cert: cert.clone(),
                client_nonce,
            })?,
        )
        .await?;
        // The host device is pinned, so it may be another person's: the
        // host of a room this device was invited to.
        auth::verify_pinned_host_ack(
            &challenge.ack,
            revocations,
            crate::clock::now_ns(),
            &client_nonce,
        )
        .map_err(|error: AuthError| LinkError::Refused(error.to_string()))?;
        if challenge.ack.cert.device_public_key != host_device {
            return Err(LinkError::Refused("host is not the pinned device".into()));
        }
        let sig_over_server_nonce = signer
            .sign(auth::challenge_digest(&challenge.ack.server_nonce).to_vec())
            .await
            .map_err(|error| LinkError::Sign(error.0))?;
        let (grant, features) = decode_grant(
            &call_raw(
                client.as_ref(),
                "/session/finish",
                None,
                encode(&SessionFinish {
                    handshake: challenge.handshake,
                    ack2: HelloAck2 {
                        sig_over_server_nonce,
                    },
                })?,
            )
            .await?,
        )
        .map_err(|error| LinkError::Malformed(error.to_string()))?;
        let session = hex::encode(grant.token);
        let (outbound, outgoing) = mpsc::unbounded_channel();
        let (incoming, inbound) = mpsc::channel(1024);
        let (end, ended) = watch::channel(false);
        n0_future::task::spawn(send_pump(
            client.clone(),
            session.clone(),
            outgoing,
            end.clone(),
        ));
        n0_future::task::spawn(poll_pump(
            client.clone(),
            session.clone(),
            incoming,
            ended,
            end.clone(),
        ));
        Ok(Self {
            outbound,
            inbound,
            session,
            client,
            end,
            features,
        })
    }

    /// Whether the host resumes subscriptions from positions
    /// (`Envelope::SubscribeFrom`).
    pub fn resumable(&self) -> bool {
        self.features & FEATURE_RESUME != 0
    }

    /// Whether the host asks members to compact sealed routes
    /// (`Envelope::CompactVolunteer`).
    pub fn compacts(&self) -> bool {
        self.features & tilo_relay_protocol::web::FEATURE_COMPACTION != 0
    }

    /// Whether the host governs calls in its rooms
    /// (`Envelope::VoiceJoin`, see [`tilo_relay_protocol::voice`]).
    pub fn voice(&self) -> bool {
        self.features & tilo_relay_protocol::web::FEATURE_VOICE != 0
    }

    /// Whether the host administers rooms and redeems invitations for pages
    /// (`/rooms`, `/join`).
    pub fn rooms(&self) -> bool {
        self.features & tilo_relay_protocol::web::FEATURE_ROOMS != 0
    }

    /// Queue one envelope for the host, in order.
    ///
    /// # Errors
    ///
    /// Returns the envelope when the link has ended.
    pub fn send(&self, envelope: Envelope) -> Result<(), Box<Envelope>> {
        if *self.end.borrow() {
            return Err(Box::new(envelope));
        }
        self.outbound
            .send(envelope)
            .map_err(|error| Box::new(error.0))
    }

    /// The next envelope from the host; `None` once the link ended.
    pub async fn recv(&mut self) -> Option<Envelope> {
        self.inbound.recv().await
    }

    /// The session token, for [`live_grant`].
    pub fn session(&self) -> &str {
        &self.session
    }

    /// The HTTP client of this link.
    pub fn client(&self) -> &Arc<dyn HttpClient> {
        &self.client
    }

    /// End the link. The host ends the session when polls stop.
    pub fn close(&self) {
        self.end.send_replace(true);
    }
}

impl Drop for HttpsLink {
    fn drop(&mut self) {
        self.close();
    }
}

/// Encoded envelopes one `/send` carries: the host refuses a body past
/// [`MAX_REQUEST_BYTES`], and the batch's length prefix needs a few bytes.
const SEND_BATCH_BYTES: usize = MAX_REQUEST_BYTES.saturating_sub(64 * 1024);

fn encoded_len(envelope: &Envelope) -> usize {
    postcard::serialize_with_flavor(envelope, postcard::ser_flavors::Size::default())
        .unwrap_or(usize::MAX)
}

async fn send_pump(
    client: Arc<dyn HttpClient>,
    session: String,
    mut outgoing: mpsc::UnboundedReceiver<Envelope>,
    end: watch::Sender<bool>,
) {
    let mut ended = end.subscribe();
    // An envelope that did not fit the previous batch opens the next.
    let mut carried = None;
    loop {
        let first = if let Some(envelope) = carried.take() {
            Some(envelope)
        } else {
            tokio::select! {
                envelope = outgoing.recv() => envelope,
                _ = ended.wait_for(|ended| *ended) => None,
            }
        };
        let Some(first) = first else {
            return;
        };
        let mut bytes = encoded_len(&first);
        let mut batch = vec![first];
        while let Ok(next) = outgoing.try_recv() {
            let len = encoded_len(&next);
            if bytes.saturating_add(len) > SEND_BATCH_BYTES {
                carried = Some(next);
                break;
            }
            bytes = bytes.saturating_add(len);
            batch.push(next);
        }
        let Ok(body) = postcard::to_allocvec(&batch) else {
            end.send_replace(true);
            return;
        };
        if !matches!(
            client.post(&path("/send"), Some(&session), body).await,
            Ok((204 | 200, _))
        ) {
            end.send_replace(true);
            return;
        }
    }
}

async fn poll_pump(
    client: Arc<dyn HttpClient>,
    session: String,
    incoming: mpsc::Sender<Envelope>,
    mut ended: watch::Receiver<bool>,
    end: watch::Sender<bool>,
) {
    let mut ack = None;
    loop {
        if *ended.borrow() {
            return;
        }
        let Ok(body) = postcard::to_allocvec(&EventsRequest {
            ack,
            wait_ms: MAX_POLL_MS - 5_000,
        }) else {
            break;
        };
        let answer = tokio::select! {
            answer = client.post(&path("/events"), Some(&session), body) => answer,
            _ = ended.wait_for(|ended| *ended) => return,
        };
        let page: EventsPage = match answer {
            Ok((200, bytes)) => match postcard::from_bytes(&bytes) {
                Ok(page) => page,
                Err(_) => break,
            },
            _ => break,
        };
        for envelope in page.envelopes {
            if incoming.send(envelope).await.is_err() {
                end.send_replace(true);
                return;
            }
        }
        ack = page.cursor.or(ack);
    }
    end.send_replace(true);
}
