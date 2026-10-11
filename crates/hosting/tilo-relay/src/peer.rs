//! Per-connection peer task — reads envelopes from the wire,
//! dispatches to the workspace runtime, drains the outbound queue
//! back to the wire.
//!
//! The connection carries **one** bidirectional stream. Reads and
//! writes run concurrently: a `tokio::select!` between the wire
//! reader and the outbound mpsc receiver. The receiver stops when
//! the connection closes; the reader stops on EOF / decode error;
//! either termination cancels the other via drop.

mod access_audit;
pub mod sync_refresh;

use crate::store::bounded::Invariant;
use access_audit::AccessAudit;
use std::collections::HashMap;
use std::sync::Arc;

use tilo_clock::unix_now_ns;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use crate::auth::AuthenticatedPeer;
use crate::persistence::{ReplayAdvance, ReplayHandle, ReplayTicket, WrittenBatch};
use crate::room_access::{self, Decision, RelayTrust, RoomOperation};
use crate::store::StoreError;
use crate::store::bounded::WorkLimit;
use crate::store::replay::Step;
use crate::transport::Envelope;
use crate::workspace::{SharedWorkspace, Subscriber};
use tilo_delivery_protocol::peer_actions::{
    ActionMessage, DirectedAction, SignedDirectedAction, UnavailableReason,
};
use tilo_delivery_protocol::transport_v2::{ActionCapability, EnvelopeV2, SessionReader};

/// Maximum live fan-out backlog per connected peer. Deltas are durable before
/// queueing; saturation disconnects the recipient for replay on reconnect.
const PEER_OUTBOUND_CAPACITY: usize = 32;

#[cfg(feature = "test-support")]
const CHECKPOINT_PREFIX_FAULT_ARMED: &str = ".checkpoint-prefix-post-first-promotion.armed";
#[cfg(feature = "test-support")]
const CHECKPOINT_PREFIX_FAULT_FIRED: &str = ".checkpoint-prefix-post-first-promotion.fired";

/// State of the test-only store marker used by the native-owner
/// reconnect regression. This API is absent from production builds.
#[cfg(feature = "test-support")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointPrefixPromotionFaultState {
    /// No marker: the fault is not armed.
    Absent,
    /// Armed: the next first promotion trips the fault.
    Armed,
    /// The fault fired after the first promotion.
    Fired,
}

#[cfg(feature = "test-support")]
fn checkpoint_prefix_post_first_promotion_fault(
    root: &std::path::Path,
) -> Result<(), crate::store::StoreError> {
    let armed = root.join(CHECKPOINT_PREFIX_FAULT_ARMED);
    let fired = root.join(CHECKPOINT_PREFIX_FAULT_FIRED);
    match std::fs::rename(armed, fired) {
        Ok(()) => Err(crate::persistence::error(
            "injected checkpoint prefix failure after first promotion",
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(not(feature = "test-support"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "matches the fallible test-support fault hook it stands in for"
)]
fn checkpoint_prefix_post_first_promotion_fault(
    _root: &std::path::Path,
) -> Result<(), crate::store::StoreError> {
    Ok(())
}

#[cfg(test)]
static READER_BACKLOG_OBSERVERS: std::sync::OnceLock<
    std::sync::Mutex<HashMap<u64, Arc<std::sync::atomic::AtomicUsize>>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
fn track_reader_backlog(workspace_id: u64) -> Arc<std::sync::atomic::AtomicUsize> {
    let observed = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    READER_BACKLOG_OBSERVERS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(workspace_id, observed.clone());
    observed
}

#[cfg(test)]
fn observe_reader_backlog(frame: &EnvelopeV2, len: usize) {
    let EnvelopeV2::State(Envelope::Delta { workspace_id, .. }) = frame else {
        return;
    };
    if let Some(observed) = READER_BACKLOG_OBSERVERS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .get(workspace_id)
    {
        observed.store(len, std::sync::atomic::Ordering::SeqCst);
    }
}

#[cfg(test)]
fn stop_tracking_reader_backlog(workspace_id: u64) {
    READER_BACKLOG_OBSERVERS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .remove(&workspace_id);
}

/// Reason a peer task ends.
#[derive(Debug, thiserror::Error)]
pub enum PeerError {
    /// Reader error.
    #[error("frame: {0}")]
    Frame(#[from] crate::frame::FrameError),
    /// Writer error.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    /// Postcard encode error.
    #[error("encode: {0}")]
    Encode(#[from] postcard::Error),
    /// Backing store error.
    #[error("store: {0}")]
    Store(#[from] StoreError),
    /// The peer produced a message we did not expect (e.g. Delta
    /// for a workspace it never Subscribed to).
    #[error("peer protocol violation: {0}")]
    Protocol(String),
    /// A newer connection for the same device replaced this connection's
    /// subscriber slot while its replay capture was queued.
    #[error("superseded by a newer connection for this device")]
    Superseded,
}

/// Handle to the set of workspaces the daemon owns.
pub type WorkspaceRegistry = Arc<AsyncMutex<HashMap<u64, SharedWorkspace>>>;
pub(crate) const MAX_AUTOBOUND_WORKSPACES: usize = 4096;

#[cfg(test)]
const fn autobind_has_capacity(current: usize) -> bool {
    current < MAX_AUTOBOUND_WORKSPACES
}

/// Optional on-demand workspace creation used by a same-owner home host.
/// Shared/provider hosts leave this disabled and pre-bind their tenant
/// workspaces explicitly.
#[derive(Clone, Debug)]
pub struct WorkspaceAutobind {
    /// Relay state root under which a newly seen workspace is persisted.
    pub state_root: std::path::PathBuf,
}

/// Relay-wide state a peer session needs for its whole lifetime — the
/// workspace registry, autobind policy, live revocation list, own
/// user chain, and metrics. Bundled into one struct so [`run_peer`]'s
/// argument count stays reasonable; see `SessionCtx` for the subset
/// `handle_envelope` reads on every message.
pub struct PeerContext {
    /// Handle to the set of workspaces the daemon owns.
    pub workspaces: WorkspaceRegistry,
    /// Optional on-demand workspace creation (same-owner home host).
    pub autobind: Option<WorkspaceAutobind>,
    /// Live revocation list, refreshed independently of this session.
    pub revocations: Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
    /// Live signed-workspace-derived collaborator admission policy.
    pub trust_policy: Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    /// The user pubkey this relay itself chains to.
    pub own_user_public_key: [u8; 32],
    /// Exact certified device running this relay. Private actions may use this
    /// identity for a local durable-broker self-connection even though its
    /// host capability classifies it as a peer relay.
    pub own_device_cert: tilo_user_identity::DeviceCert,
    /// Metrics — cheap counters.
    pub metrics: Arc<crate::metrics::Metrics>,
    /// This relay's own device key: the address remote-action frames use to
    /// reach the app-serving plane.
    pub own_device_public_key: [u8; 32],
    /// Answers remote-action frames addressed to this relay. `None` unless
    /// the host supplied one and advertises `app_serving`.
    pub app_serving: Option<Arc<dyn crate::app_serving::AppServingPlane>>,
    /// Also hands validated live frames and devices' presence to the
    /// browser plane, when the host serves browsers.
    pub live_bridge: Option<LiveBridge>,
    /// Agent devices this host was told about, see [`AgentRegistry`].
    pub agents: AgentRegistry,
    /// Mints and checks resumable subscribers' positions; `None` refuses
    /// resumption, and a `SubscribeFrom` replays everything.
    pub positions: Option<crate::position::PositionIssuer>,
    /// Keeps retained document channels short; `None` never compacts and
    /// refuses compaction volunteers.
    pub compactor: Option<crate::compaction::Compactor>,
}

/// The host's browser plane, as the relay's sessions feed it.
pub trait BrowserBridge: Send + Sync {
    /// A validated live frame: workspace, stamped sender, frame.
    fn live(&self, workspace_id: u64, from: [u8; 32], frame: &[u8]);
    /// The presence payload a subscribed device (`from`, of `user`) sent.
    fn presence(&self, workspace_id: u64, from: [u8; 32], user: [u8; 32], payload: &[u8]);
}

/// Receives what sessions hand the browser plane, see [`BrowserBridge`].
pub type LiveBridge = Arc<dyn BrowserBridge>;

/// Agent devices by device key: their verified delegation, or `None` once
/// the agent was withdrawn. An agent device is confined to its delegated
/// rooms, is its own principal for remote actions, and a withdrawn one is
/// refused like a revoked device.
pub type AgentRegistry = Arc<
    std::sync::RwLock<
        std::collections::HashMap<[u8; 32], Option<tilo_relay_protocol::agents::AgentDelegation>>,
    >,
>;

/// Read-mostly session state [`handle_envelope`] and its per-arm
/// helpers read on every inbound message for one connection. Built
/// once per [`run_peer`] call from a [`PeerContext`] plus the
/// session's own outbound / disconnect channels.
struct SessionCtx<'a> {
    workspaces: &'a WorkspaceRegistry,
    autobind: Option<&'a WorkspaceAutobind>,
    outbound: &'a mpsc::Sender<Envelope>,
    state_outbound: Option<&'a mpsc::Sender<StateDelivery>>,
    connection_live: Option<&'a Arc<std::sync::atomic::AtomicBool>>,
    own_user_public_key: &'a [u8; 32],
    own_device_cert: &'a tilo_user_identity::DeviceCert,
    metrics: &'a Arc<crate::metrics::Metrics>,
    trust_policy: &'a Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    disconnect: &'a mpsc::Sender<()>,
    own_device_public_key: &'a [u8; 32],
    app_serving: Option<&'a Arc<dyn crate::app_serving::AppServingPlane>>,
    /// The state-lane framing the session's protocol fixed.
    framing: tilo_delivery_protocol::transport_v2::StateFraming,
    /// The session negotiated `ALPN_SIGNED_ACTIONS_V1`: it may receive
    /// `CertifiedSignedDelivered`. Older sessions never see that variant.
    signed_actions: bool,
    private_tx: &'a mpsc::Sender<PrivateOutbound>,
    revocations: &'a Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
    live_bridge: Option<&'a LiveBridge>,
    agents: &'a AgentRegistry,
    positions: Option<crate::position::PositionIssuer>,
    compactor: Option<&'a crate::compaction::Compactor>,
    /// This connection's resume log per resumed workspace.
    resumes: &'a std::sync::Mutex<HashMap<u64, ResumeLog>>,
    /// Rooms whose live deliveries this connection could not take.
    lag: &'a Arc<Lag>,
    /// Where this connection's rooms post the presence they fan out to it
    /// (TIL-710); `None` takes each presence on the frame queue.
    presence: Option<&'a Arc<crate::presence_mailbox::PresenceMailbox>>,
}

impl SessionCtx<'_> {
    /// This relay's certificate and agent trust, as the session sees it.
    fn trust(&self) -> RelayTrust<'_> {
        RelayTrust {
            revocations: self.revocations,
            policy: self.trust_policy,
            own_user: self.own_user_public_key,
            agents: self.agents,
        }
    }

    /// The room admission decision for `peer` on `op`.
    fn decide(
        &self,
        room: &crate::workspace::WorkspaceRuntime,
        peer: &AuthenticatedPeer,
        op: RoomOperation,
    ) -> Decision {
        room_access::decide(room, &self.trust(), peer, op)
    }

    /// Whether `peer` may act in `room` on `op` now, without enrolment.
    fn admits(
        &self,
        room: &crate::workspace::WorkspaceRuntime,
        peer: &AuthenticatedPeer,
        op: RoomOperation,
    ) -> bool {
        self.decide(room, peer, op).admitted()
    }
}

/// What a resuming subscriber has received: the frames this connection wrote
/// to it are appended to its resume log, and a [`Envelope::Position`] naming
/// the log's length follows them.
struct ResumeLog {
    store: crate::store::WorkspaceStore,
    issuer: crate::position::PositionIssuer,
    device: [u8; 32],
    generation: [u8; 16],
    resume: crate::position::Resume,
}

impl ResumeLog {
    /// Record `frames` as delivered; returns the position covering them.
    fn record<'f>(&mut self, frames: impl IntoIterator<Item = &'f Envelope>) -> Envelope {
        let digests: Vec<[u8; 32]> = frames
            .into_iter()
            .filter(|frame| matches!(frame, Envelope::Delta { .. }))
            .filter_map(crate::store::resume::frame_digest)
            .collect();
        if !digests.is_empty() {
            match self
                .store
                .append_resume(&self.device, &self.resume.log, &digests)
            {
                Ok(()) => {
                    self.resume.len = self.resume.len.saturating_add(digests.len() as u64);
                }
                // A position never covers what the log did not record; the
                // device replays those frames once more on its next resume.
                Err(error) => tracing::warn!(%error, "resume log append failed"),
            }
        }
        self.issuer.envelope(
            self.store.workspace_id(),
            self.device,
            self.generation,
            self.resume,
        )
    }
}

/// How often a lagging connection checks whether its queues drained.
const LAG_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// The next lagging room this connection can resume: a `SubscribeFrom` at
/// its current position, handled as if the device had sent it. A current
/// plain subscription catches up through the same authorized full replay.
fn lag_resume(session: &SessionCtx<'_>, subscribed: &[u64]) -> Option<Envelope> {
    while let Some(workspace_id) = session.lag.next() {
        if !subscribed.contains(&workspace_id) {
            continue;
        }
        if let Some(Envelope::Position { position, .. }) = resume_record(session, workspace_id, [])
        {
            return Some(Envelope::SubscribeFrom {
                workspace_id,
                position: Some(position),
            });
        }
        return Some(Envelope::Subscribe { workspace_id });
    }
    None
}

/// Record frames written to `workspace_id` in its resume log, if it has one,
/// and return the position to write after them.
fn resume_record<'f>(
    session: &SessionCtx<'_>,
    workspace_id: u64,
    frames: impl IntoIterator<Item = &'f Envelope>,
) -> Option<Envelope> {
    let mut resumes = session
        .resumes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    resumes.get_mut(&workspace_id).map(|log| log.record(frames))
}

/// Run one peer connection until the wire closes.
///
/// `read` / `write` are the halves of the bidirectional stream
/// **after** the auth handshake completed.
///
/// # Errors
///
/// Returns [`PeerError`] if a frame fails to decode, a write to the
/// wire fails, the peer's device is revoked or falls out of workspace
/// membership while connected, the peer violates the protocol (e.g. a
/// `Delta` before `Subscribe`), or the peer's outbound queue overflows
/// its backpressure limit.
pub async fn run_peer<R, W>(
    peer: AuthenticatedPeer,
    connection: iroh::endpoint::Connection,
    read: R,
    write: W,
    ctx: PeerContext,
) -> Result<(), PeerError>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    run_peer_version(peer, connection, read, write, ctx, SessionProtocol::V1).await
}

/// What a session negotiated through its ALPN.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SessionProtocol {
    /// How its state lane is framed.
    pub(crate) framing: tilo_delivery_protocol::transport_v2::StateFraming,
    /// It may send device-signed actions and receive them certified.
    pub(crate) signed_actions: bool,
}

impl SessionProtocol {
    /// `tilo-relay/1`.
    pub(crate) const V1: Self = Self {
        framing: tilo_delivery_protocol::transport_v2::StateFraming::V1,
        signed_actions: false,
    };
    /// `ALPN_AGENT_READS_V1`.
    pub(crate) const V2: Self = Self {
        framing: tilo_delivery_protocol::transport_v2::StateFraming::V2,
        signed_actions: false,
    };
    /// `ALPN_SIGNED_ACTIONS_V1`.
    pub(crate) const SIGNED_ACTIONS: Self = Self {
        framing: tilo_delivery_protocol::transport_v2::StateFraming::V2,
        signed_actions: true,
    };
    /// `ALPN_COMPACT_STATE_V1`: signed actions, and zero-signature deltas
    /// without their signature.
    pub(crate) const COMPACT_STATE: Self = Self {
        framing: tilo_delivery_protocol::transport_v2::StateFraming::Compact,
        signed_actions: true,
    };
    /// `ALPN_PRESENCE_BATCH_V1`: [`Self::COMPACT_STATE`] whose presence may
    /// arrive slotted and is written batched and slot-coded.
    pub(crate) const PRESENCE_BATCH: Self = Self {
        framing: tilo_delivery_protocol::transport_v2::StateFraming::Presence,
        signed_actions: true,
    };
    /// `ALPN_CODED_DELTA_V1`: [`Self::PRESENCE_BATCH`] whose live deltas are
    /// written coded against the session's previous delta from the same
    /// author (TIL-755).
    pub(crate) const CODED_DELTA: Self = Self {
        framing: tilo_delivery_protocol::transport_v2::StateFraming::CodedDelta,
        signed_actions: true,
    };
}

/// [`run_peer`] over a carrier other than an iroh connection (a browser's
/// HTTPS session): `closed` resolves when that carrier ends.
///
/// # Errors
///
/// As [`run_peer`].
pub async fn run_peer_until<R, W>(
    peer: AuthenticatedPeer,
    closed: impl std::future::Future<Output = ()> + Send,
    read: R,
    write: W,
    ctx: PeerContext,
) -> Result<(), PeerError>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    run_peer_stream(peer, read, write, ctx, SessionProtocol::V1, closed).await
}

pub(crate) async fn run_peer_version<R, W>(
    peer: AuthenticatedPeer,
    connection: iroh::endpoint::Connection,
    read: R,
    write: W,
    ctx: PeerContext,
    protocol: SessionProtocol,
) -> Result<(), PeerError>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    run_peer_stream(peer, read, write, ctx, protocol, connection.closed()).await
}

// Shared stream driver also exercised with a deterministic in-memory wire.
#[expect(
    clippy::too_many_lines,
    reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the session context is borrowed by the concurrent reader and writer futures, and permits move into the backlog; the lint does not see those uses"
)]
async fn run_peer_stream<R, W>(
    peer: AuthenticatedPeer,
    read: R,
    write: W,
    ctx: PeerContext,
    protocol: SessionProtocol,
    closed: impl std::future::Future,
) -> Result<(), PeerError>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::pin!(closed);
    let SessionProtocol {
        framing,
        signed_actions,
    } = protocol;
    let v2 = framing.is_v2();
    let PeerContext {
        workspaces,
        autobind,
        revocations,
        trust_policy,
        own_user_public_key,
        own_device_cert,
        metrics,
        own_device_public_key,
        app_serving,
        live_bridge,
        agents,
        positions,
        compactor,
    } = ctx;
    let resumes = std::sync::Mutex::new(HashMap::new());
    let lag = Arc::new(Lag::default());
    // Everything this session is sent passes here, so its egress and the
    // relay's are what the stream accepted, not an estimate.
    let mut write = crate::metrics::EgressWrite::new(write, &metrics, peer.device_public_key);

    // Outbound channel — every workspace runtime this peer is
    // subscribed to writes into it; we drain to the wire.
    let (tx, mut rx) = mpsc::channel::<Envelope>(PEER_OUTBOUND_CAPACITY);
    let (state_tx, mut state_rx) = mpsc::channel::<StateDelivery>(PEER_OUTBOUND_CAPACITY);
    let (disconnect_tx, mut disconnect_rx) = mpsc::channel::<()>(1);
    let mut reader = SessionReader::framed(read, framing);
    let (private_tx, mut private_rx) = mpsc::channel(PEER_OUTBOUND_CAPACITY);
    // Track our subscriptions so we can unregister on disconnect.
    let mut subscribed: Vec<u64> = Vec::new();
    let mut authority = crate::authority_epoch::subscribe();
    authority.mark_unchanged();
    let mut access_audit = AccessAudit::new();

    let connection_live = ConnectionLease(Arc::new(std::sync::atomic::AtomicBool::new(true)));
    // Every protocol gets its presence coalesced and paced; only a
    // `tilo-relay/6` session gets it batched and slot-coded.
    let presence = Arc::new(crate::presence_mailbox::PresenceMailbox::default());
    let session = SessionCtx {
        workspaces: &workspaces,
        autobind: autobind.as_ref(),
        outbound: &tx,
        state_outbound: Some(&state_tx),
        connection_live: Some(&connection_live.0),
        own_user_public_key: &own_user_public_key,
        own_device_cert: &own_device_cert,
        metrics: &metrics,
        trust_policy: &trust_policy,
        disconnect: &disconnect_tx,
        own_device_public_key: &own_device_public_key,
        app_serving: app_serving.as_ref(),
        framing,
        signed_actions,
        private_tx: &private_tx,
        revocations: &revocations,
        live_bridge: live_bridge.as_ref(),
        agents: &agents,
        positions,
        compactor: compactor.as_ref(),
        resumes: &resumes,
        lag: &lag,
        presence: Some(&presence),
    };

    // A clean EOF is only one of the ways this ends: a write to a
    // connection the peer has already closed, a frame that will not
    // decode, an envelope the handler rejects. Every one of them has to
    // unregister, so the session runs to a value and the cleanup below
    // is unconditional. Leaving a subscription behind is not cosmetic —
    // fan-out then writes deltas to a peer that is gone instead of
    // queueing them for store-and-forward, and they are lost.
    let (reply_tx, mut reply_rx) = mpsc::channel::<ReplayWrite>(8);
    let pending_replays = std::sync::atomic::AtomicUsize::new(0);
    let reading = async {
        let mut backlog: std::collections::VecDeque<(
            EnvelopeV2,
            Arc<tokio::sync::OwnedSemaphorePermit>,
        )> = std::collections::VecDeque::new();
        let mut catching_up = false;
        let mut reader_open = true;
        loop {
            let (frame, inbound_budget) = if let Some((frame, budget)) = backlog.pop_front() {
                (frame, Some(budget))
            } else {
                if !reader_open {
                    // Keep the authenticated lease live until every owner job
                    // admitted before the FIN has passed a durable barrier.
                    for id in &subscribed {
                        let workspace = session.workspaces.lock().await.get(id).cloned();
                        if let Some(workspace) = workspace {
                            let owner = crate::persistence::handle(&workspace).await;
                            let done = owner
                                .submit_prepared_wait(
                                    1,
                                    async { Ok(()) },
                                    |_, ()| Ok(()),
                                    |result| async { result },
                                )
                                .await?;
                            crate::persistence::receipt(done).await?;
                        }
                    }
                    return Ok::<(), PeerError>(());
                }
                // Prefer an already-readable inbound frame over the membership
                // audit. After a live revoke the audit and a refused delta can
                // become ready together; the delta must win so the peer still
                // receives NotAuthorised rather than a bare close.
                access_audit.cover(&subscribed);
                tokio::select! {
                    biased;
                    frame = reader.read_next() => {
                        if let Some(frame) = frame? {
                            (frame, None)
                        } else {
                            reader_open = false;
                            continue;
                        }
                    },
                    () = lag.wake.notified() => { catching_up = true; continue; }
                    // A lagging room resumes once everything already queued
                    // for this connection is written, so its replay starts
                    // where the wire left off.
                    () = tokio::time::sleep(LAG_POLL), if catching_up => {
                        if state_tx.capacity() < state_tx.max_capacity() || tx.capacity() < tx.max_capacity() {
                            continue;
                        }
                        let resume = lag_resume(&session, &subscribed);
                        catching_up = !lag.is_empty();
                        match resume { Some(frame) => (EnvelopeV2::State(frame), None), None => continue }
                    }
                    () = tokio::time::sleep_until(access_audit.due()) => {
                        let watched = audit_subscriber_access(&peer, &session, &subscribed).await?;
                        access_audit.audited(&peer, &agents, &subscribed, watched);
                        continue;
                    }
                    // The epoch's sender is a process-wide static and never
                    // closes, so this cannot spin on an error.
                    _ = authority.changed() => { access_audit.changed(); continue; }
                }
            };
            match frame {
                EnvelopeV2::State(env) => {
                    trace_native_publication("server_received", &env, None);
                    let prefix_len =
                        eligible_checkpoint_prefix(&env, backlog.iter().map(|(frame, _)| frame));
                    let mut prefix = vec![env];
                    let mut prefix_budgets = vec![inbound_budget];
                    for _ in 1..prefix_len {
                        let Some((EnvelopeV2::State(next), budget)) = backlog.pop_front() else {
                            return Err(PeerError::Io(std::io::Error::other(
                                "eligible checkpoint prefix changed while draining",
                            )));
                        };
                        prefix.push(next);
                        prefix_budgets.push(Some(budget));
                    }
                    let env = prefix.remove(0);
                    let replaying = matches!(
                        env,
                        Envelope::Subscribe { .. } | Envelope::SubscribeFrom { .. }
                    );
                    let blocked_room = match &env {
                        Envelope::Subscribe { workspace_id }
                        | Envelope::SubscribeFrom { workspace_id, .. }
                        | Envelope::Unsubscribe { workspace_id } => Some(*workspace_id),
                        _ => None,
                    };
                    if replaying {
                        pending_replays.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                    let mut ticket = None;
                    let reply = {
                        let mut operation = Box::pin(async {
                            if prefix.is_empty() {
                                handle_envelope(&peer, env, &session, &mut subscribed, &mut ticket)
                                    .await
                            } else {
                                let mut envelopes =
                                    Vec::with_capacity(prefix.len().saturating_add(1));
                                envelopes.push(env);
                                envelopes.extend(prefix);
                                handle_delta_prefix(
                                    &peer,
                                    &session,
                                    &subscribed,
                                    envelopes,
                                    prefix_budgets,
                                )
                                .await
                            }
                        });
                        loop {
                            tokio::select! {
                                biased;
                                result = &mut operation => break result?,
                                frame = reader.read_next(), if reader_open && backlog.len() < 32 => {
                                    let Some(frame) = frame? else { reader_open = false; continue; };
                                    let can_route = private_may_overtake(&frame, blocked_room, v2 && pending_replays.load(std::sync::atomic::Ordering::SeqCst) == 0, backlog.iter().map(|(frame, _)| frame));
                                    if can_route {
                                        // The state future may hold the authorization lane
                                        // needed by this private request. Keep polling it
                                        // until it completes, retaining its one reply.
                                        let private = handle_private(frame, &peer, &session);
                                        tokio::pin!(private);
                                        let mut completed = None;
                                        let reply = loop {
                                            tokio::select! {
                                                biased;
                                                result = &mut operation, if completed.is_none() => {
                                                    completed = Some(result?);
                                                }
                                                result = &mut private => break result?,
                                            }
                                        };
                                        if let Some(reply) = reply {
                                            // Wait for the writer, still polling the held
                                            // state future that it may depend on.
                                            let sending = send_private_reply(&private_tx, reply);
                                            tokio::pin!(sending);
                                            loop {
                                                tokio::select! {
                                                    biased;
                                                    result = &mut operation, if completed.is_none() => {
                                                        completed = Some(result?);
                                                    }
                                                    result = &mut sending => break result?,
                                                }
                                            }
                                        }
                                        if let Some(reply) = completed {
                                            break reply;
                                        }
                                    } else {
                                        let budget = crate::persistence::delivery_budget(inbound_bytes(&frame))?;
                                        backlog.push_back((frame, budget));
                                        #[cfg(test)]
                                        observe_reader_backlog(&backlog.back().unwrap().0, backlog.len());
                                    }
                                }
                            }
                        }
                    };
                    if crate::persistence::diagnostics()
                        && let Some(shape) =
                            buffered_retention_prefix(backlog.iter().map(|(frame, _budget)| frame))
                    {
                        tracing::info!(
                            phase = "buffered_retention_prefix",
                            authorization = "unvalidated_pre_auth",
                            frames = shape.frames,
                            input_bytes = shape.input_bytes,
                            checkpoints = shape.checkpoints,
                            deltas = shape.deltas,
                            distinct_channels = shape.distinct_channels,
                            frame_cap = RETENTION_PREFIX_FRAME_CAP,
                            byte_cap = RETENTION_PREFIX_BYTE_CAP,
                            frame_cap_reached = shape.frame_cap_reached,
                            byte_cap_reached = shape.byte_cap_reached,
                            stop_reason = shape.stop_reason,
                        );
                    }
                    if replaying || reply.is_some() || ticket.is_some() {
                        let sending = reply_tx.send(ReplayWrite {
                            ticket,
                            reply,
                            replaying,
                        });
                        tokio::pin!(sending);
                        loop {
                            tokio::select! {
                                biased;
                                result = &mut sending => {
                                    result.map_err(|_| PeerError::Protocol("reply writer closed".into()))?;
                                    break;
                                }
                                frame = reader.read_next(), if reader_open && backlog.len() < 32 => {
                                    let Some(frame) = frame? else { reader_open = false; continue; };
                                    let can_route = private_may_overtake(&frame, None, v2 && pending_replays.load(std::sync::atomic::Ordering::SeqCst) == 0, backlog.iter().map(|(frame, _)| frame));
                                    if can_route {
                                        if let Some(reply) = handle_private(frame, &peer, &session).await? {
                                            send_private_reply(&private_tx, reply).await?;
                                        }
                                    } else {
                                        let budget = crate::persistence::delivery_budget(inbound_bytes(&frame))?;
                                        backlog.push_back((frame, budget));
                                        #[cfg(test)]
                                        observe_reader_backlog(&backlog.back().unwrap().0, backlog.len());
                                    }
                                    // Give the sole writer a turn even when the
                                    // peer supplies an always-ready control flood.
                                    tokio::task::yield_now().await;
                                }
                            }
                        }
                    }
                }
                frame => {
                    if let Some(reply) = handle_private(frame, &peer, &session).await? {
                        send_private_reply(&private_tx, reply).await?;
                    }
                }
            }
        }
    };
    let writing = async {
        let mut active: Option<ReplayWrite> = None;
        let mut private_burst = 0usize;
        let mut deferred_state = None;
        let mut deferred_frame = None;
        let mut raw_turn = false;
        // This connection's halves of its presence and delta slot coding:
        // they see every batch and coded delta this writer writes, in order.
        let mut coding = SessionCoding::default();
        loop {
            if active.is_none() {
                active = reply_rx.try_recv().ok();
            }
            // At most eight private frames before one available state/replay
            // frame. A single writer completes every frame before switching.
            if private_burst < 8
                && let Ok(private) = private_rx.try_recv()
            {
                write_private(private, &peer, &session, &mut write).await?;
                private_burst = private_burst.saturating_add(1);
                continue;
            }
            if let Some(batch) = active.take() {
                Box::pin(finish_replay_write(
                    batch,
                    &peer,
                    &session,
                    &pending_replays,
                    &mut private_rx,
                    &mut write,
                ))
                .await?;
                private_burst = 0;
                continue;
            }
            let live = pending_replays.load(std::sync::atomic::Ordering::SeqCst) == 0;
            if live {
                if deferred_frame.is_none() {
                    deferred_frame = rx.try_recv().ok();
                }
                if deferred_state.is_none() {
                    deferred_state = state_rx.try_recv().ok();
                }
                if (raw_turn || deferred_state.is_none())
                    && let Some(frame) = deferred_frame.take()
                {
                    authorize_state_frame(&frame, &peer, &session).await?;
                    // Presence and host-authored deltas are fan-out; the
                    // rest of this lane answers or addresses one device.
                    let live = matches!(frame, Envelope::Delta { .. } | Envelope::Presence { .. });
                    write
                        .write_all(&encode_charged(&frame, &session, live)?)
                        .await?;
                    trace_native_publication("written", &frame, Some(&peer.device_public_key));
                    private_burst = 0;
                    raw_turn = false;
                    continue;
                }
                if let Some(state) = deferred_state.take() {
                    write_state_delivery(
                        state,
                        &peer,
                        &session,
                        &mut coding,
                        &presence,
                        &mut write,
                    )
                    .await?;
                    private_burst = 0;
                    raw_turn = true;
                    continue;
                }
                // Presence goes once no delta is waiting: it never delays one.
                if presence
                    .next_due()
                    .is_some_and(|due| due <= tokio::time::Instant::now())
                {
                    write_presence(&presence, &mut coding.presence, &peer, &session, &mut write)
                        .await?;
                    private_burst = 0;
                    continue;
                }
            }
            // None of the queues was ready. Await readiness without starting
            // a write in a cancellable select branch.
            tokio::select! {
                biased;
                batch = reply_rx.recv() => { active = batch; }
                private = private_rx.recv(), if v2 => {
                    if let Some(private) = private { write_private(private, &peer, &session, &mut write).await?; private_burst = 1; }
                }
                state = state_rx.recv(), if live => {
                    deferred_state = state;
                }
                frame = rx.recv(), if live => {
                    deferred_frame = frame;
                }
                () = presence.ready(), if live => {}
            }
        }
        #[allow(
            unreachable_code,
            reason = "The terminal Ok fixes the async loop result type although every exit currently returns an error."
        )]
        Ok::<(), PeerError>(())
    };
    let outcome = tokio::select! {
        _ = &mut closed => Ok(()),
        _ = disconnect_rx.recv() => Err(PeerError::Protocol("subscriber exceeded outbound backpressure limit".into())),
        result = reading => result,
        result = writing => result,
    };

    connection_live
        .0
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let outcome = finish_peer_outcome(outcome, &metrics, &peer.device_public_key);
    tracing::debug!(
        device = %hex::encode(&peer.device_public_key[..4]),
        bytes_sent = write.bytes_sent(),
        "peer session egress"
    );
    if crate::persistence::correlated_diagnostics() {
        tracing::info!(phase = "peer_end", device = %hex::encode(&peer.device_public_key[..4]), outcome = ?outcome, workspaces = ?subscribed);
    }
    let wsmap = workspaces.lock().await;
    for id in &subscribed {
        if let Some(ws) = wsmap.get(id) {
            ws.lock()
                .await
                .remove_subscriber_session(&peer.device_public_key, session.outbound);
        }
    }
    drop(wsmap);
    outcome
}

fn finish_peer_outcome(
    outcome: Result<(), PeerError>,
    metrics: &crate::metrics::Metrics,
    device: &[u8; 32],
) -> Result<(), PeerError> {
    match outcome {
        // A superseded session is a normal handover, not a failure.
        Ok(()) | Err(PeerError::Superseded) => Ok(()),
        Err(error) => {
            metrics.inc_subscriber_dropped();
            tracing::warn!(
                device = %hex::encode(&device[..4]),
                %error,
                "peer session ended with error"
            );
            Err(error)
        }
    }
}

struct ConnectionLease(Arc<std::sync::atomic::AtomicBool>);
impl Drop for ConnectionLease {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}
fn control_room(frame: &EnvelopeV2) -> Option<u64> {
    match frame {
        EnvelopeV2::State(
            Envelope::Subscribe { workspace_id }
            | Envelope::SubscribeFrom { workspace_id, .. }
            | Envelope::Unsubscribe { workspace_id },
        ) => Some(*workspace_id),
        EnvelopeV2::EnableActions { room }
        | EnvelopeV2::EnableBrokerClient { room }
        | EnvelopeV2::RegisterBroker { room, .. } => {
            crate::room_workspace_id(&room.to_string()).ok()
        }
        _ => None,
    }
}

fn private_may_overtake<'a>(
    frame: &EnvelopeV2,
    active_control: Option<u64>,
    heartbeat_ready: bool,
    mut queued: impl Iterator<Item = &'a EnvelopeV2>,
) -> bool {
    // A transport-only heartbeat cannot depend on state admission finishing:
    // its silence deadline would otherwise replace an authorized live session.
    // Preserve Ready and earlier capability/unsubscribe reply ordering.
    if matches!(frame, EnvelopeV2::State(Envelope::Ping)) {
        return heartbeat_ready
            && active_control.is_none()
            && queued.all(|frame| control_room(frame).is_none());
    }
    let action = match frame {
        EnvelopeV2::Directed(action) => action,
        EnvelopeV2::SignedDirected(signed) => signed.action(),
        _ => return false,
    };
    let Ok(room) = crate::room_workspace_id(&action.room.to_string()) else {
        return false;
    };
    active_control != Some(room) && queued.all(|frame| control_room(frame) != Some(room))
}

// Both ends trace the same publication hop; the client side lives with the
// session client.
pub(crate) use tilo_relay_session::trace_native_publication;

fn inbound_bytes(frame: &EnvelopeV2) -> usize {
    match frame {
        EnvelopeV2::State(Envelope::Delta { delta, .. }) => delta.len().saturating_add(256),
        EnvelopeV2::State(Envelope::Presence { payload, .. }) => payload.len().saturating_add(256),
        EnvelopeV2::State(Envelope::SyncResponse { delta_since, .. }) => {
            delta_since.len().saturating_add(256)
        }
        EnvelopeV2::State(Envelope::SyncRequest { have_heads, .. }) => {
            have_heads.len().saturating_mul(32).saturating_add(256)
        }
        EnvelopeV2::State(Envelope::PeerRelayHello { hosted_workspaces }) => hosted_workspaces
            .len()
            .saturating_mul(8)
            .saturating_add(256),
        EnvelopeV2::State(Envelope::NotAuthorised { reason, .. }) => {
            reason.len().saturating_add(256)
        }
        // Bounds all non-state/private controls without serializing payloads.
        _ => 64 * 1024,
    }
}

const RETENTION_PREFIX_FRAME_CAP: usize = 32;
const RETENTION_PREFIX_BYTE_CAP: usize = 64 * 1024 * 1024;

fn eligible_checkpoint_prefix<'a>(
    current: &Envelope,
    backlog: impl Iterator<Item = &'a EnvelopeV2>,
) -> usize {
    let Envelope::Delta {
        workspace_id,
        author_device_public_key,
        delta,
        ..
    } = current
    else {
        return 1;
    };
    let Some((current_channel, crate::coedit::RetentionKind::Checkpoint)) =
        crate::coedit::retention_metadata(delta)
    else {
        return 1;
    };
    let current_bytes = delta.len().saturating_add(256);
    if current_bytes > RETENTION_PREFIX_BYTE_CAP {
        return 1;
    }
    let mut channels = std::collections::HashSet::new();
    channels.insert(current_channel);
    let mut total_bytes = current_bytes;
    let mut total = 1;
    for frame in backlog {
        if total == RETENTION_PREFIX_FRAME_CAP {
            break;
        }
        let EnvelopeV2::State(Envelope::Delta {
            workspace_id: frame_workspace,
            author_device_public_key: frame_author,
            delta: frame_delta,
            ..
        }) = frame
        else {
            break;
        };
        if frame_workspace != workspace_id || frame_author != author_device_public_key {
            break;
        }
        let Some((channel, crate::coedit::RetentionKind::Checkpoint)) =
            crate::coedit::retention_metadata(frame_delta)
        else {
            break;
        };
        if !channels.insert(channel) {
            break;
        }
        let frame_bytes = frame_delta.len().saturating_add(256);
        if total_bytes.saturating_add(frame_bytes) > RETENTION_PREFIX_BYTE_CAP {
            break;
        }
        total_bytes = total_bytes.saturating_add(frame_bytes);
        total = total.saturating_add(1);
    }
    total
}

#[derive(Debug, PartialEq, Eq)]
struct BufferedRetentionPrefix {
    frames: usize,
    input_bytes: usize,
    checkpoints: usize,
    deltas: usize,
    distinct_channels: usize,
    frame_cap_reached: bool,
    byte_cap_reached: bool,
    stop_reason: &'static str,
}

fn buffered_retention_prefix<'a>(
    frames: impl Iterator<Item = &'a EnvelopeV2>,
) -> Option<BufferedRetentionPrefix> {
    buffered_retention_prefix_with_limits(
        frames,
        RETENTION_PREFIX_FRAME_CAP,
        RETENTION_PREFIX_BYTE_CAP,
    )
}

fn buffered_retention_prefix_with_limits<'a>(
    frames: impl Iterator<Item = &'a EnvelopeV2>,
    frame_cap: usize,
    byte_cap: usize,
) -> Option<BufferedRetentionPrefix> {
    let mut group: Option<(u64, [u8; 32])> = None;
    let mut channels = std::collections::HashSet::new();
    let mut inspected = false;
    let mut shape = BufferedRetentionPrefix {
        frames: 0,
        input_bytes: 0,
        checkpoints: 0,
        deltas: 0,
        distinct_channels: 0,
        frame_cap_reached: false,
        byte_cap_reached: false,
        stop_reason: "end",
    };
    for frame in frames {
        inspected = true;
        if shape.frames == frame_cap {
            shape.frame_cap_reached = true;
            shape.stop_reason = "frame_cap";
            break;
        }
        let EnvelopeV2::State(Envelope::Delta {
            workspace_id,
            author_device_public_key,
            delta,
            ..
        }) = frame
        else {
            shape.stop_reason = "non_delta";
            break;
        };
        let Some((channel, kind)) = crate::coedit::retention_metadata(delta) else {
            shape.stop_reason = "non_retained";
            break;
        };
        let current = (*workspace_id, *author_device_public_key);
        match group {
            Some(expected) if expected != current => {
                shape.stop_reason = if expected.0 == current.0 {
                    "author_change"
                } else {
                    "workspace_change"
                };
                break;
            }
            None => group = Some(current),
            _ => {}
        }
        let bytes = inbound_bytes(frame);
        if shape.input_bytes.saturating_add(bytes) > byte_cap {
            shape.byte_cap_reached = true;
            shape.stop_reason = "byte_cap";
            break;
        }
        shape.frames = shape.frames.saturating_add(1);
        shape.input_bytes = shape.input_bytes.saturating_add(bytes);
        channels.insert(channel);
        match kind {
            crate::coedit::RetentionKind::Checkpoint => {
                shape.checkpoints = shape.checkpoints.saturating_add(1);
            }
            crate::coedit::RetentionKind::Delta => shape.deltas = shape.deltas.saturating_add(1),
        }
    }
    if !inspected {
        return None;
    }
    if shape.frames == frame_cap && shape.stop_reason == "end" {
        shape.frame_cap_reached = true;
        shape.stop_reason = "frame_cap";
    }
    shape.distinct_channels = channels.len();
    Some(shape)
}

fn retention_publication_kind(
    publication: &crate::store::shell_selection::Publication,
) -> &'static str {
    match publication {
        crate::store::shell_selection::Publication::Suppress => "suppress",
        crate::store::shell_selection::Publication::Activate(_) => "activate",
        crate::store::shell_selection::Publication::ReplayRequired(_) => "recovery",
        crate::store::shell_selection::Publication::Original => "original",
    }
}

struct ReplayWrite {
    ticket: Option<(u64, ReplayTicket, ReplayHandle, Arc<ReplayCatchup>)>,
    reply: Option<Envelope>,
    replaying: bool,
}
/// Queue a reply to the peer's own private request. A full queue means the
/// peer is not reading its replies yet: wait for the writer, which stops this
/// reader and so backpressures the peer, rather than dropping the connection.
async fn send_private_reply(
    private_tx: &mpsc::Sender<PrivateOutbound>,
    reply: EnvelopeV2,
) -> Result<(), PeerError> {
    private_tx
        .send(PrivateOutbound::Reply(reply))
        .await
        .map_err(|_| PeerError::Protocol("private reply writer closed".into()))
}

async fn write_private<W: tokio::io::AsyncWrite + Unpin>(
    private: PrivateOutbound,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    write: &mut W,
) -> Result<(), PeerError> {
    match private {
        PrivateOutbound::Reply(reply) => {
            write
                .write_all(&tilo_delivery_protocol::transport_v2::encode(&reply)?)
                .await?;
        }
        PrivateOutbound::Delivery(delivery) => {
            write_private_delivery(&delivery, peer, session, write).await?;
        }
    }
    Ok(())
}
async fn authorize_state_frame(
    frame: &Envelope,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<(), PeerError> {
    let (id, subscribed) = match frame {
        Envelope::Delta { workspace_id, .. }
        | Envelope::Ready { workspace_id, .. }
        | Envelope::Presence { workspace_id, .. } => (*workspace_id, true),
        Envelope::SyncResponse { workspace_id, .. } => (*workspace_id, false),
        _ => return Ok(()),
    };
    authorize_state_recipient(id, subscribed, peer, session).await
}

/// [`authorize_state_frame`] for a frame to room `id`; `subscribed` frames
/// (deltas, presence, `Ready`) also need this connection to be the room's
/// current subscriber.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the membership guard spans the whole authorization so it observes one roster generation"
)]
async fn authorize_state_recipient(
    id: u64,
    subscribed: bool,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<(), PeerError> {
    let ws = session
        .workspaces
        .lock()
        .await
        .get(&id)
        .cloned()
        .ok_or_else(|| PeerError::Protocol("workspace disappeared".into()))?;
    let wr = crate::workspace_io::fresh_membership(&ws).await?;
    if !session.admits(&wr, peer, RoomOperation::State)
        || ((subscribed || wr.subscribers.contains_key(&peer.device_public_key))
            && !current_session(&wr, peer, session.outbound))
    {
        return Err(PeerError::Protocol(
            "state recipient authorization changed".into(),
        ));
    }
    Ok(())
}
#[expect(
    clippy::too_many_lines,
    reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the workspace and authority handles are cloned into the boundary closures that outlive their last named use"
)]
async fn finish_replay_write<W: tokio::io::AsyncWrite + Unpin>(
    batch: ReplayWrite,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    pending: &std::sync::atomic::AtomicUsize,
    private_rx: &mut mpsc::Receiver<PrivateOutbound>,
    write: &mut W,
) -> Result<(), PeerError> {
    let mut reply = batch.reply;
    let mut completed_replay = None;
    if let Some((workspace_id, ticket, mut handle, catchup)) = batch.ticket {
        let ws = session
            .workspaces
            .lock()
            .await
            .get(&workspace_id)
            .cloned()
            .ok_or_else(|| PeerError::Protocol("replay workspace disappeared".into()))?;
        let owner = crate::persistence::handle(&ws).await;
        let mut written = None;
        let mut boundary_done = false;
        let mut replayed_frames = 0u64;
        loop {
            let authority = ticket.authority.clone();
            let source = StateSource::new(peer, session);
            let preparing = async {
                authorize_replay(&ws, &source, &authority).await?;
                handle.advance(&authority, written.take()).await
            };
            match replay_direct_wait(preparing, peer, session, private_rx, write).await? {
                ReplayAdvance::Preparing => {}
                ReplayAdvance::Batch(mut frames) => {
                    while let Some(frame) = frames.front() {
                        for _ in 0..8 {
                            let Ok(private) = private_rx.try_recv() else {
                                break;
                            };
                            write_private(private, peer, session, write).await?;
                        }
                        authorize_state_frame(frame, peer, session).await?;
                        let source = StateSource::new(peer, session);
                        authorize_replay(&ws, &source, &ticket.authority).await?;
                        write
                            .write_all(&encode_charged(frame, session, false)?)
                            .await?;
                        trace_native_publication(
                            "replay_written",
                            frame,
                            Some(&peer.device_public_key),
                        );
                        // The position follows after `Ready`; a reconnect
                        // before it replays these frames once more.
                        let _ = resume_record(session, workspace_id, [frame]);
                        frames.pop_written();
                    }
                    written = Some(WrittenBatch(frames.number));
                    // No frame or output permit escapes this one outstanding batch.
                    drop(frames);
                    continue;
                }
                ReplayAdvance::Exhausted(summary) => {
                    replayed_frames = replayed_frames.saturating_add(summary.total_frames);
                    let authority = ticket.authority.clone();
                    let source = StateSource::new(peer, session);
                    let workspace = ws.clone();
                    let checking_workspace = ws.clone();
                    let checking_source = source.clone();
                    let cleanup_handle = handle.clone();
                    let cleanup = owner.submit_replay_resumable_wait(
                        false,
                        1,
                        async move {
                            authorize_replay(&workspace, &source, &authority).await?;
                            Ok(authority)
                        },
                        move |state, authority| {
                            state.finish_replay(
                                authority.as_ref().invariant("replay cleanup")?,
                                &cleanup_handle,
                                |store| {
                                    authorize_replay_now(
                                        store,
                                        &checking_workspace,
                                        &checking_source,
                                        authority.as_ref().invariant("replay cleanup")?,
                                    )
                                },
                            )
                        },
                        |result| async { result },
                    );
                    let _ =
                        Box::pin(replay_wait(cleanup, peer, session, private_rx, write)).await?;
                    if !boundary_done {
                        let authority = ticket.authority.clone();
                        let capture = authority.clone();
                        let boundary_authority = authority.clone();
                        let source = StateSource::new(peer, session);
                        let workspace = ws.clone();
                        let boundary_workspace = ws.clone();
                        let boundary_source = source.clone();
                        let checking = catchup.clone();
                        let completing = catchup.clone();
                        let boundary = owner.submit_replay_resumable_wait(
                            true,
                            1,
                            async move {
                                authorize_replay(&workspace, &source, &authority).await?;
                                Ok(())
                            },
                            move |state, _| {
                                if !checking.dirty.load(std::sync::atomic::Ordering::SeqCst) {
                                    return Ok(Step::Done(None));
                                }
                                let step = state.capture_pending_replay(&capture)?;
                                match step {
                                    Step::More => Ok(Step::More),
                                    Step::Done(handle) => Ok(Step::Done(Some(handle))),
                                }
                            },
                            move |result| async move {
                                let result = result?;
                                finish_replay_boundary(
                                    &boundary_workspace,
                                    &boundary_source,
                                    &boundary_authority,
                                    &completing,
                                )
                                .await?;
                                Ok(result)
                            },
                        );
                        let caught_up =
                            Box::pin(replay_wait(boundary, peer, session, private_rx, write))
                                .await?;
                        if let Some(catchup_handle) = caught_up {
                            handle = catchup_handle;
                            written = None;
                            boundary_done = true;
                            continue;
                        }
                    }
                    authorize_replay(&ws, &StateSource::new(peer, session), &ticket.authority)
                        .await?;
                    reply = Some(Envelope::Ready {
                        workspace_id,
                        pending_deltas: u32::try_from(replayed_frames).unwrap_or(u32::MAX),
                    });
                    completed_replay = Some((
                        catchup.clone(),
                        ws.clone(),
                        StateSource::new(peer, session),
                        ticket,
                    ));
                    break;
                }
            }
            written = None;
        }
        // Dropping this ticket schedules specific bounded spool reclamation.
    }
    if let Some(reply) = reply {
        authorize_state_frame(&reply, peer, session).await?;
        if let Some((catchup, workspace, source, ticket)) = &completed_replay {
            #[cfg(test)]
            {
                let root = workspace.lock().await.store.root().to_path_buf();
                let workers = crate::persistence::handle(workspace).await.workers();
                crate::persistence::blocking(workers, move || {
                    crate::store::test_barrier::pause(&root, "replay-before-ready");
                })
                .await?;
            }
            authorize_replay_boundary(workspace, source, &ticket.authority, catchup).await?;
            if !ticket.authority.bound_active() {
                return Err(PeerError::Protocol(
                    "replay session fenced before Ready".into(),
                ));
            }
        }
        write
            .write_all(&encode_charged(&reply, session, false)?)
            .await?;
        if let Envelope::Ready { workspace_id, .. } = &reply
            && let Some(position) = resume_record(session, *workspace_id, [])
        {
            write
                .write_all(&encode_charged(&position, session, false)?)
                .await?;
        }
        if let Some((catchup, workspace, source, ticket)) = completed_replay {
            complete_replay_ready(&workspace, &source, &ticket.authority, &catchup).await?;
        }
    }
    if batch.replaying {
        pending.fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
    }
    Ok(())
}

async fn authorize_replay(
    workspace: &SharedWorkspace,
    source: &StateSource,
    authority: &crate::persistence::ReplayAuthority,
) -> Result<(), StoreError> {
    let runtime = crate::workspace_io::fresh_membership(workspace).await?;
    if !source.authorized(&runtime)
        || !runtime
            .state_sessions
            .get(&source.peer.device_public_key)
            .is_some_and(|session| authority.matches(&session.live))
    {
        return Err(crate::persistence::error(
            "replay subscriber authorization changed",
        ));
    }
    Ok(())
}

async fn finish_replay_boundary(
    workspace: &SharedWorkspace,
    source: &StateSource,
    authority: &crate::persistence::ReplayAuthority,
    catchup: &Arc<ReplayCatchup>,
) -> Result<(), StoreError> {
    authorize_replay(workspace, source, authority).await?;
    let runtime = workspace.lock().await;
    let current = runtime
        .state_sessions
        .get(&source.peer.device_public_key)
        .and_then(|session| session.replay.as_ref())
        .filter(|current| Arc::ptr_eq(current, catchup))
        .ok_or_else(|| crate::persistence::error("replay session boundary changed"))?;
    current
        .phase
        .store(ReplayCatchup::BOUNDARY, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the session is read from the membership guard held for the whole boundary check"
)]
async fn authorize_replay_boundary(
    workspace: &SharedWorkspace,
    source: &StateSource,
    authority: &crate::persistence::ReplayAuthority,
    catchup: &Arc<ReplayCatchup>,
) -> Result<(), StoreError> {
    authorize_replay(workspace, source, authority).await?;
    let runtime = workspace.lock().await;
    let session = runtime
        .state_sessions
        .get(&source.peer.device_public_key)
        .filter(|session| authority.matches(&session.live))
        .filter(|session| {
            session
                .replay
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, catchup))
        })
        .ok_or_else(|| crate::persistence::error("replay session boundary changed"))?;
    if session
        .replay
        .as_ref()
        .invariant("replay missing")?
        .phase
        .load(std::sync::atomic::Ordering::SeqCst)
        != ReplayCatchup::BOUNDARY
    {
        return Err(crate::persistence::error(
            "replay session boundary not ready",
        ));
    }
    Ok(())
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the session is read from the membership guard held until readiness is recorded"
)]
async fn complete_replay_ready(
    workspace: &SharedWorkspace,
    source: &StateSource,
    authority: &crate::persistence::ReplayAuthority,
    catchup: &Arc<ReplayCatchup>,
) -> Result<(), StoreError> {
    let runtime = workspace.lock().await;
    let session = runtime
        .state_sessions
        .get(&source.peer.device_public_key)
        .filter(|session| authority.matches(&session.live))
        .filter(|session| {
            session
                .replay
                .as_ref()
                .is_some_and(|current| Arc::ptr_eq(current, catchup))
        })
        .ok_or_else(|| crate::persistence::error("replay session fenced during Ready"))?;
    let replay = session.replay.as_ref().invariant("replay missing")?;
    if replay
        .phase
        .compare_exchange(
            ReplayCatchup::BOUNDARY,
            ReplayCatchup::READY,
            std::sync::atomic::Ordering::SeqCst,
            std::sync::atomic::Ordering::SeqCst,
        )
        .is_err()
    {
        return Err(crate::persistence::error(
            "replay session boundary changed during Ready",
        ));
    }
    Ok(())
}
/// Blocking-worker authority check, immediately before one exact cleanup entry.
/// Disk I/O precedes the short routing check; no routing guard crosses I/O.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the runtime guard is read by every membership condition below; clippy misreads its last use"
)]
fn authorize_replay_now(
    store: &crate::store::WorkspaceStore,
    workspace: &SharedWorkspace,
    source: &StateSource,
    authority: &crate::persistence::ReplayAuthority,
) -> Result<(), StoreError> {
    // The in-memory set while the membership watch vouches for it; otherwise
    // the file, as a fresh membership reload would read it.
    let current = workspace.blocking_lock().membership_is_current();
    let members = if current {
        None
    } else {
        Some(store.load_membership()?)
    };
    let runtime = workspace.blocking_lock();
    let room = members.as_deref().map_or_else(
        || room_access::Room::current(&runtime),
        |listed| room_access::Room::with_listed(&runtime, listed),
    );
    let member = room_access::decide_in(
        room,
        &source.trust(),
        &source.peer,
        RoomOperation::Replay,
        unix_now_ns(),
    )
    .admitted();
    if !member
        || !source.authorized(&runtime)
        || !runtime
            .state_sessions
            .get(&source.peer.device_public_key)
            .is_some_and(|session| authority.matches(&session.live))
    {
        return Err(crate::persistence::error(
            "replay cleanup authority changed",
        ));
    }
    Ok(())
}
async fn replay_wait<T, W: tokio::io::AsyncWrite + Unpin>(
    admission: impl std::future::Future<
        Output = Result<tokio::sync::oneshot::Receiver<Result<T, StoreError>>, StoreError>,
    >,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    private_rx: &mut mpsc::Receiver<PrivateOutbound>,
    write: &mut W,
) -> Result<T, PeerError> {
    // Both bounded admission and durable completion must leave the private
    // writer live; awaiting admission before this select would stall replies.
    let completed = async {
        let receipt = admission.await?;
        crate::persistence::receipt(receipt).await
    };
    replay_direct_wait(completed, peer, session, private_rx, write).await
}

async fn replay_direct_wait<T, W: tokio::io::AsyncWrite + Unpin>(
    operation: impl std::future::Future<Output = Result<T, StoreError>>,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    private_rx: &mut mpsc::Receiver<PrivateOutbound>,
    write: &mut W,
) -> Result<T, PeerError> {
    tokio::pin!(operation);
    let mut private_open = session.framing.is_v2();
    loop {
        tokio::select! {
            biased;
            result = &mut operation => return Ok(result?),
            private = private_rx.recv(), if private_open => {
                match private {
                    Some(private) => {
                        let writing = write_private(private, peer, session, write);
                        tokio::pin!(writing);
                        let mut completed = None;
                        loop {
                            tokio::select! {
                                biased;
                                result = &mut writing => {
                                    result?;
                                    if let Some(result) = completed {
                                        return Ok(result?);
                                    }
                                    break;
                                }
                                result = &mut operation, if completed.is_none() => {
                                    // Finish the active private wire frame before
                                    // propagating either replay success or failure.
                                    completed = Some(result);
                                }
                            }
                        }
                    }
                    None => private_open = false,
                }
            }
        }
    }
}

/// Membership + revocation re-check for every workspace this peer is
/// subscribed to, run when [`AccessAudit`] says. Reloads membership
/// from disk when the membership watch saw it change, or on every audit
/// while no live watch covers the room, and ends the session if the
/// device's own cert was revoked or it fell out of membership.
///
/// Returns whether every subscribed room's membership is change-notified,
/// so the next audit can wait for a change instead of the disk.
async fn audit_subscriber_access(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
) -> Result<bool, PeerError> {
    let mut watched = true;
    // Relay-wide first, so a revoked or untrusted device ends even with no
    // bound room left to ask.
    session
        .trust()
        .cert(peer, unix_now_ns())
        .map_err(|reason| PeerError::Protocol(reason.reason().into()))?;
    for workspace_id in subscribed {
        let Some(workspace) = session.workspaces.lock().await.get(workspace_id).cloned() else {
            continue;
        };
        let workspace = crate::workspace_io::fresh_membership(&workspace).await?;
        watched &= workspace.membership_is_current();
        let current = current_session(&workspace, peer, session.outbound);
        let decision = session.decide(&workspace, peer, RoomOperation::Subscribe);
        drop(workspace);
        if !current {
            return Err(PeerError::Protocol(
                "subscriber authorization changed".into(),
            ));
        }
        // A live subscription was enrolled when it subscribed: a device the
        // room would only enrol now is no longer a member.
        match decision {
            Decision::Admit => {}
            Decision::AdmitAndEnrol | Decision::Deny(room_access::DenyReason::NotAMember) => {
                return Err(PeerError::Protocol(
                    "device was removed from workspace membership".into(),
                ));
            }
            Decision::Deny(reason) => return Err(PeerError::Protocol(reason.reason().into())),
        }
    }
    Ok(watched)
}

/// Handle one successfully-decoded inbound envelope: dispatch to
/// [`handle_envelope`], then write any replay backlog to the wire —
/// followed by the direct reply, if any. Pending on-disk backlog
/// files are cleaned up only after being written, so a write failure
/// leaves them intact for the next reconnect.
#[cfg(test)]
async fn handle_inbound_frame<W>(
    env: Envelope,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &mut Vec<u64>,
    write: &mut W,
) -> Result<(), PeerError>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    let replaying = matches!(
        env,
        Envelope::Subscribe { .. } | Envelope::SubscribeFrom { .. }
    );
    let mut ticket = None;
    let reply = handle_envelope(peer, env, session, subscribed, &mut ticket).await?;
    let pending = std::sync::atomic::AtomicUsize::new(usize::from(replaying));
    let (_tx, mut private_rx) = mpsc::channel(1);
    Box::pin(finish_replay_write(
        ReplayWrite {
            ticket,
            reply,
            replaying,
        },
        peer,
        session,
        &pending,
        &mut private_rx,
        write,
    ))
    .await
}

async fn handle_envelope(
    peer: &AuthenticatedPeer,
    env: Envelope,
    session: &SessionCtx<'_>,
    subscribed: &mut Vec<u64>,
    replay: &mut Option<(u64, ReplayTicket, ReplayHandle, Arc<ReplayCatchup>)>,
) -> Result<Option<Envelope>, PeerError> {
    match env {
        Envelope::Subscribe { workspace_id } => {
            handle_subscribe(
                peer,
                session,
                subscribed,
                replay,
                workspace_id,
                Subscription::Whole,
            )
            .await
        }
        Envelope::SubscribeFrom {
            workspace_id,
            position,
        } => {
            handle_subscribe(
                peer,
                session,
                subscribed,
                replay,
                workspace_id,
                Subscription::From(position),
            )
            .await
        }
        Envelope::Unsubscribe { workspace_id } => {
            handle_unsubscribe(peer, session, subscribed, workspace_id).await
        }
        env @ Envelope::Delta { .. } => handle_delta(peer, session, subscribed, env).await,
        env @ Envelope::Presence { .. } => handle_presence(peer, session, subscribed, env).await,
        Envelope::PeerRelayHello { hosted_workspaces } => {
            handle_peer_relay_hello(peer, session, hosted_workspaces).await
        }
        Envelope::SyncRequest {
            workspace_id,
            have_heads,
        } => handle_sync_request(peer, session, workspace_id, have_heads).await,
        Envelope::Ping => Ok(Some(Envelope::Pong)),
        Envelope::RemoteAction {
            workspace_id,
            version,
            to_device_public_key,
            frame,
            ..
        } => Ok(handle_remote_action(
            peer,
            session,
            subscribed,
            RemoteActionRoute {
                workspace_id,
                version,
                to: to_device_public_key,
            },
            frame,
        )
        .await),
        env @ Envelope::RecordContent { .. } => {
            route_record_content(peer, session, subscribed, env).await;
            Ok(None)
        }
        env @ Envelope::Commit { .. } => handle_commit(peer, session, subscribed, env).await,
        Envelope::LiveJoin { workspace_id } => {
            handle_live_join(peer, session, subscribed, workspace_id).await
        }
        Envelope::LiveLeave { workspace_id } => {
            handle_live_leave(peer, session, workspace_id).await
        }
        Envelope::Live {
            workspace_id,
            frame,
            ..
        } => handle_live(peer, session, subscribed, workspace_id, frame).await,
        env @ (Envelope::CompactVolunteer { .. } | Envelope::Compacted { .. }) => {
            handle_compaction(peer, session, subscribed, env).await;
            Ok(None)
        }
        env @ (Envelope::VoiceCall { .. }
        | Envelope::VoiceJoin { .. }
        | Envelope::VoiceLeave { .. }
        | Envelope::VoiceCalls { .. }) => handle_voice(peer, session, subscribed, env).await,
        // Envelopes the relay only *sends*, never receives; ignore
        // silently so a peer bug doesn't wedge the connection.
        Envelope::CompactRequest { .. }
        | Envelope::Ready { .. }
        | Envelope::NotAuthorised { .. }
        | Envelope::Pong
        | Envelope::SyncResponse { .. }
        | Envelope::RemoteActionUndeliverable { .. }
        | Envelope::Committed { .. }
        | Envelope::CommitRefused { .. }
        | Envelope::LiveMember { .. }
        | Envelope::Position { .. }
        | Envelope::VoiceCallResult { .. }
        | Envelope::VoiceGranted { .. }
        | Envelope::VoiceRefused { .. }
        | Envelope::VoiceCallList { .. } => Ok(None),
    }
}

/// How a subscriber's replay capture finished.
enum SubscribeReplayCompletion {
    Ready(ReplayHandle),
    Superseded,
}

/// How a subscription asks for its replay.
enum Subscription {
    /// `Subscribe`: everything, with no positions.
    Whole,
    /// `SubscribeFrom`: what the device has not received since the position.
    From(Option<Vec<u8>>),
}

/// Set up this subscription's resume log. A plain `Subscribe` has none. A
/// `SubscribeFrom` with a position this host issued to this device for this
/// store continues that log and returns the retained frames the device holds,
/// which its replay leaves out; any other `SubscribeFrom` starts a new log
/// and replays everything.
async fn open_resume(
    session: &SessionCtx<'_>,
    peer: &AuthenticatedPeer,
    workspace_id: u64,
    store: crate::store::WorkspaceStore,
    resume: Subscription,
) -> Result<Option<crate::store::replay::ResumeSkip>, PeerError> {
    let (Subscription::From(position), Some(issuer)) = (resume, session.positions) else {
        session
            .resumes
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&workspace_id);
        return Ok(None);
    };
    let device = peer.device_public_key;
    let opened = tokio::task::spawn_blocking(move || {
        let generation = store.generation()?;
        let held = match position
            .as_deref()
            .and_then(|position| issuer.verify(position, workspace_id, &device, &generation))
        {
            Some(resume) => match store.load_resume(&device, &resume.log, resume.len)? {
                Some(held) => store
                    .fork_resume(&device, &resume.log, resume.len)?
                    .map(|log| {
                        (
                            crate::position::Resume {
                                log,
                                len: resume.len,
                            },
                            held,
                        )
                    }),
                None => None,
            },
            None => None,
        };
        let (resume, skip) = match held {
            Some((resume, held)) => (resume, Some(Arc::new(held))),
            None => (
                crate::position::Resume {
                    log: store.start_resume(&device)?,
                    len: 0,
                },
                None,
            ),
        };
        Ok::<_, crate::store::StoreError>((
            ResumeLog {
                store,
                issuer,
                device,
                generation,
                resume,
            },
            skip,
        ))
    })
    .await
    .map_err(|error| PeerError::Io(std::io::Error::other(error)))??;
    let (log, skip) = opened;
    session
        .resumes
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(workspace_id, log);
    Ok(skip)
}

/// Handle `Subscribe`: gate on membership (or admit, for a
/// same-owner autobind), replay retained baselines + this device's
/// pending queue, then register the live subscriber.
#[expect(
    clippy::too_many_lines,
    reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the ticket and replay handle move into the connection's replay slot; the lint does not see the move"
)]
async fn handle_subscribe(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &mut Vec<u64>,
    replay: &mut Option<(u64, ReplayTicket, ReplayHandle, Arc<ReplayCatchup>)>,
    workspace_id: u64,
    resume: Subscription,
) -> Result<Option<Envelope>, PeerError> {
    // An agent outside its delegation never gets a room bound for it.
    if let Err(reason) = session
        .trust()
        .agent(&peer.device_public_key, workspace_id, unix_now_ns())
    {
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            reason: reason.reason().into(),
        }));
    }
    // Only the relay owner's own devices can be enrolled into an autobound
    // room, so only they may cause one to be bound.
    let autobind = session
        .autobind
        .filter(|_| peer.user_public_key == *session.own_user_public_key);
    let ws = get_or_autobind_workspace(session.workspaces, autobind, workspace_id).await?;
    let Some(ws) = ws else {
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            reason: "workspace not bound on this relay".into(),
        }));
    };
    let mut wr = crate::workspace_io::fresh_membership(&ws).await?;
    if !session_peer_authorized(peer, session)
        || (subscribed.contains(&workspace_id) && !current_session(&wr, peer, session.outbound))
    {
        return Err(PeerError::Protocol(
            "subscriber authorization changed".into(),
        ));
    }
    // Same-owner peer relays are gated by their active federation lease, so
    // they don't need to appear in the workspace membership list.
    match session.decide(&wr, peer, RoomOperation::Subscribe) {
        Decision::Admit => {}
        Decision::AdmitAndEnrol => {
            let revision = wr.membership_revision;
            drop(wr);
            crate::workspace_io::admit_member(&ws, peer.device_public_key, revision).await?;
            wr = crate::workspace_io::fresh_membership(&ws).await?;
            if !session.admits(&wr, peer, RoomOperation::Subscribe)
                || (subscribed.contains(&workspace_id)
                    && !current_session(&wr, peer, session.outbound))
            {
                return Err(PeerError::Protocol(
                    "subscriber authorization changed".into(),
                ));
            }
        }
        Decision::Deny(reason) => {
            return Ok(Some(Envelope::NotAuthorised {
                workspace_id,
                reason: reason.reason().into(),
            }));
        }
    }
    let expected = wr
        .subscribers
        .get(&peer.device_public_key)
        .map(|sub| sub.tx.clone());
    let store = wr.store.clone();
    drop(wr);
    let skip = open_resume(session, peer, workspace_id, store, resume).await?;
    let owner = crate::persistence::handle(&ws).await;
    let source = StateSource::new(peer, session);
    let state_tx = session
        .state_outbound
        .cloned()
        .unwrap_or_else(|| mpsc::channel(1).0);
    let completion_workspace = ws.clone();
    let ticket = ReplayTicket::reserve(peer.device_public_key, session.connection_live.cloned())?;
    let capture = ticket.authority.clone();
    let bind = ticket.authority.clone();
    let catchup = Arc::new(ReplayCatchup::new());
    let session_catchup = catchup.clone();
    let session_lag = session.lag.clone();
    let presence = session.presence.cloned();

    let rx = owner
        .submit_replay_resumable_wait(
            true,
            1,
            async { Ok(()) },
            move |state, _| {
                let step = state.capture_resumed_replay(&capture, skip.clone())?;
                match step {
                    Step::More => Ok(Step::More),
                    Step::Done(handle) => Ok(Step::Done(SubscribeReplayCompletion::Ready(handle))),
                }
            },
            move |result| async move {
                let SubscribeReplayCompletion::Ready(handle) = result? else {
                    return Err(crate::persistence::error(
                        "only replay completion can classify a subscriber as superseded",
                    ));
                };
                let mut wr = crate::workspace_io::fresh_membership(&completion_workspace).await?;
                let current = wr.subscribers.get(&source.peer.device_public_key);
                let unchanged = match (expected.as_ref(), current) {
                    (Some(expected), Some(current)) => expected.same_channel(&current.tx),
                    (None, None) => true,
                    // The device's earlier connection held the slot when this
                    // one subscribed, and its teardown has since released it.
                    // This subscription would have displaced it anyway, so it
                    // takes the slot. Only this connection's own slot
                    // vanishing is a loss of authorization.
                    (Some(expected), None) => !expected.same_channel(&source.tx),
                    (None, Some(_)) => false,
                };
                if !source.alive() || !source.admits(&wr, &source.peer) {
                    return Err(crate::persistence::error(
                        "replay subscriber authorization changed",
                    ));
                }
                let replaced = current.is_some_and(|current| !current.tx.same_channel(&source.tx))
                    && !unchanged;
                if replaced {
                    return Ok(SubscribeReplayCompletion::Superseded);
                }
                if !unchanged {
                    return Err(crate::persistence::error(
                        "replay subscriber authorization changed",
                    ));
                }
                let live = wr
                    .subscribers
                    .get(&source.peer.device_public_key)
                    .filter(|current| current.tx.same_channel(&source.tx))
                    .and_then(|_| wr.state_sessions.get(&source.peer.device_public_key))
                    .map_or_else(
                        || Arc::new(std::sync::atomic::AtomicBool::new(true)),
                        |session| session.live.clone(),
                    );
                // Binding may fail when the waiting connection/receipt was dropped.
                // It must precede every registration side effect.
                bind.bind(live.clone())?;
                wr.add_subscriber(Subscriber {
                    device_public_key: source.peer.device_public_key,
                    role: source.peer.role,
                    tx: source.tx,
                    disconnect: source.disconnect,
                });
                if let Some(mailbox) = presence {
                    wr.route_presence(source.peer.device_public_key, mailbox);
                }
                wr.state_sessions.insert(
                    source.peer.device_public_key,
                    StateSession {
                        live,
                        peer: source.peer,
                        tx: state_tx,
                        replay: Some(session_catchup),
                        lag: Some(session_lag),
                    },
                );
                drop(wr);
                Ok(SubscribeReplayCompletion::Ready(handle))
            },
        )
        .await?;
    let completion = crate::persistence::receipt(rx).await?;
    let handle = match completion {
        SubscribeReplayCompletion::Ready(handle) => handle,
        SubscribeReplayCompletion::Superseded => return Err(PeerError::Superseded),
    };
    *replay = Some((workspace_id, ticket, handle, catchup));
    if !subscribed.contains(&workspace_id) {
        subscribed.push(workspace_id);
    }
    Ok(None)
}

/// Handle `Unsubscribe`: drop the live subscriber (if the workspace
/// is known) and stop tracking it for this connection's teardown.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the flagged value is a shared workspace handle (an Arc clone); dropping it earlier releases no lock"
)]
async fn handle_unsubscribe(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &mut Vec<u64>,
    workspace_id: u64,
) -> Result<Option<Envelope>, PeerError> {
    let ws = {
        let map = session.workspaces.lock().await;
        map.get(&workspace_id).cloned()
    };
    if let Some(ws) = ws {
        ws.lock()
            .await
            .remove_subscriber_session(&peer.device_public_key, session.outbound);
    }
    subscribed.retain(|id| id != &workspace_id);
    Ok(None)
}

/// Handle an inbound `Delta`: validate the author, fan out to every
/// other subscriber via the same path a federated peer relay uses,
/// and account it in the metrics.
#[derive(Clone)]
pub(crate) struct StateSession {
    pub(crate) peer: AuthenticatedPeer,
    pub(crate) tx: mpsc::Sender<StateDelivery>,
    pub(crate) live: Arc<std::sync::atomic::AtomicBool>,
    replay: Option<Arc<ReplayCatchup>>,
    lag: Option<Arc<Lag>>,
}
impl StateSession {
    /// Everything fanned out to this session so far reached its wire: none
    /// of it is queued, left on disk while it lagged, or still replaying.
    pub(crate) fn caught_up(&self) -> bool {
        self.tx.capacity() == self.tx.max_capacity()
            && self.lag.as_ref().is_none_or(|lag| lag.is_empty())
            && self.replay.as_ref().is_none_or(|replay| {
                replay.phase.load(std::sync::atomic::Ordering::SeqCst) != ReplayCatchup::ACTIVE
            })
    }
}
/// The rooms whose live deliveries a connection could not take and left on
/// disk (its pending queue and the retained log). Once the connection's
/// queues drain it resumes each of them from its position, so the replay
/// carries exactly what it missed; a lagging reader costs disk and latency,
/// never its session. A subscription without a resume log catches up through
/// its ordinary full retained replay, with current authorization checked again.
#[derive(Default)]
pub(crate) struct Lag {
    rooms: std::sync::Mutex<std::collections::BTreeSet<u64>>,
    wake: tokio::sync::Notify,
}
impl Lag {
    fn mark(&self, workspace_id: u64) {
        let first = self
            .rooms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(workspace_id);
        if first {
            self.wake.notify_one();
        }
    }

    fn next(&self) -> Option<u64> {
        self.rooms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pop_first()
    }

    fn is_empty(&self) -> bool {
        self.rooms
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_empty()
    }
}

struct ReplayCatchup {
    phase: std::sync::atomic::AtomicU8,
    dirty: std::sync::atomic::AtomicBool,
}
impl ReplayCatchup {
    const ACTIVE: u8 = 0;
    const BOUNDARY: u8 = 1;
    const READY: u8 = 2;

    fn new() -> Self {
        Self {
            phase: std::sync::atomic::AtomicU8::new(Self::ACTIVE),
            dirty: std::sync::atomic::AtomicBool::new(false),
        }
    }
}
#[derive(Clone)]
pub(crate) struct StateSource {
    peer: AuthenticatedPeer,
    tx: mpsc::Sender<Envelope>,
    disconnect: mpsc::Sender<()>,
    revocations: Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
    policy: Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    own_user: [u8; 32],
    agents: AgentRegistry,
    federation: Option<Arc<std::sync::atomic::AtomicBool>>,
    session_live: Option<Arc<std::sync::atomic::AtomicBool>>,
}
impl StateSource {
    /// The source of an outbound federation lease. Its trust is the
    /// relay's own user only — not the live handshake policy, which may admit
    /// collaborators' roots — and it carries no agent delegations: a peer
    /// relay is never an agent.
    pub(crate) fn federated(
        peer: AuthenticatedPeer,
        tx: mpsc::Sender<Envelope>,
        disconnect: mpsc::Sender<()>,
        revocations: Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
        own_user: [u8; 32],
        lease: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            peer,
            tx,
            disconnect,
            revocations,
            policy: Arc::new(std::sync::RwLock::new(
                crate::policy::TrustPolicy::AllowMatchingUser,
            )),
            own_user,
            agents: AgentRegistry::default(),
            federation: Some(lease),
            session_live: None,
        }
    }
    fn alive(&self) -> bool {
        !self.tx.is_closed()
            && self
                .session_live
                .as_ref()
                .is_none_or(|live| live.load(std::sync::atomic::Ordering::SeqCst))
    }
    pub(crate) fn failed(&self) {
        let _ = self.disconnect.try_send(());
    }
    fn new(peer: &AuthenticatedPeer, session: &SessionCtx<'_>) -> Self {
        Self {
            peer: peer.clone(),
            tx: session.outbound.clone(),
            disconnect: session.disconnect.clone(),
            revocations: session.revocations.clone(),
            policy: session.trust_policy.clone(),
            own_user: *session.own_user_public_key,
            agents: session.agents.clone(),
            federation: None,
            session_live: session.connection_live.cloned(),
        }
    }
    fn trust(&self) -> RelayTrust<'_> {
        RelayTrust {
            revocations: &self.revocations,
            policy: &self.policy,
            own_user: &self.own_user,
            agents: &self.agents,
        }
    }
    /// Whether `peer` may hold state in `runtime`'s room under this source's
    /// trust: the source itself, or a recipient of its frames.
    fn admits(
        &self,
        runtime: &crate::workspace::WorkspaceRuntime,
        peer: &AuthenticatedPeer,
    ) -> bool {
        room_access::decide(runtime, &self.trust(), peer, RoomOperation::State).admitted()
    }
    pub(crate) fn authorized(&self, runtime: &crate::workspace::WorkspaceRuntime) -> bool {
        self.alive()
            && self.admits(runtime, &self.peer)
            && match &self.federation {
                Some(lease) => {
                    lease.load(std::sync::atomic::Ordering::SeqCst)
                        && runtime
                            .outbound_federation
                            .get(&self.peer.device_public_key)
                            .is_some_and(|current| Arc::ptr_eq(current, lease))
                }
                None => current_session(runtime, &self.peer, &self.tx),
            }
    }
}

#[derive(Clone)]
enum LiveEnvelope {
    Original(Arc<Envelope>),
    Selected(Arc<crate::store::replay::ReplayBatch>),
}
// Return only channels subject to the ordinary once-per-request live gate.
// This is delivery policy, not authority: callers must first revalidate the
// current authenticated source and retain all recipient/session fences.
fn gated_checkpoint_channel(
    envelope: &LiveEnvelope,
    source: Option<&StateSource>,
) -> Option<[u8; 32]> {
    let LiveEnvelope::Original(original) = envelope else {
        return None;
    };
    let Envelope::Delta { delta, .. } = &**original else {
        return None;
    };
    let (payload, outer_trailing) =
        postcard::take_from_bytes::<crate::coedit::CoeditPayload>(delta).ok()?;
    let crate::coedit::CoeditPayload::Checkpoint { channel, body } = payload else {
        return None;
    };
    // Unknown wrapper suffixes retain the generic checkpoint gate.
    if outer_trailing.is_empty()
        && let Some(source) = source
        && let Ok((msg, trailing)) =
            postcard::take_from_bytes::<tilo_peer_state::NeighbourMsg>(&body)
        && trailing.is_empty()
        && matches!(
            &msg,
            tilo_peer_state::NeighbourMsg::PersonalContinuityCheckpoint {
                user_public_key, ..
            } if *user_public_key == source.peer.user_public_key
        )
        && tilo_peer_state::transport::retention(&msg, false)
            == Some((channel, tilo_peer_state::transport::Retention::Checkpoint))
    {
        return None;
    }
    Some(channel)
}

impl std::ops::Deref for LiveEnvelope {
    type Target = Envelope;
    #[expect(
        clippy::expect_used,
        reason = "a Selected envelope is only built from a non-empty activation batch"
    )]
    fn deref(&self) -> &Envelope {
        match self {
            Self::Original(envelope) => envelope,
            Self::Selected(batch) => batch.front().expect("one activation frame"),
        }
    }
}
pub(crate) struct StateDelivery {
    envelope: LiveEnvelope,
    cleanup: Option<std::path::PathBuf>,
    source: Option<StateSource>,
    _budget: Option<Arc<tokio::sync::OwnedSemaphorePermit>>,
    live: Arc<std::sync::atomic::AtomicBool>,
}
impl StateDelivery {
    fn current_subscription(
        &self,
        runtime: &crate::workspace::WorkspaceRuntime,
        peer: &AuthenticatedPeer,
        outbound: &mpsc::Sender<Envelope>,
    ) -> bool {
        self.live.load(std::sync::atomic::Ordering::SeqCst)
            && current_session(runtime, peer, outbound)
            && runtime
                .state_sessions
                .get(&peer.device_public_key)
                .is_some_and(|current| Arc::ptr_eq(&current.live, &self.live))
    }
}

#[derive(Clone)]
enum SelectedPublication {
    Activation(std::path::PathBuf),
    Recovery(std::path::PathBuf),
}
impl SelectedPublication {
    /// The selected publication a store outcome names, if it names one.
    fn from_publication(publication: crate::store::shell_selection::Publication) -> Option<Self> {
        match publication {
            crate::store::shell_selection::Publication::Activate(path) => {
                Some(Self::Activation(path))
            }
            crate::store::shell_selection::Publication::ReplayRequired(path) => {
                Some(Self::Recovery(path))
            }
            crate::store::shell_selection::Publication::Original
            | crate::store::shell_selection::Publication::Suppress => None,
        }
    }
    fn open(&self) -> Result<crate::store::replay::PublicationCursor, crate::store::StoreError> {
        match self {
            Self::Activation(path) => crate::store::replay::PublicationCursor::open(path),
            Self::Recovery(path) => crate::store::replay::PublicationCursor::open_recovery(path),
        }
    }
}
#[derive(Clone)]
struct SessionLease {
    device: [u8; 32],
    outbound: mpsc::Sender<Envelope>,
    live: Arc<std::sync::atomic::AtomicBool>,
}
type SessionLives = Vec<SessionLease>;
fn captured_live_current(
    lives: &SessionLives,
    runtime: &crate::workspace::WorkspaceRuntime,
    device: &[u8; 32],
    outbound: &mpsc::Sender<Envelope>,
) -> bool {
    lives.iter().any(|lease| {
        lease.device == *device
            && lease.outbound.same_channel(outbound)
            && lease.live.load(std::sync::atomic::Ordering::SeqCst)
            && runtime
                .state_sessions
                .get(device)
                .is_some_and(|session| Arc::ptr_eq(&lease.live, &session.live))
    })
}
fn union_session_lives<'a>(
    prepared: impl IntoIterator<Item = &'a PreparedRecipients>,
) -> SessionLives {
    let mut result: SessionLives = Vec::new();
    for item in prepared {
        for lease in &item.session_lives {
            if !result.iter().any(|current| {
                current.device == lease.device
                    && current.outbound.same_channel(&lease.outbound)
                    && Arc::ptr_eq(&current.live, &lease.live)
            }) {
                result.push(lease.clone());
            }
        }
    }
    result
}
struct StateCommit {
    continuity: Option<crate::store::continuity::Receipt>,
    envelope: Arc<Envelope>,
    pending: HashMap<[u8; 32], std::path::PathBuf>,
    persisted_bytes: u64,
    sessions: Vec<([u8; 32], mpsc::Sender<Envelope>)>,
    session_lives: SessionLives,
    logical_offline_bytes: u64,
    publish: bool,
    selected: Option<SelectedPublication>,
    recovery_scope: bool,
    activation_error: Option<crate::store::StoreError>,
    replay_targets: Vec<ReplayPendingTarget>,
    accepted_deltas: u64,
}

struct PreparedRecipients {
    session_lives: SessionLives,
    sessions: Vec<([u8; 32], mpsc::Sender<Envelope>)>,
    replay_targets: Vec<ReplayPendingTarget>,
    logical_offline_bytes: u64,
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the membership guard is read by the iterator closures until the recipient snapshot is complete"
)]
async fn prepare_recipients(
    workspace: &SharedWorkspace,
    source: &StateSource,
    author: [u8; 32],
    role: crate::auth::PeerRole,
    delta_len: usize,
) -> Result<PreparedRecipients, crate::store::StoreError> {
    let runtime = crate::workspace_io::fresh_membership(workspace).await?;
    if !source.authorized(&runtime) {
        return Err(crate::persistence::error(
            "state source authorization changed",
        ));
    }
    let mut recipients = runtime
        .membership
        .devices
        .iter()
        .copied()
        .filter(|key| key != &author)
        .collect::<std::collections::HashSet<_>>();
    if role == crate::auth::PeerRole::Device {
        recipients.extend(
            runtime
                .known_peer_relays
                .iter()
                .copied()
                .filter(|key| key != &author),
        );
    }
    let sessions = runtime
        .subscribers
        .iter()
        .filter(|(key, sub)| {
            **key != author
                && !(role == crate::auth::PeerRole::PeerRelay
                    && sub.role == crate::auth::PeerRole::PeerRelay)
        })
        .map(|(key, sub)| (*key, sub.tx.clone()))
        .collect::<Vec<_>>();
    let session_lives = sessions
        .iter()
        .filter_map(|(device, tx)| {
            runtime
                .state_sessions
                .get(device)
                .map(|session| SessionLease {
                    device: *device,
                    outbound: tx.clone(),
                    live: session.live.clone(),
                })
        })
        .collect();
    let replay_targets = sessions
        .iter()
        .filter_map(|(device, _)| {
            let session = runtime.state_sessions.get(device)?;
            let catchup = session.replay.as_ref()?.clone();
            (session.live.load(std::sync::atomic::Ordering::SeqCst)
                && catchup.phase.load(std::sync::atomic::Ordering::SeqCst) == ReplayCatchup::ACTIVE)
                .then(|| ReplayPendingTarget {
                    device: *device,
                    live: session.live.clone(),
                    catchup,
                })
        })
        .collect::<Vec<_>>();
    recipients.extend(sessions.iter().map(|(key, _)| *key));
    if role == crate::auth::PeerRole::PeerRelay {
        recipients.retain(|key| !runtime.known_peer_relays.contains(key));
    }
    let logical_offline_bytes = (delta_len as u64).saturating_mul(
        recipients
            .iter()
            .filter(|key| !runtime.subscribers.contains_key(*key))
            .count() as u64,
    );
    Ok(PreparedRecipients {
        session_lives,
        sessions,
        replay_targets,
        logical_offline_bytes,
    })
}

#[derive(Clone)]
struct ReplayPendingTarget {
    device: [u8; 32],
    live: Arc<std::sync::atomic::AtomicBool>,
    catchup: Arc<ReplayCatchup>,
}
impl ReplayPendingTarget {
    fn active(&self) -> bool {
        self.live.load(std::sync::atomic::Ordering::SeqCst)
            && self.catchup.phase.load(std::sync::atomic::Ordering::SeqCst) == ReplayCatchup::ACTIVE
    }
    fn current(&self, runtime: &crate::workspace::WorkspaceRuntime) -> bool {
        runtime
            .state_sessions
            .get(&self.device)
            .is_some_and(|session| {
                Arc::ptr_eq(&session.live, &self.live)
                    && session
                        .replay
                        .as_ref()
                        .is_some_and(|catchup| Arc::ptr_eq(catchup, &self.catchup))
            })
    }
}

enum ReplayPendingPublication {
    Original(Option<Arc<Envelope>>),
    Selected(crate::store::replay::PublicationCursor),
}
struct ReplayPendingFrame {
    bytes: Vec<u8>,
    _selected: Option<crate::store::replay::ReplayBatch>,
    sequence: Option<u64>,
    next_target: usize,
}
struct ReplayPendingWork {
    continuity: Option<crate::store::continuity::Receipt>,
    publication: ReplayPendingPublication,
    targets: Vec<ReplayPendingTarget>,
    frame: Option<ReplayPendingFrame>,
    written: u64,
}
impl ReplayPendingWork {
    fn original(
        envelope: Arc<Envelope>,
        targets: Vec<ReplayPendingTarget>,
        continuity: Option<crate::store::continuity::Receipt>,
    ) -> Self {
        Self {
            continuity,
            publication: ReplayPendingPublication::Original(Some(envelope)),
            targets,
            frame: None,
            written: 0,
        }
    }
    fn selected(
        publication: &SelectedPublication,
        targets: Vec<ReplayPendingTarget>,
    ) -> Result<Self, crate::store::StoreError> {
        Ok(Self {
            continuity: None,
            publication: ReplayPendingPublication::Selected(publication.open()?),
            targets,
            frame: None,
            written: 0,
        })
    }
    fn step(
        &mut self,
        state: &mut crate::persistence::State,
        budget: &crate::store::replay::ReplayBudget,
    ) -> Result<Step<u64>, crate::store::StoreError> {
        for _ in 0..crate::store::bounded::RUN_ENTRIES {
            if self.frame.is_none() {
                let (bytes, selected) = match &mut self.publication {
                    ReplayPendingPublication::Original(envelope) => envelope
                        .take()
                        .map(|envelope| crate::transport::encode(&envelope))
                        .transpose()
                        .map(|bytes| (bytes, None))
                        .map_err(|error| crate::persistence::error(error.to_string()))?,
                    ReplayPendingPublication::Selected(cursor) => match cursor.step(budget)? {
                        Step::More => continue,
                        Step::Done(Some(batch)) => {
                            let bytes = crate::transport::encode(
                                batch.front().invariant("one activation frame")?,
                            )
                            .map_err(|error| crate::persistence::error(error.to_string()))?;
                            (Some(bytes), Some(batch))
                        }
                        Step::Done(None) => (None, None),
                    },
                };
                let Some(bytes) = bytes else {
                    return Ok(Step::Done(self.written));
                };
                self.frame = Some(ReplayPendingFrame {
                    bytes,
                    _selected: selected,
                    sequence: None,
                    next_target: 0,
                });
            }
            let frame = self.frame.as_mut().invariant("frame missing")?;
            let Some(target) = self.targets.get(frame.next_target) else {
                self.frame = None;
                continue;
            };
            frame.next_target = frame.next_target.saturating_add(1);
            if !target.active() {
                continue;
            }
            let sequence = if let Some(sequence) = frame.sequence {
                sequence
            } else {
                let sequence = match &self.continuity {
                    Some(receipt) => receipt.sequence,
                    None => state.next_sequence()?,
                };
                frame.sequence = Some(sequence);
                sequence
            };
            if let Some(receipt) = &self.continuity {
                state
                    .store
                    .enqueue_continuity_pending(&target.device, &frame.bytes, receipt)?;
            } else {
                state
                    .store
                    .enqueue_pending(&target.device, sequence, &frame.bytes)?;
            }
            self.written = self.written.saturating_add(frame.bytes.len() as u64);
        }
        #[cfg(test)]
        if self.frame.is_some() {
            crate::store::test_barrier::pause(state.store.root(), "replay-pending-frame-held");
        }
        Ok(Step::More)
    }
}

struct CheckpointBatchInput {
    continuity: Option<crate::store::continuity::Receipt>,
    envelope: Arc<Envelope>,
    raw: Vec<u8>,
    channel: [u8; 32],
    incoming: [u8; 32],
    stage: Option<crate::store::retained::CheckpointStage>,
    prepared: Option<PreparedRecipients>,
    sequence: Option<u64>,
    cleanup_registered: bool,
}

struct CheckpointBatch {
    root: std::path::PathBuf,
    inputs: Vec<CheckpointBatchInput>,
    selector: Option<crate::store::shell_selection::Selection>,
    projected: Option<crate::store::shell_selection::BatchSelectionOutcome>,
    stage_index: usize,
    all_projectable: bool,
}

fn new_checkpoint_batch(
    state: &mut crate::persistence::State,
    envelopes: Vec<Envelope>,
    peer: Option<&AuthenticatedPeer>,
) -> Result<CheckpointBatch, crate::store::StoreError> {
    let root = state.store.root().join("store-work-v1").join(format!(
        "retain-{}",
        hex::encode(rand::random::<[u8; 16]>())
    ));
    std::fs::create_dir(&root)?;
    state.retention_scratch = Some(root.clone());
    let mut inputs = Vec::with_capacity(envelopes.len());
    for (index, envelope) in envelopes.into_iter().enumerate() {
        let Envelope::Delta { delta, .. } = &envelope else {
            return Err(crate::persistence::error(
                "checkpoint batch contained non-delta",
            ));
        };
        let Some((channel, crate::coedit::RetentionKind::Checkpoint)) =
            crate::coedit::retention_metadata(delta)
        else {
            return Err(crate::persistence::error(
                "checkpoint batch contained non-checkpoint",
            ));
        };
        let raw = crate::transport::encode(&envelope)
            .map_err(|error| crate::persistence::error(error.to_string()))?;
        let incoming =
            crate::store::shell_selection::incoming_identity(&raw, state.store.workspace_id())
                .ok_or_else(|| crate::persistence::error("invalid checkpoint identity"))?;
        // Reservation is speculative until fresh authorization permits promotion.
        // Keep the same ordered receipt through projection and serial fallback.
        let sequence = state.next_sequence()?;
        let continuity = peer
            .and_then(|peer| crate::store::continuity::Evidence::authenticated(&envelope, peer))
            .map(|evidence| evidence.accept(sequence));
        let checkpoint_stage = if state.store.checkpoint_lineage_exists(&channel, &raw)? {
            Some(
                state
                    .store
                    .start_staged_checkpoint(
                        &channel,
                        raw.clone(),
                        root.join(format!("input-{index:02}")),
                    )?
                    .with_continuity(continuity.clone()),
            )
        } else {
            None
        };
        inputs.push(CheckpointBatchInput {
            continuity,
            envelope: Arc::new(envelope),
            raw,
            channel,
            incoming,
            stage: checkpoint_stage,
            prepared: None,
            sequence: Some(sequence),
            cleanup_registered: false,
        });
    }
    Ok(CheckpointBatch {
        root,
        inputs,
        selector: None,
        projected: None,
        stage_index: 0,
        all_projectable: true,
    })
}

fn step_checkpoint_batch(
    state: &mut crate::persistence::State,
    batch: &mut CheckpointBatch,
) -> Result<Step<()>, crate::store::StoreError> {
    #[cfg(test)]
    let _progress = checkpoint_progress_step(batch);
    if let Some(input) = batch.inputs.get_mut(batch.stage_index) {
        let Some(checkpoint_stage) = input.stage.as_mut() else {
            batch.all_projectable = false;
            batch.stage_index = batch.stage_index.saturating_add(1);
            return Ok(Step::More);
        };
        let result = checkpoint_stage.step(WorkLimit::default());
        if !input.cleanup_registered
            && let Some(paths) = checkpoint_stage.cleanup_paths()
        {
            state.retention_candidates.push(paths);
            input.cleanup_registered = true;
        }
        match result? {
            Step::More => return Ok(Step::More),
            Step::Done(eligible) => {
                batch.all_projectable &= eligible;
                batch.stage_index = batch.stage_index.saturating_add(1);
                return Ok(Step::More);
            }
        }
    }
    if batch.selector.is_none() && batch.all_projectable {
        let overrides = batch
            .inputs
            .iter()
            .map(|input| {
                let checkpoint_stage = input.stage.as_ref().invariant("stage missing")?;
                Ok((
                    checkpoint_stage.lineage_name()?.to_os_string(),
                    checkpoint_stage.projected_generation().to_path_buf(),
                ))
            })
            .collect::<Result<_, crate::store::StoreError>>()?;
        let incoming = batch.inputs.iter().map(|input| input.incoming).collect();
        let selector = crate::store::shell_selection::Selection::new_projected(
            &state.store,
            batch.root.join("selection"),
            incoming,
            overrides,
        )?;
        state.retention_candidates.push(selector.cleanup_paths());
        batch.selector = Some(selector);
    }
    let Some(selector) = batch.selector.as_mut() else {
        return Ok(Step::Done(()));
    };
    match selector.step_projection(WorkLimit::default())? {
        Step::More => Ok(Step::More),
        Step::Done(projected) => {
            batch.projected = Some(projected);
            Ok(Step::Done(()))
        }
    }
}

/// Live recipient sessions and replay targets gathered from prepared recipients.
type UnionSessions = (
    Vec<([u8; 32], mpsc::Sender<Envelope>)>,
    Vec<ReplayPendingTarget>,
);

fn union_sessions<'a>(prepared: impl IntoIterator<Item = &'a PreparedRecipients>) -> UnionSessions {
    let mut sessions: Vec<([u8; 32], mpsc::Sender<Envelope>)> = Vec::new();
    let mut replay_targets: Vec<ReplayPendingTarget> = Vec::new();
    for item in prepared {
        for (device, tx) in &item.sessions {
            if !sessions
                .iter()
                .any(|(known, current)| known == device && current.same_channel(tx))
            {
                sessions.push((*device, tx.clone()));
            }
        }
        for target in &item.replay_targets {
            if !replay_targets.iter().any(|known| {
                known.device == target.device
                    && Arc::ptr_eq(&known.live, &target.live)
                    && Arc::ptr_eq(&known.catchup, &target.catchup)
            }) {
                replay_targets.push(target.clone());
            }
        }
    }
    (sessions, replay_targets)
}

fn commit_and_materialize(
    state: &mut crate::persistence::State,
    input: &CheckpointBatchInput,
) -> Result<u64, crate::store::StoreError> {
    if input.sequence.is_none() {
        return Err(crate::persistence::error(
            "checkpoint sequence was not reserved",
        ));
    }
    let Envelope::Delta { delta, .. } = &*input.envelope else {
        return Err(crate::persistence::error(
            "checkpoint input was not a delta",
        ));
    };
    state.materialize(delta);
    Ok(input
        .stage
        .as_ref()
        .map_or(0, super::store::retained::CheckpointStage::written))
}

fn state_commit_for(
    input: &mut CheckpointBatchInput,
    publication: crate::store::shell_selection::Publication,
) -> Result<StateCommit, crate::store::StoreError> {
    let prepared = input
        .prepared
        .take()
        .ok_or_else(|| crate::persistence::error("checkpoint recipients were not prepared"))?;
    let (publish, selected) = match publication {
        crate::store::shell_selection::Publication::Suppress => (false, None),
        crate::store::shell_selection::Publication::Activate(path) => {
            (true, Some(SelectedPublication::Activation(path)))
        }
        crate::store::shell_selection::Publication::ReplayRequired(path) => {
            (true, Some(SelectedPublication::Recovery(path)))
        }
        crate::store::shell_selection::Publication::Original => (true, None),
    };
    Ok(StateCommit {
        continuity: input.continuity.clone(),
        envelope: input.envelope.clone(),
        pending: HashMap::new(),
        persisted_bytes: input
            .stage
            .as_ref()
            .map_or(0, super::store::retained::CheckpointStage::written),
        session_lives: prepared.session_lives,
        sessions: prepared.sessions,
        logical_offline_bytes: prepared.logical_offline_bytes,
        publish,
        selected,
        recovery_scope: false,
        activation_error: None,
        replay_targets: prepared.replay_targets,
        accepted_deltas: 1,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckpointBatchMode {
    Represented,
    AllOriginal,
    Serial,
}

#[cfg(test)]
type CheckpointBatchModeObservers =
    std::sync::Mutex<HashMap<u64, Arc<std::sync::Mutex<Vec<CheckpointBatchMode>>>>>;
#[cfg(test)]
static CHECKPOINT_BATCH_MODE_OBSERVERS: std::sync::OnceLock<CheckpointBatchModeObservers> =
    std::sync::OnceLock::new();

#[cfg(test)]
#[derive(Debug, Default)]
struct CheckpointProgress {
    active: Option<String>,
    completed: std::collections::BTreeMap<String, (usize, u128)>,
}

#[cfg(test)]
static CHECKPOINT_PROGRESS: std::sync::OnceLock<
    std::sync::Mutex<HashMap<u64, Arc<std::sync::Mutex<CheckpointProgress>>>>,
> = std::sync::OnceLock::new();

#[cfg(test)]
struct CheckpointProgressStep {
    observed: Arc<std::sync::Mutex<CheckpointProgress>>,
    phase: String,
    started: std::time::Instant,
}

#[cfg(test)]
impl Drop for CheckpointProgressStep {
    fn drop(&mut self) {
        let mut observed = self
            .observed
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = observed.completed.entry(self.phase.clone()).or_default();
        entry.0 = entry.0.saturating_add(1);
        entry.1 = entry.1.saturating_add(self.started.elapsed().as_micros());
        observed.active = None;
    }
}

#[cfg(test)]
fn checkpoint_progress_step(batch: &CheckpointBatch) -> Option<CheckpointProgressStep> {
    let phase = if batch.stage_index < batch.inputs.len() {
        "retention-stage".into()
    } else {
        batch.selector.as_ref().map_or_else(
            || "selection-create".into(),
            super::store::shell_selection::Selection::diagnostic_phase,
        )
    };
    checkpoint_progress_phase(batch, phase)
}

#[cfg(test)]
fn checkpoint_progress_phase(
    batch: &CheckpointBatch,
    phase: String,
) -> Option<CheckpointProgressStep> {
    let Envelope::Delta { workspace_id, .. } = &*batch.inputs.first()?.envelope else {
        return None;
    };
    let observed = CHECKPOINT_PROGRESS
        .get()?
        .lock()
        .ok()?
        .get(workspace_id)?
        .clone();
    observed.lock().ok()?.active = Some(phase.clone());
    Some(CheckpointProgressStep {
        observed,
        phase,
        started: std::time::Instant::now(),
    })
}

#[cfg(test)]
fn track_checkpoint_batch_modes(
    workspace_id: u64,
) -> Arc<std::sync::Mutex<Vec<CheckpointBatchMode>>> {
    let observed = Arc::new(std::sync::Mutex::new(Vec::new()));
    CHECKPOINT_BATCH_MODE_OBSERVERS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .insert(workspace_id, observed.clone());
    observed
}

#[cfg(test)]
fn observe_checkpoint_batch_mode(batch: &CheckpointBatch, mode: CheckpointBatchMode) {
    let Some(Envelope::Delta { workspace_id, .. }) =
        batch.inputs.first().map(|input| &*input.envelope)
    else {
        return;
    };
    if let Some(observed) = CHECKPOINT_BATCH_MODE_OBSERVERS
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .get(workspace_id)
    {
        observed.lock().unwrap().push(mode);
    }
}

fn checkpoint_batch_mode(batch: &CheckpointBatch) -> CheckpointBatchMode {
    checkpoint_mode_for(batch.projected.as_ref(), batch.inputs.len())
}

fn checkpoint_mode_for(
    outcome: Option<&crate::store::shell_selection::BatchSelectionOutcome>,
    input_count: usize,
) -> CheckpointBatchMode {
    use crate::store::shell_selection::{BatchProjectionReason, IncomingDisposition, Publication};
    let Some(outcome) = outcome else {
        return CheckpointBatchMode::Serial;
    };
    if outcome.dispositions.len() != input_count {
        return CheckpointBatchMode::Serial;
    }
    if outcome.reason == BatchProjectionReason::FinalBasisOmission
        && outcome.publication == Publication::Original
        && outcome
            .dispositions
            .iter()
            .all(|value| *value == IncomingDisposition::Original)
    {
        return CheckpointBatchMode::AllOriginal;
    }
    if outcome.reason == BatchProjectionReason::Represented
        && outcome.publication != Publication::Original
        && outcome
            .dispositions
            .iter()
            .all(|value| *value != IncomingDisposition::Original)
    {
        return CheckpointBatchMode::Represented;
    }
    CheckpointBatchMode::Serial
}

enum ProjectedCheckpointStep {
    More,
    Commit(Box<StateCommit>),
    Done,
}

struct ProjectedCheckpointCommit {
    mode: CheckpointBatchMode,
    selected_committed: bool,
    publication: Option<crate::store::shell_selection::Publication>,
    gc_index: usize,
    commit: Option<StateCommit>,
    replay: Option<ReplayPendingWork>,
    publication_index: usize,
    emission_done: bool,
    full_fence: Option<CheckpointCommitFence>,
    recovery_fence: Option<CheckpointCommitFence>,
    supplemental_recovery: Option<std::path::PathBuf>,
}

impl ProjectedCheckpointCommit {
    fn new(batch: &CheckpointBatch, mode: CheckpointBatchMode) -> Self {
        let full_fence = (mode == CheckpointBatchMode::AllOriginal).then(|| {
            let (sessions, replay_targets) = union_sessions(
                batch
                    .inputs
                    .iter()
                    .filter_map(|input| input.prepared.as_ref()),
            );
            CheckpointCommitFence {
                recovery_lives: None,
                activating: true,
                sessions,
                replay_targets,
            }
        });
        let supplemental_recovery = (mode == CheckpointBatchMode::AllOriginal)
            .then(|| {
                batch
                    .projected
                    .as_ref()
                    .and_then(|p| p.supplemental_recovery.clone())
            })
            .flatten();
        // Both exist only in AllOriginal mode, so the pairing never drops a fence.
        let recovery_fence = supplemental_recovery
            .as_ref()
            .and_then(|_| full_fence.clone())
            .map(|mut fence| {
                fence.recovery_lives = Some(union_session_lives(
                    batch
                        .inputs
                        .iter()
                        .filter_map(|input| input.prepared.as_ref()),
                ));
                fence
            });
        Self {
            recovery_fence,
            supplemental_recovery,
            mode,
            selected_committed: false,
            publication: None,
            gc_index: 0,
            commit: None,
            replay: None,
            publication_index: 0,
            emission_done: false,
            full_fence,
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
    )]
    fn step(
        &mut self,
        state: &mut crate::persistence::State,
        batch: &mut CheckpointBatch,
    ) -> Result<ProjectedCheckpointStep, crate::store::StoreError> {
        if !self.selected_committed {
            match batch
                .selector
                .as_mut()
                .invariant("projected checkpoint selector")?
                .step(WorkLimit::default())?
            {
                Step::More => return Ok(ProjectedCheckpointStep::More),
                Step::Done(crate::store::shell_selection::SelectionOutcome::BatchPublished {
                    supplemental_recovery,
                    publication,
                    dispositions,
                }) => {
                    // CURRENT has already switched when Selection returns. Set
                    // this before validating the repeated outcome so every
                    // mismatch takes the immutable full-session fence.
                    self.selected_committed = true;
                    let expected = batch.projected.as_ref().ok_or_else(|| {
                        crate::persistence::error("projected checkpoint outcome disappeared")
                    })?;
                    if publication != expected.publication
                        || dispositions != expected.dispositions
                        || supplemental_recovery != expected.supplemental_recovery
                    {
                        return Err(crate::persistence::error(
                            "checkpoint projection changed while committing",
                        ));
                    }
                    self.publication = Some(publication);
                    return Ok(ProjectedCheckpointStep::More);
                }
                Step::Done(_) => {
                    return Err(crate::persistence::error(
                        "checkpoint batch returned an invalid selection outcome",
                    ));
                }
            }
        }
        if let Some(input) = batch.inputs.get_mut(self.gc_index) {
            match input
                .stage
                .as_mut()
                .invariant("stage missing")?
                .finish_gc(WorkLimit::default())?
            {
                Step::More => return Ok(ProjectedCheckpointStep::More),
                Step::Done(()) => {
                    commit_and_materialize(state, input)?;
                    self.gc_index = self.gc_index.saturating_add(1);
                    return Ok(ProjectedCheckpointStep::More);
                }
            }
        }
        // A previous serial prefix may still be held. Deliver its safe full
        // fallback before any current Original checkpoint can advance a floor.
        // This is publication only: every input keeps its original disposition.
        if let Some(path) = &self.supplemental_recovery {
            if self.commit.is_none() {
                let prepared = || {
                    batch
                        .inputs
                        .iter()
                        .filter_map(|input| input.prepared.as_ref())
                };
                let (sessions, replay_targets) = union_sessions(prepared());
                let selected = SelectedPublication::Recovery(path.clone());
                self.replay = (!replay_targets.is_empty())
                    .then(|| ReplayPendingWork::selected(&selected, replay_targets.clone()))
                    .transpose()?;
                self.commit = Some(StateCommit {
                    continuity: None,
                    envelope: batch
                        .inputs
                        .last()
                        .invariant("admitted prefix")?
                        .envelope
                        .clone(),
                    pending: HashMap::new(),
                    persisted_bytes: 0,
                    sessions,
                    session_lives: union_session_lives(prepared()),
                    logical_offline_bytes: 0,
                    publish: true,
                    selected: Some(selected),
                    recovery_scope: false,
                    activation_error: None,
                    replay_targets,
                    accepted_deltas: 0,
                });
                return Ok(ProjectedCheckpointStep::More);
            }
            if let Some(replay) = &mut self.replay {
                match replay.step(state, crate::persistence::output_budget())? {
                    Step::More => return Ok(ProjectedCheckpointStep::More),
                    Step::Done(written) => {
                        self.commit
                            .as_mut()
                            .invariant("commit missing")?
                            .persisted_bytes = written;
                        self.replay = None;
                    }
                }
            }
            self.supplemental_recovery = None;
            return Ok(ProjectedCheckpointStep::Commit(Box::new(
                self.commit.take().invariant("commit missing")?,
            )));
        }
        if self.mode == CheckpointBatchMode::AllOriginal {
            #[cfg(test)]
            if self.publication_index == 1 {
                crate::store::test_barrier::check(
                    state.store.root(),
                    "checkpoint-prefix-after-first-original",
                )?;
            }
            if self.publication != Some(crate::store::shell_selection::Publication::Original) {
                return Err(crate::persistence::error(
                    "all-original checkpoint projection activated a selected basis",
                ));
            }
            if self.publication_index == batch.inputs.len() {
                return Ok(ProjectedCheckpointStep::Done);
            }
            if self.commit.is_none() {
                let mut commit = state_commit_for(
                    batch
                        .inputs
                        .get_mut(self.publication_index)
                        .invariant("checkpoint input missing")?,
                    crate::store::shell_selection::Publication::Original,
                )?;
                commit.recovery_scope = self.recovery_fence.is_some();
                self.replay = (!commit.replay_targets.is_empty()).then(|| {
                    ReplayPendingWork::original(
                        commit.envelope.clone(),
                        commit.replay_targets.clone(),
                        commit.continuity.clone(),
                    )
                });
                self.commit = Some(commit);
                return Ok(ProjectedCheckpointStep::More);
            }
            if let Some(replay) = &mut self.replay {
                match replay.step(state, crate::persistence::output_budget())? {
                    Step::More => return Ok(ProjectedCheckpointStep::More),
                    Step::Done(written) => {
                        self.commit
                            .as_mut()
                            .invariant("commit missing")?
                            .persisted_bytes = self
                            .commit
                            .as_ref()
                            .invariant("commit missing")?
                            .persisted_bytes
                            .saturating_add(written);
                        self.replay = None;
                    }
                }
            }
            self.publication_index = self.publication_index.saturating_add(1);
            return Ok(ProjectedCheckpointStep::Commit(Box::new(
                self.commit.take().invariant("commit missing")?,
            )));
        }
        if self.emission_done {
            return Ok(ProjectedCheckpointStep::Done);
        }
        if self.commit.is_none() {
            let prepared = batch
                .inputs
                .iter_mut()
                .map(|input| input.prepared.take().invariant("prepared checkpoint input"))
                .collect::<Result<Vec<_>, _>>()?;
            let (sessions, replay_targets) = union_sessions(&prepared);
            let (publish, selected) = match self
                .publication
                .take()
                .invariant("represented checkpoint publication")?
            {
                crate::store::shell_selection::Publication::Suppress => (false, None),
                crate::store::shell_selection::Publication::Activate(path) => {
                    (true, Some(SelectedPublication::Activation(path)))
                }
                crate::store::shell_selection::Publication::ReplayRequired(_)
                | crate::store::shell_selection::Publication::Original => {
                    return Err(crate::persistence::error(
                        "represented checkpoint batch returned original publication",
                    ));
                }
            };
            let commit = StateCommit {
                continuity: None,
                envelope: batch
                    .inputs
                    .last()
                    .invariant("inputs missing")?
                    .envelope
                    .clone(),
                pending: HashMap::new(),
                persisted_bytes: batch
                    .inputs
                    .iter()
                    .map(|input| {
                        input
                            .stage
                            .as_ref()
                            .map(crate::store::retained::CheckpointStage::written)
                            .invariant("checkpoint stage missing")
                    })
                    .sum::<Result<_, _>>()?,
                session_lives: Vec::new(),
                sessions,
                logical_offline_bytes: prepared.iter().map(|item| item.logical_offline_bytes).sum(),
                publish,
                selected,
                recovery_scope: false,
                activation_error: None,
                replay_targets,
                accepted_deltas: batch.inputs.len() as u64,
            };
            self.commit = Some(commit);
            let commit = self.commit.as_ref().invariant("commit missing")?;
            self.replay = if let Some(path) = &commit.selected {
                (!commit.replay_targets.is_empty())
                    .then(|| ReplayPendingWork::selected(path, commit.replay_targets.clone()))
                    .transpose()?
            } else {
                None
            };
            return Ok(ProjectedCheckpointStep::More);
        }
        if let Some(replay) = &mut self.replay {
            match replay.step(state, crate::persistence::output_budget())? {
                Step::More => return Ok(ProjectedCheckpointStep::More),
                Step::Done(written) => {
                    let commit = self.commit.as_mut().invariant("commit missing")?;
                    commit.persisted_bytes = commit.persisted_bytes.saturating_add(written);
                    self.replay = None;
                }
            }
        }
        self.emission_done = true;
        Ok(ProjectedCheckpointStep::Commit(Box::new(
            self.commit.take().invariant("commit missing")?,
        )))
    }

    fn error_fence(&self, batch: &CheckpointBatch) -> Option<CheckpointCommitFence> {
        if self.mode == CheckpointBatchMode::AllOriginal
            && (self.selected_committed
                || batch.selector.as_ref().is_some_and(
                    super::store::shell_selection::Selection::current_switch_attempted,
                ))
        {
            return self
                .recovery_fence
                .clone()
                .or_else(|| self.full_fence.clone());
        }
        // The represented commit now owns these recipients. Reconstructing a
        // fence from consumed inputs would hide the publisher's captured fence.
        if self.emission_done {
            return None;
        }
        if let Some(commit) = &self.commit {
            return Some(CheckpointCommitFence::capture(commit));
        }
        let activating = batch
            .selector
            .as_ref()
            .is_some_and(|selector| selector.publication_on_error().is_some());
        if !activating {
            return None;
        }
        let mut sessions = Vec::new();
        let mut replay_targets = Vec::new();
        for prepared in batch
            .inputs
            .iter()
            .filter_map(|input| input.prepared.as_ref())
        {
            for (device, tx) in &prepared.sessions {
                if !sessions
                    .iter()
                    .any(|(known, current): &([u8; 32], mpsc::Sender<Envelope>)| {
                        known == device && current.same_channel(tx)
                    })
                {
                    sessions.push((*device, tx.clone()));
                }
            }
            for target in &prepared.replay_targets {
                if !replay_targets.iter().any(|known: &ReplayPendingTarget| {
                    known.device == target.device
                        && Arc::ptr_eq(&known.live, &target.live)
                        && Arc::ptr_eq(&known.catchup, &target.catchup)
                }) {
                    replay_targets.push(target.clone());
                }
            }
        }
        Some(CheckpointCommitFence {
            recovery_lives: None,
            activating: true,
            sessions,
            replay_targets,
        })
    }
}

fn promote_checkpoint_input(
    _state: &mut crate::persistence::State,
    batch: &mut CheckpointBatch,
    index: usize,
) -> Result<(), crate::store::StoreError> {
    let input = batch
        .inputs
        .get_mut(index)
        .invariant("checkpoint input missing")?;
    input.stage.as_mut().invariant("stage missing")?.promote()
}

struct SerialCheckpointCommit {
    index: usize,
    retain: Option<crate::store::retained::RetainBuild>,
    selection: Option<crate::store::shell_selection::Selection>,
    publication: Option<crate::store::shell_selection::Publication>,
    retained_written: u64,
    commit: Option<StateCommit>,
    replay: Option<ReplayPendingWork>,
    post_cut_fence: Option<CheckpointCommitFence>,
}

impl SerialCheckpointCommit {
    fn new(
        state: &mut crate::persistence::State,
        batch: &mut CheckpointBatch,
        index: usize,
    ) -> Result<Self, crate::store::StoreError> {
        let (retain, selection) = if batch
            .inputs
            .get(index)
            .invariant("checkpoint input missing")?
            .stage
            .is_some()
        {
            promote_checkpoint_input(state, batch, index)?;
            let selector = crate::store::shell_selection::Selection::new(
                &state.store,
                batch.root.join(format!("serial-{index:02}")),
                Some(
                    batch
                        .inputs
                        .get(index)
                        .invariant("checkpoint input missing")?
                        .incoming,
                ),
            )?;
            state.retention_candidates.push(selector.cleanup_paths());
            (None, Some(selector))
        } else {
            let input = batch
                .inputs
                .get(index)
                .invariant("checkpoint input missing")?;
            (
                Some(
                    state
                        .store
                        .start_retain_in(
                            &input.channel,
                            None,
                            input.raw.clone(),
                            batch.root.join(format!("serial-retain-{index:02}")),
                        )?
                        .with_continuity(input.continuity.clone()),
                ),
                None,
            )
        };
        Ok(Self {
            index,
            retain,
            selection,
            publication: None,
            retained_written: 0,
            commit: None,
            replay: None,
            post_cut_fence: None,
        })
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
    )]
    fn step(
        &mut self,
        state: &mut crate::persistence::State,
        batch: &mut CheckpointBatch,
    ) -> Result<Step<StateCommit>, crate::store::StoreError> {
        if let Some(retain) = &mut self.retain {
            match retain.step(WorkLimit::default())? {
                Step::More => return Ok(Step::More),
                Step::Done(outcome) => {
                    self.retained_written = outcome.written;
                    self.publication = Some(outcome.publication);
                    self.retain = None;
                    return Ok(Step::More);
                }
            }
        }
        if let Some(selection) = &mut self.selection {
            match selection.step(WorkLimit::default())? {
                Step::More => return Ok(Step::More),
                Step::Done(crate::store::shell_selection::SelectionOutcome::Published(
                    publication,
                )) => {
                    if matches!(
                        publication,
                        crate::store::shell_selection::Publication::Activate(_)
                            | crate::store::shell_selection::Publication::ReplayRequired(_)
                    ) {
                        let prepared = batch
                            .inputs
                            .get(self.index)
                            .invariant("checkpoint input missing")?
                            .prepared
                            .as_ref()
                            .invariant("prepared checkpoint input")?;
                        self.post_cut_fence = Some(CheckpointCommitFence {
                            recovery_lives: matches!(
                                publication,
                                crate::store::shell_selection::Publication::ReplayRequired(_)
                            )
                            .then(|| prepared.session_lives.clone()),
                            activating: true,
                            sessions: prepared.sessions.clone(),
                            replay_targets: prepared.replay_targets.clone(),
                        });
                    }
                    self.publication = Some(publication);
                    self.selection = None;
                    return Ok(Step::More);
                }
                Step::Done(_) => {
                    return Err(crate::persistence::error(
                        "serial checkpoint fallback returned invalid selection outcome",
                    ));
                }
            }
        }
        if self.commit.is_none() {
            let input = batch
                .inputs
                .get_mut(self.index)
                .invariant("checkpoint input missing")?;
            if let Some(stage) = input.stage.as_mut()
                && matches!(stage.finish_gc(WorkLimit::default())?, Step::More)
            {
                return Ok(Step::More);
            }
            commit_and_materialize(state, input)?;
            let mut commit = state_commit_for(
                input,
                self.publication
                    .take()
                    .invariant("serial checkpoint publication")?,
            )?;
            commit.persisted_bytes = commit.persisted_bytes.max(self.retained_written);
            self.commit = Some(commit);
            let commit = self.commit.as_ref().invariant("commit missing")?;
            self.replay = if let Some(path) = &commit.selected {
                (!commit.replay_targets.is_empty())
                    .then(|| ReplayPendingWork::selected(path, commit.replay_targets.clone()))
                    .transpose()?
            } else if commit.publish && !commit.replay_targets.is_empty() {
                Some(ReplayPendingWork::original(
                    commit.envelope.clone(),
                    commit.replay_targets.clone(),
                    commit.continuity.clone(),
                ))
            } else {
                None
            };
            return Ok(Step::More);
        }
        if let Some(replay) = &mut self.replay {
            match replay.step(state, crate::persistence::output_budget())? {
                Step::More => return Ok(Step::More),
                Step::Done(written) => {
                    self.commit
                        .as_mut()
                        .invariant("commit missing")?
                        .persisted_bytes = self
                        .commit
                        .as_ref()
                        .invariant("commit missing")?
                        .persisted_bytes
                        .saturating_add(written);
                    self.replay = None;
                }
            }
        }
        Ok(Step::Done(self.commit.take().invariant("commit missing")?))
    }

    fn error_fence(&self, batch: &CheckpointBatch) -> Option<CheckpointCommitFence> {
        if let Some(commit) = &self.commit {
            return Some(CheckpointCommitFence::capture(commit));
        }
        if let Some(fence) = &self.post_cut_fence {
            return Some(fence.clone());
        }
        let publication = self
            .selection
            .as_ref()
            .and_then(super::store::shell_selection::Selection::publication_on_error)
            .or_else(|| {
                self.retain
                    .as_ref()
                    .and_then(super::store::retained::RetainBuild::publication_on_error)
            })?;
        let prepared = batch.inputs.get(self.index)?.prepared.as_ref()?;
        Some(CheckpointCommitFence {
            recovery_lives: matches!(
                publication,
                crate::store::shell_selection::Publication::ReplayRequired(_)
            )
            .then(|| prepared.session_lives.clone()),
            activating: true,
            sessions: prepared.sessions.clone(),
            replay_targets: prepared.replay_targets.clone(),
        })
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
)]
async fn publish_checkpoint_commit(
    workspace: &SharedWorkspace,
    source: &StateSource,
    workers: Arc<tokio::sync::Semaphore>,
    metrics: &crate::metrics::Metrics,
    mut commit: StateCommit,
) -> Result<(), crate::store::StoreError> {
    trace_native_publication(
        if commit.activation_error.is_some() {
            "prefix_activation_error"
        } else if commit.publish {
            "prefix_durable"
        } else {
            "prefix_suppressed"
        },
        &commit.envelope,
        None,
    );
    if !commit.publish {
        return Ok(());
    }
    for _ in 0..commit.accepted_deltas {
        metrics.inc_deltas_received();
    }
    let activating = commit.selected.is_some() || commit.recovery_scope;
    let recovering =
        commit.recovery_scope || matches!(commit.selected, Some(SelectedPublication::Recovery(_)));
    let mut cursor = if let Some(path) = commit.selected.take() {
        Some(crate::persistence::blocking(workers.clone(), move || path.open()).await??)
    } else {
        None
    };
    let mut original = cursor.is_none();
    let mut delivered = 0u64;
    loop {
        let (envelope, budget) = if let Some(mut selected) = cursor.take() {
            let (next, result) = crate::persistence::blocking(workers.clone(), move || {
                let result = selected.step(crate::persistence::output_budget());
                (selected, result)
            })
            .await?;
            cursor = Some(next);
            match result? {
                Step::More => continue,
                Step::Done(None) => break,
                Step::Done(Some(batch)) => (LiveEnvelope::Selected(Arc::new(batch)), Ok(None)),
            }
        } else if original {
            original = false;
            let bytes = match &*commit.envelope {
                Envelope::Delta { delta, .. } => delta.len().saturating_add(256),
                _ => 1,
            };
            (
                LiveEnvelope::Original(commit.envelope.clone()),
                crate::persistence::delivery_budget(bytes).map(Some),
            )
        } else {
            break;
        };
        let mut runtime = crate::workspace_io::fresh_membership(workspace).await?;
        if !source.authorized(&runtime) {
            if activating {
                return Err(crate::persistence::error(
                    "activation source authorization changed",
                ));
            }
            break;
        }
        let checkpoint_channel = gated_checkpoint_channel(&envelope, Some(source));
        for (recipient, original) in &commit.sessions {
            if recovering
                && !captured_live_current(&commit.session_lives, &runtime, recipient, original)
            {
                continue;
            }
            let Some(sub) = runtime.subscribers.get(recipient) else {
                continue;
            };
            if !sub.tx.same_channel(original) {
                continue;
            }
            let Some(target) = runtime.state_sessions.get(recipient) else {
                continue;
            };
            if !source.admits(&runtime, &target.peer) {
                continue;
            }
            let replay_phase = target
                .replay
                .as_ref()
                .map(|replay| replay.phase.load(std::sync::atomic::Ordering::SeqCst));
            if replay_phase == Some(ReplayCatchup::ACTIVE) {
                target
                    .replay
                    .as_ref()
                    .invariant("replay missing")?
                    .dirty
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                continue;
            }
            if target.peer.role == crate::auth::PeerRole::Device
                && checkpoint_channel
                    .is_some_and(|channel| !runtime.needs_checkpoint(recipient, &channel))
            {
                continue;
            }
            let Ok(budget) = &budget else {
                if activating {
                    return Err(crate::persistence::error(
                        "activation delivery byte backpressure",
                    ));
                }
                if replay_phase == Some(ReplayCatchup::BOUNDARY) {
                    target
                        .live
                        .store(false, std::sync::atomic::Ordering::SeqCst);
                    let _ = sub.disconnect.try_send(());
                } else if let Some(lag) = &target.lag {
                    lag.mark(runtime.store.workspace_id());
                }
                continue;
            };
            let delivery = StateDelivery {
                envelope: envelope.clone(),
                cleanup: commit.pending.remove(recipient),
                source: Some(source.clone()),
                _budget: budget.clone(),
                live: target.live.clone(),
            };
            match target.tx.try_send(delivery) {
                Ok(()) => {
                    delivered = delivered.saturating_add(1);
                    if target.peer.role == crate::auth::PeerRole::Device
                        && let Some(channel) = checkpoint_channel
                    {
                        runtime.mark_checkpoint_served(recipient, channel);
                    }
                }
                Err(mpsc::error::TrySendError::Full(delivery)) => {
                    if activating || matches!(&delivery.envelope, LiveEnvelope::Selected(_)) {
                        let _ = sub.disconnect.try_send(());
                        runtime.remove_subscriber_session(recipient, original);
                    } else if replay_phase == Some(ReplayCatchup::BOUNDARY) {
                        target
                            .live
                            .store(false, std::sync::atomic::Ordering::SeqCst);
                        let _ = sub.disconnect.try_send(());
                    } else if let Some(lag) = &target.lag {
                        lag.mark(runtime.store.workspace_id());
                    }
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    let _ = sub.disconnect.try_send(());
                    runtime.remove_subscriber_session(recipient, original);
                }
            }
        }
    }
    metrics.add_deltas_fanned_out(delivered);
    metrics.add_bytes_queued(commit.persisted_bytes);
    Ok(())
}

#[derive(Clone)]
struct CheckpointCommitFence {
    recovery_lives: Option<SessionLives>,
    activating: bool,
    sessions: Vec<([u8; 32], mpsc::Sender<Envelope>)>,
    replay_targets: Vec<ReplayPendingTarget>,
}

impl CheckpointCommitFence {
    fn capture(commit: &StateCommit) -> Self {
        Self {
            recovery_lives: (commit.recovery_scope
                || matches!(commit.selected, Some(SelectedPublication::Recovery(_))))
            .then(|| commit.session_lives.clone()),
            activating: commit.selected.is_some() || commit.recovery_scope,
            sessions: commit.sessions.clone(),
            replay_targets: commit.replay_targets.clone(),
        }
    }

    async fn apply(self, workspace: &SharedWorkspace) {
        let mut runtime = workspace.lock().await;
        if self.activating {
            for (recipient, original) in &self.sessions {
                if self.recovery_lives.as_ref().is_some_and(|lives| {
                    !captured_live_current(lives, &runtime, recipient, original)
                }) {
                    continue;
                }
                if let Some(sub) = runtime.subscribers.get(recipient)
                    && sub.tx.same_channel(original)
                {
                    let _ = sub.disconnect.try_send(());
                    runtime.remove_subscriber_session(recipient, original);
                }
            }
        } else {
            for target in &self.replay_targets {
                if target.current(&runtime)
                    && let Some(sub) = runtime.subscribers.get(&target.device)
                {
                    let outbound = sub.tx.clone();
                    let _ = sub.disconnect.try_send(());
                    runtime.remove_subscriber_session(&target.device, &outbound);
                }
            }
        }
    }
}

async fn publish_checkpoint_commit_fenced(
    workspace: &SharedWorkspace,
    source: &StateSource,
    workers: Arc<tokio::sync::Semaphore>,
    metrics: &crate::metrics::Metrics,
    commit: StateCommit,
) -> Result<(), crate::store::StoreError> {
    let fence = CheckpointCommitFence::capture(&commit);
    let result = publish_checkpoint_commit(workspace, source, workers, metrics, commit).await;
    if result.is_err() {
        fence.apply(workspace).await;
    }
    result
}

#[expect(
    clippy::too_many_lines,
    reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "each owner-state guard lives exactly for one blocking checkpoint quantum and ends with its closure"
)]
async fn enqueue_checkpoint_prefix(
    workspace: &SharedWorkspace,
    envelopes: Vec<Envelope>,
    source: StateSource,
    role: crate::auth::PeerRole,
    metrics: Arc<crate::metrics::Metrics>,
    inbound_budget: Vec<Option<Arc<tokio::sync::OwnedSemaphorePermit>>>,
) -> Result<(), PeerError> {
    for envelope in &envelopes {
        trace_native_publication("prefix_received", envelope, None);
    }
    let diagnostics = crate::persistence::diagnostics();
    let admitted_at = diagnostics.then(std::time::Instant::now);
    let message_units = u32::try_from(envelopes.len())
        .map_err(|_| PeerError::Protocol("checkpoint prefix is too long".into()))?;
    let charged_size = envelopes.iter().try_fold(0usize, |total, envelope| {
        let Envelope::Delta { delta, .. } = envelope else {
            return Err(PeerError::Protocol(
                "checkpoint prefix contained non-delta".into(),
            ));
        };
        Ok(total.saturating_add(delta.len().saturating_add(256).saturating_mul(3)))
    })?;
    if diagnostics {
        tracing::info!(
            phase = "checkpoint_prefix_admitted",
            inputs = message_units,
            charged_bytes = charged_size,
        );
    }
    let owner = crate::persistence::handle(workspace).await;
    let work_workspace = workspace.clone();
    let work_source = source.clone();
    let work_metrics = metrics.clone();
    let rx = owner
        .submit_sequence_wait(
            message_units,
            charged_size,
            move |state, workers| async move {
                let queue_wait_ms = admitted_at.map_or(0, |started| started.elapsed().as_millis());
                let projection_started = diagnostics.then(std::time::Instant::now);
                let runtime = crate::workspace_io::fresh_membership(&work_workspace).await?;
                if !work_source.authorized(&runtime) {
                    return Err(crate::persistence::error(
                        "state source authorization changed",
                    ));
                }
                drop(runtime);
                let staging_peer = work_source.peer.clone();
                let staging_state = state.clone();
                let mut batch = crate::persistence::blocking(workers.clone(), move || {
                    let mut state = staging_state
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    new_checkpoint_batch(&mut state, envelopes, Some(&staging_peer))
                })
                .await??;
                let started = diagnostics.then(std::time::Instant::now);
                loop {
                    let staging_state = state.clone();
                    let (next, result) =
                        Box::pin(crate::persistence::blocking(workers.clone(), move || {
                            let mut state = staging_state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let result = step_checkpoint_batch(&mut state, &mut batch);
                            (batch, result)
                        }))
                        .await?;
                    batch = next;
                    if matches!(result?, Step::Done(())) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                let projection_elapsed_ms =
                    projection_started.map_or(0, |started| started.elapsed().as_millis());
                if tracing::enabled!(target: "tilo_relay::native_publication", tracing::Level::DEBUG) {
                    for (index, input) in batch.inputs.iter().enumerate() {
                        use crate::store::shell_selection::IncomingDisposition;
                        let disposition = batch.projected.as_ref().and_then(|outcome| outcome.dispositions.get(index));
                        let phase = match disposition {
                            Some(IncomingDisposition::Covered) => "prefix_covered",
                            Some(IncomingDisposition::Staged) => "prefix_staged",
                            Some(IncomingDisposition::SelectedLive) => "prefix_selected_live",
                            Some(IncomingDisposition::Original) => "prefix_original",
                            None => "prefix_unclassified",
                        };
                        trace_native_publication(phase, &input.envelope, None);
                    }
                }
                if diagnostics {
                    use crate::store::shell_selection::{
                        BatchProjectionReason, IncomingDisposition, Publication,
                    };
                    let (publication, reason, dispositions) = batch.projected.as_ref().map_or(
                        ("none", "unprojectable_stage", None),
                        |outcome| {
                            let publication = match outcome.publication {
                                Publication::Original => "original",
                                Publication::Suppress => "suppress",
                                Publication::Activate(_) => "activate",
                                Publication::ReplayRequired(_) => "recovery",
                            };
                            let reason = match outcome.reason {
                                BatchProjectionReason::Represented => "represented",
                                BatchProjectionReason::FinalBasisOmission => "final_basis_omission",
                                BatchProjectionReason::UnresolvedTailDependencies => {
                                    "unresolved_tail_dependencies"
                                }
                            };
                            (publication, reason, Some(outcome.dispositions.as_slice()))
                        },
                    );
                    let dispositions = dispositions.unwrap_or_default();
                    let unclassified = batch.inputs.len().saturating_sub(dispositions.len());
                    let covered = dispositions
                        .iter()
                        .filter(|value| **value == IncomingDisposition::Covered)
                        .count();
                    let staged = dispositions
                        .iter()
                        .filter(|value| **value == IncomingDisposition::Staged)
                        .count();
                    let selected_live = dispositions
                        .iter()
                        .filter(|value| **value == IncomingDisposition::SelectedLive)
                        .count();
                    let originals = dispositions
                        .iter()
                        .filter(|value| **value == IncomingDisposition::Original)
                        .count();
                    let first_original = dispositions
                        .iter()
                        .position(|value| *value == IncomingDisposition::Original);
                    let last_original = dispositions
                        .iter()
                        .rposition(|value| *value == IncomingDisposition::Original);
                    tracing::info!(
                        phase = "checkpoint_prefix_projection",
                        inputs = batch.inputs.len(),
                        queue_wait_ms,
                        projection_elapsed_ms,
                        publication,
                        reason,
                        unclassified,
                        covered,
                        staged,
                        selected_live,
                        originals,
                        ?first_original,
                        ?last_original,
                    );
                }
                let mode = checkpoint_batch_mode(&batch);
                #[cfg(test)]
                observe_checkpoint_batch_mode(&batch, mode);
                for index in 0..batch.inputs.len() {
                    let input = batch
                        .inputs
                        .get(index)
                        .invariant("checkpoint input missing")?;
                    let Envelope::Delta {
                        author_device_public_key,
                        delta,
                        ..
                    } = &*input.envelope
                    else {
                        return Err(crate::persistence::error(
                            "checkpoint batch input is not a delta",
                        ));
                    };
                    let prepared = {
                        #[cfg(test)]
                        let _progress =
                            checkpoint_progress_phase(&batch, "fresh-recipients".into());
                        prepare_recipients(
                            &work_workspace,
                            &work_source,
                            *author_device_public_key,
                            role,
                            delta.len(),
                        )
                        .await?
                    };
                    batch
                        .inputs
                        .get_mut(index)
                        .invariant("checkpoint input missing")?
                        .prepared = Some(prepared);
                    if mode == CheckpointBatchMode::Serial {
                        let operation_state = state.clone();
                        let (next, result) =
                            Box::pin(crate::persistence::blocking(workers.clone(), move || {
                                let mut state = operation_state
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                let result =
                                    SerialCheckpointCommit::new(&mut state, &mut batch, index);
                                (batch, result)
                            }))
                            .await?;
                        batch = next;
                        let mut work = result?;
                        loop {
                            let operation_state = state.clone();
                            let (next_batch, next_work, result) = Box::pin(
                                crate::persistence::blocking(workers.clone(), move || {
                                    let mut state = operation_state
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                                    let result = work.step(&mut state, &mut batch);
                                    (batch, work, result)
                                }),
                            )
                            .await?;
                            batch = next_batch;
                            work = next_work;
                            let result = match result {
                                Ok(result) => result,
                                Err(error) => {
                                    if let Some(fence) = work.error_fence(&batch) {
                                        fence.apply(&work_workspace).await;
                                    }
                                    return Err(error);
                                }
                            };
                            match result {
                                Step::More => tokio::task::yield_now().await,
                                Step::Done(commit) => {
                                    publish_checkpoint_commit_fenced(
                                        &work_workspace,
                                        &work_source,
                                        workers.clone(),
                                        &work_metrics,
                                        commit,
                                    )
                                    .await?;
                                    break;
                                }
                            }
                        }
                    } else {
                        let operation_state = state.clone();
                        let (next, result) =
                            Box::pin(crate::persistence::blocking(workers.clone(), move || {
                                let mut state = operation_state
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                #[cfg(test)]
                                let _progress = checkpoint_progress_phase(&batch, "promote".into());
                                let result =
                                    promote_checkpoint_input(&mut state, &mut batch, index);
                                let result = if index == 0 {
                                    result.and_then(|()| {
                                        checkpoint_prefix_post_first_promotion_fault(
                                            state.store.root(),
                                        )
                                    })
                                } else {
                                    result
                                };
                                (batch, result)
                            }))
                            .await?;
                        batch = next;
                        result?;
                    }
                }
                if mode != CheckpointBatchMode::Serial {
                    let mut work = ProjectedCheckpointCommit::new(&batch, mode);
                    loop {
                        // Every AllOriginal worker quantum runs after all N
                        // promotions. A worker acquisition/join error cannot
                        // prove whether the selected CURRENT switched, so the
                        // immutable exact-session fence is conservative here.
                        let worker_fence = work
                            .recovery_fence
                            .clone()
                            .or_else(|| work.full_fence.clone());
                        let operation_state = state.clone();
                        let blocking =
                            Box::pin(crate::persistence::blocking(workers.clone(), move || {
                                let mut state = operation_state
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                #[cfg(test)]
                                let _progress = checkpoint_progress_phase(
                                    &batch,
                                    if !work.selected_committed {
                                        format!(
                                            "selected-{}",
                                            batch.selector.as_ref().unwrap().diagnostic_phase()
                                        )
                                    } else if work.gc_index < batch.inputs.len() {
                                        "retained-gc".into()
                                    } else {
                                        "delivery".into()
                                    },
                                );
                                let result = work.step(&mut state, &mut batch);
                                (batch, work, result)
                            }))
                            .await;
                        let (next_batch, next_work, result) = match blocking {
                            Ok(result) => result,
                            Err(error) => {
                                if let Some(fence) = worker_fence {
                                    fence.apply(&work_workspace).await;
                                }
                                return Err(error);
                            }
                        };
                        batch = next_batch;
                        work = next_work;
                        let result = match result {
                            Ok(result) => result,
                            Err(error) => {
                                if let Some(fence) = work.error_fence(&batch) {
                                    fence.apply(&work_workspace).await;
                                }
                                return Err(error);
                            }
                        };
                        match result {
                            ProjectedCheckpointStep::More => tokio::task::yield_now().await,
                            ProjectedCheckpointStep::Commit(commit) => {
                                let commit = *commit;
                                #[cfg(test)]
                                let _progress = checkpoint_progress_phase(&batch, "publish".into());
                                let commit_fence = CheckpointCommitFence::capture(&commit);
                                if let Err(error) = publish_checkpoint_commit(
                                    &work_workspace,
                                    &work_source,
                                    workers.clone(),
                                    &work_metrics,
                                    commit,
                                )
                                .await
                                {
                                    work.error_fence(&batch)
                                        .unwrap_or(commit_fence)
                                        .apply(&work_workspace)
                                        .await;
                                    return Err(error);
                                }
                            }
                            ProjectedCheckpointStep::Done => break,
                        }
                    }
                }
                if diagnostics {
                    tracing::info!(
                        phase = "checkpoint_prefix_complete",
                        inputs = batch.inputs.len(),
                        disposition = match mode {
                            CheckpointBatchMode::Represented => "represented",
                            CheckpointBatchMode::AllOriginal => "all_original",
                            CheckpointBatchMode::Serial => "serial",
                        },
                        success = true,
                        elapsed_ms = started.map_or(0, |started| started.elapsed().as_millis()),
                        selection_preparation_reads = batch
                            .selector
                            .as_ref()
                            .map_or(0, |selector| selector.stats().preparation_reads),
                    );
                }
                drop(inbound_budget);
                Ok(())
            },
        )
        .await?;
    let failure = source.clone();
    tokio::spawn(async move {
        if crate::persistence::receipt(rx).await.is_err() {
            failure.failed();
        }
    });
    Ok(())
}

async fn enqueue_state(
    workspace: &SharedWorkspace,
    envelope: Envelope,
    source: Option<StateSource>,
    role: crate::auth::PeerRole,
    metrics: Arc<crate::metrics::Metrics>,
) -> Result<(), PeerError> {
    Box::pin(enqueue_state_inner(
        workspace, envelope, source, role, metrics, false,
    ))
    .await
    .map(drop)
}

/// A state commit admitted to its workspace's ordered persistence owner.
struct AdmittedState {
    /// Resolves once the commit is durable and published.
    completion: tokio::sync::oneshot::Receiver<Result<StateCommit, StoreError>>,
    /// The durable host order it took; 0 until then, or when unpublished.
    sequence: Arc<std::sync::atomic::AtomicU64>,
}

/// Correlate admission and owner phases without retaining a signed payload.
#[derive(Clone)]
struct StateAdmissionTrace {
    id: blake3::Hash,
    kind: &'static str,
    workspace_id: u64,
    author: [u8; 4],
}

impl StateAdmissionTrace {
    fn new(envelope: &Envelope) -> Option<Self> {
        if !tracing::enabled!(target: "tilo_relay::native_publication", tracing::Level::DEBUG) {
            return None;
        }
        let Envelope::Delta {
            workspace_id,
            author_device_public_key,
            delta,
            ..
        } = envelope
        else {
            return None;
        };
        let payload = crate::coedit::CoeditPayload::decode(delta)?;
        let (kind, body) = match payload {
            crate::coedit::CoeditPayload::Checkpoint { body, .. } => ("checkpoint", body),
            crate::coedit::CoeditPayload::RetainedDelta { body, .. } => ("delta", body),
        };
        let mut author = [0; 4];
        author.copy_from_slice(&author_device_public_key[..4]);
        Some(Self {
            id: blake3::hash(&body),
            kind,
            workspace_id: *workspace_id,
            author,
        })
    }

    fn emit(&self, phase: &str) {
        tracing::debug!(target: "tilo_relay::native_publication",
            phase, id = %self.id, kind = self.kind,
            workspace_id = self.workspace_id,
            author = %hex::encode(self.author),
            "native relay publication hop");
    }
}

/// A dropped admission future must be distinguishable from a slow owner job.
struct AdmissionWaitTrace(Option<StateAdmissionTrace>);

impl Drop for AdmissionWaitTrace {
    fn drop(&mut self) {
        if let Some(trace) = &self.0 {
            trace.emit("persistence_admission_cancelled");
        }
    }
}

/// Admit `envelope`, authored by this host's own device, through the
/// workspace's ordered persistence owner, as a device's commit is admitted.
/// Resolves with its host order once it is durable and published.
pub(crate) async fn admit_host_state(
    workspace: &SharedWorkspace,
    envelope: Envelope,
    metrics: Arc<crate::metrics::Metrics>,
) -> Result<u64, String> {
    let admitted = Box::pin(enqueue_state_inner(
        workspace,
        envelope,
        None,
        crate::auth::PeerRole::Device,
        metrics,
        false,
    ))
    .await
    .map_err(|error| error.to_string())?;
    admitted
        .completion
        .await
        .map_err(|_| "the workspace owner stopped before the commit was durable".to_owned())?
        .map_err(|error| error.to_string())?;
    Ok(admitted.sequence.load(std::sync::atomic::Ordering::SeqCst))
}
pub(crate) async fn enqueue_federated(
    workspace: &SharedWorkspace,
    envelope: Envelope,
    source: StateSource,
    metrics: Arc<crate::metrics::Metrics>,
    sync: bool,
) -> Result<(), PeerError> {
    Box::pin(enqueue_state_inner(
        workspace,
        envelope,
        Some(source),
        crate::auth::PeerRole::PeerRelay,
        metrics,
        sync,
    ))
    .await
    .map(drop)
}
#[expect(
    clippy::too_many_lines,
    reason = "one ordered protocol state machine; splitting it would scatter the invariants each step relies on"
)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the workspace handle and delivery budget move into the persistence closures; the lint does not see the move"
)]
async fn enqueue_state_inner(
    workspace: &SharedWorkspace,
    envelope: Envelope,
    source: Option<StateSource>,
    role: crate::auth::PeerRole,
    metrics: Arc<crate::metrics::Metrics>,
    sync: bool,
) -> Result<AdmittedState, PeerError> {
    let Envelope::Delta {
        author_device_public_key: author,
        delta,
        ..
    } = &envelope
    else {
        return Err(PeerError::Protocol("expected delta".into()));
    };
    let author = *author;
    if crate::persistence::correlated_diagnostics()
        && let Envelope::Delta {
            workspace_id,
            authored_at_ns,
            ..
        } = &envelope
    {
        tracing::info!(phase = "server_delta_admit", workspace_id, authored_at_ns);
    }
    let charge = delta.len().saturating_add(256).saturating_mul(3);
    trace_native_publication("admission_started", &envelope, None);
    let runtime = crate::workspace_io::fresh_membership(workspace).await?;
    if source
        .as_ref()
        .is_some_and(|source| !source.authorized(&runtime))
    {
        trace_native_publication("admission_source_changed", &envelope, None);
        return Err(PeerError::Protocol(
            "state source authorization changed".into(),
        ));
    }
    trace_native_publication("admission_authorized", &envelope, None);
    drop(runtime);
    let owner = crate::persistence::handle(workspace).await;
    trace_native_publication("persistence_admission_wait", &envelope, None);
    let admission_trace = StateAdmissionTrace::new(&envelope);
    let mut wait_trace = AdmissionWaitTrace(admission_trace.clone());
    let workers = owner.workers();
    let workspace = workspace.clone();
    let failure = source.clone();
    let persist_metrics = metrics.clone();
    let prepare_workspace = workspace.clone();
    let prepare_source = source.clone();
    let continuity_evidence = source.as_ref().and_then(|source| {
        crate::store::continuity::Evidence::authenticated(&envelope, &source.peer)
    });
    let delta_len = delta.len();
    let mut envelope = Some(envelope);
    let mut commit: Option<StateCommit> = None;
    let mut sequence = None;
    let mut retained_build: Option<crate::store::retained::RetainBuild> = None;
    let mut pending_work = None;
    let mut replay_pending_work: Option<ReplayPendingWork> = None;
    let durable = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let durable_sequence = durable.clone();
    let operation_trace = admission_trace.clone();
    let completion_trace = admission_trace.clone();
    let completion = Box::pin(owner.submit_resumable_wait(
        charge,
        async move {
            let runtime = crate::workspace_io::fresh_membership(&prepare_workspace).await?;
            if prepare_source.as_ref().is_some_and(|source| !source.authorized(&runtime)) {
                return Err(crate::persistence::error("state source authorization changed"));
            }
            let mut recipients = runtime
                .membership
                .devices
                .iter()
                .copied()
                .filter(|key| key != &author)
                .collect::<std::collections::HashSet<_>>();
            if role == crate::auth::PeerRole::Device {
                recipients.extend(
                    runtime
                        .known_peer_relays
                        .iter()
                        .copied()
                        .filter(|key| key != &author),
                );
            }
            // Include admitted cross-user live members; explicit device membership is
            // not the only signed-room authority supported by subscriptions.
            let sessions = runtime
                .subscribers
                .iter()
                .filter(|(key, sub)| {
                    **key != author
                        && !(role == crate::auth::PeerRole::PeerRelay
                            && sub.role == crate::auth::PeerRole::PeerRelay)
                })
                .map(|(key, sub)| (*key, sub.tx.clone()))
                .collect::<Vec<_>>();
            let session_lives = sessions.iter().filter_map(|(device,tx)| runtime.state_sessions.get(device).map(|session| SessionLease {device:*device,outbound:tx.clone(),live:session.live.clone()})).collect();
            let replay_targets = sessions
                .iter()
                .filter_map(|(device, _)| {
                    let session = runtime.state_sessions.get(device)?;
                    let catchup = session.replay.as_ref()?.clone();
                    (session.live.load(std::sync::atomic::Ordering::SeqCst)
                        && catchup.phase.load(std::sync::atomic::Ordering::SeqCst)
                            == ReplayCatchup::ACTIVE)
                        .then(|| ReplayPendingTarget {
                            device: *device,
                            live: session.live.clone(),
                            catchup,
                        })
                })
                .collect::<Vec<_>>();
            recipients.extend(sessions.iter().map(|(key, _)| *key));
            if role == crate::auth::PeerRole::PeerRelay {
                recipients.retain(|key| !runtime.known_peer_relays.contains(key));
            }
            let logical_offline_bytes = (delta_len as u64).saturating_mul(
                recipients
                    .iter()
                    .filter(|key| !runtime.subscribers.contains_key(*key))
                    .count() as u64,
            );

            drop(runtime);
            Ok((
                recipients,
                sessions,
                session_lives,
                replay_targets,
                logical_offline_bytes,
            ))
        },
        move |state, prepared| {
            if commit.is_none() {
                if let Some(trace) = &operation_trace {
                    trace.emit("persistence_owner_started");
                }
                let (recipients, sessions, session_lives, replay_targets, logical_offline_bytes) =
                    prepared.take().invariant("ordered recipients")?;
                let envelope = envelope.take().invariant("admitted envelope")?;
                let Envelope::Delta { delta, .. } = &envelope else {
                    return Err(crate::persistence::error("expected admitted delta"));
                };
                let publish = !sync || state.would_materialize(delta);
                if !publish {
                    return Ok(Step::Done(StateCommit {
                        continuity: None,
                        envelope: Arc::new(envelope),
                        pending: HashMap::new(),
                        persisted_bytes: 0,
                        sessions,
                        session_lives,
                        logical_offline_bytes,
                        publish,
                        selected: None,
                        recovery_scope: false,
        activation_error: None,
                        replay_targets,
                        accepted_deltas: 1,
                    }));
                }
                let seq = state.next_sequence()?;
                sequence = Some(seq);
                durable_sequence.store(seq, std::sync::atomic::Ordering::SeqCst);
                let continuity = continuity_evidence.clone().map(|evidence| evidence.accept(seq));
                let raw = crate::transport::encode(&envelope)
                    .map_err(|e| crate::persistence::error(e.to_string()))?;
                let retention =
                    crate::coedit::retention_metadata(delta).map(|(channel, kind)| match kind {
                        crate::coedit::RetentionKind::Checkpoint => (channel, None),
                        crate::coedit::RetentionKind::Delta => (channel, Some(seq)),
                    });
                if let Some((channel, seq)) = retention {
                    #[cfg(test)]
                    crate::store::test_barrier::pause(state.store.root(), "state-persist");
                    let build = state.store.start_retain(&channel, seq, raw)?.with_continuity(continuity.clone());
                    state.retention_scratch = Some(build.scratch().to_owned());
                    retained_build = Some(build);
                } else {
                    pending_work = Some((raw, recipients.into_iter()));
                }
                commit = Some(StateCommit {
                    continuity,
                    envelope: Arc::new(envelope),
                    pending: HashMap::new(),
                    persisted_bytes: 0,
                    sessions,
                    session_lives,
                    logical_offline_bytes,
                    publish,
                    selected: None,
                    recovery_scope: false,
        activation_error: None,
                    replay_targets,
                    accepted_deltas: 1,
                });
                return Ok(Step::More);
            }
            let current = commit.as_mut().invariant("commit missing")?;
            if let Some(build) = retained_build.as_mut() {
                let result = match build.step(WorkLimit::default()) {
                    Ok(result) => result,
                    Err(error) => {
                        let Some(activation) = build.publication_on_error() else { return Err(error); };
                        // Keep the captured sessions and uncertain/committed cut
                        // across the worker error so completion fences them.
                        let Some(selected) = SelectedPublication::from_publication(activation) else {
                            return Err(error);
                        };
                        current.selected = Some(selected);
                        current.activation_error = Some(error);
                        return Ok(Step::Done(commit.take().invariant("commit missing")?));
                    }
                };
                match result {
                    Step::More => return Ok(Step::More),
                    Step::Done(outcome) => {
                        let written = outcome.written;
                        let publication = retention_publication_kind(&outcome.publication);
                        match outcome.publication {
                            crate::store::shell_selection::Publication::Suppress => {
                                current.publish = false;
                                current.replay_targets.clear();
                            }
                            publication @ (crate::store::shell_selection::Publication::Activate(_)
                                | crate::store::shell_selection::Publication::ReplayRequired(_)) => {
                                let Some(selected) = SelectedPublication::from_publication(publication) else {
                                    return Err(crate::persistence::error("selected publication changed kind"));
                                };
                                current.selected = Some(selected.clone());
                                if !current.replay_targets.is_empty() {
                                    match ReplayPendingWork::selected(
                                        &selected,
                                        current.replay_targets.clone(),
                                    ) {
                                        Ok(work) => replay_pending_work = Some(work),
                                        Err(error) => current.activation_error = Some(error),
                                    }
                                }
                            }
                            crate::store::shell_selection::Publication::Original => {
                                if !current.replay_targets.is_empty() {
                                    replay_pending_work = Some(ReplayPendingWork::original(
                                        current.envelope.clone(),
                                        current.replay_targets.clone(),
                                        current.continuity.clone(),
                                    ));
                                }
                            }
                        }
                        if crate::persistence::diagnostics() {
                            let selection = build.selection_stats();
                            tracing::info!(
                                phase = "retention_complete",
                                verified_publication = publication,
                                selected_obsolete = build.selected_obsolete(),
                                retention_preparation_reads = build
                                    .retention_stats()
                                    .preparation_reads,
                                retention_peak_metadata_entries = build
                                    .retention_stats()
                                    .peak_metadata_entries,
                                selection_preparation_reads = selection
                                    .map_or(0, |stats| stats.preparation_reads),
                                selection_peak_metadata_entries = selection
                                    .map_or(0, |stats| stats.peak_metadata_entries),
                            );
                        }
                        if !build.holds_scratch() {
                            // A plain delta made no scratch, so the owner has
                            // nothing to dispose of after this job.
                            state.retention_scratch = None;
                        }
                        persist_metrics.add_bytes_queued(written);
                        current.persisted_bytes = written;
                        retained_build = None;
                    }
                }
            }
            if let Some(work) = replay_pending_work.as_mut() {
                match work.step(state, crate::persistence::output_budget()) {
                    Ok(Step::More) => return Ok(Step::More),
                    Ok(Step::Done(written)) => {
                        persist_metrics.add_bytes_queued(written);
                        current.persisted_bytes = current.persisted_bytes.saturating_add(written);
                        replay_pending_work = None;
                    }
                    Err(error) => {
                        current.activation_error = Some(error);
                        replay_pending_work = None;
                    }
                }
            }
            if let Some((raw, recipients)) = &mut pending_work {
                for _ in 0..crate::store::bounded::RUN_ENTRIES {
                    let Some(recipient) = recipients.next() else {
                        pending_work = None;
                        break;
                    };
                    let path = state
                        .store
                        .enqueue_pending(&recipient, sequence.invariant("sequence missing")?, raw)?;
                    current.pending.insert(recipient, path);
                    persist_metrics.add_bytes_queued(raw.len() as u64);
                    current.persisted_bytes =
                        current.persisted_bytes.saturating_add(raw.len() as u64);
                }
                if pending_work.is_some() {
                    return Ok(Step::More);
                }
            }
            sequence.invariant("sequence missing")?;
            let Envelope::Delta { delta, .. } = &*current.envelope else {
                return Err(crate::persistence::error("state commit envelope is not a delta"));
            };
            state.materialize(delta);
            Ok(Step::Done(commit.take().invariant("commit missing")?))
        },
        move |result| async move {
            let result = async {
                let mut commit = result?;
                trace_native_publication(
                    if commit.activation_error.is_some() {
                        "activation_error"
                    } else if commit.publish {
                        "durable"
                    } else {
                        "suppressed"
                    },
                    &commit.envelope,
                    None,
                );
                if !commit.publish {
                    return Ok(commit);
                }
                for _ in 0..commit.accepted_deltas {
                    metrics.inc_deltas_received();
                }
                if crate::persistence::correlated_diagnostics() && let Envelope::Delta { workspace_id, authored_at_ns, .. } = &*commit.envelope { tracing::info!(phase="server_delta_committed", workspace_id, authored_at_ns, sessions=commit.sessions.len()); }
                if crate::persistence::diagnostics() {
                    tracing::info!(phase="state_commit", persisted_envelope_bytes=commit.persisted_bytes, logical_offline_bytes=commit.logical_offline_bytes);
                }
                let activating = commit.selected.is_some() || commit.recovery_scope;
                let recovering = commit.recovery_scope || matches!(commit.selected, Some(SelectedPublication::Recovery(_)));
                let replay_targets = commit.replay_targets.clone();
                let mut delivered = 0u64;
                let publication_result: Result<(), crate::store::StoreError> = async {
                if let Some(error) = commit.activation_error.take() { return Err(error); }
                let mut cursor = if let Some(path) = commit.selected.take() {
                    Some(crate::persistence::blocking(workers.clone(), move || path.open()).await??)
                } else { None };
                let mut original = cursor.is_none();
                loop {
                    let (envelope, budget) = if let Some(mut selected) = cursor.take() {
                        let (next, result) = crate::persistence::blocking(workers.clone(), move || {
                            let result = selected.step(crate::persistence::output_budget());
                            (selected, result)
                        }).await?;
                        cursor = Some(next);
                        match result? {
                            Step::More => continue,
                            Step::Done(None) => break,
                            Step::Done(Some(batch)) => (LiveEnvelope::Selected(Arc::new(batch)), Ok(None)),
                        }
                    } else if original {
                        original = false;
                        let bytes = match &*commit.envelope { Envelope::Delta {delta, ..} => delta.len().saturating_add(256), _ => 1 };
                        let budget = crate::persistence::delivery_budget(bytes).map(Some);
                        #[cfg(test)]
                        let budget = {
                            let mut budget = budget;
                            let root = workspace.lock().await.store.root().to_path_buf();
                            if crate::store::test_barrier::check(&root, "live-delivery-budget").is_err() {
                                budget = Err(crate::persistence::error("injected live delivery budget failure"));
                            }
                            budget
                        };
                        (LiveEnvelope::Original(commit.envelope.clone()), budget)
                    } else { break };
                    // Every hydration/worker wait precedes a fresh current-source,
                    // recipient membership and exact subscriber-channel fence.
                    let mut runtime = crate::workspace_io::fresh_membership(&workspace).await?;
                    if source.as_ref().is_some_and(|source| !source.authorized(&runtime)) {
                        if activating { return Err(crate::persistence::error("activation source authorization changed")); }
                        break;
                    }
                    let checkpoint_channel = gated_checkpoint_channel(&envelope, source.as_ref());
                    // Presence the author sent after this delta was held for
                    // it and goes out with it; from here on nothing waits.
                    if let Envelope::Delta { author_device_public_key, .. } = &*envelope {
                        runtime.delta_published(author_device_public_key);
                    }
                    for (recipient, original) in &commit.sessions {
                        if recovering && !captured_live_current(&commit.session_lives, &runtime, recipient, original) { continue; }
                        let Some(sub) = runtime.subscribers.get(recipient) else { continue };
                        if !sub.tx.same_channel(original) { continue; }
                        let Some(target) = runtime.state_sessions.get(recipient) else { continue };
                        // A host-authored frame has no source trust to ask, and
                        // checks the recipient's room listing only.
                        let admitted = match source.as_ref() {
                            Some(source) => source.admits(&runtime, &target.peer),
                            None => room_access::listed(room_access::Room::current(&runtime), &target.peer, RoomOperation::State),
                        };
                        if !admitted { continue; }
                        let replay_phase = target.replay.as_ref().map(|replay| {
                            replay.phase.load(std::sync::atomic::Ordering::SeqCst)
                        });
                        if replay_phase == Some(ReplayCatchup::ACTIVE) {
                            // This commit is already durable. The replay owner
                            // will take one final coherent cut before opening
                            // this exact session to post-Ready live delivery.
                            target.replay.as_ref().invariant("replay missing")?
                                .dirty
                                .store(true, std::sync::atomic::Ordering::SeqCst);
                            continue;
                        }
                        if target.peer.role == crate::auth::PeerRole::Device
                            && checkpoint_channel.is_some_and(|channel| !runtime.needs_checkpoint(recipient, &channel)) { continue; }
                        // Durable coverage was committed before publication. If
                        // the shared live-byte budget is full, leave that coverage
                        // for replay/anti-entropy without fencing a healthy wire.
                        let Ok(budget) = &budget else {
                            if activating {
                                return Err(crate::persistence::error(
                                    "activation delivery byte backpressure",
                                ));
                            }
                            if replay_phase == Some(ReplayCatchup::BOUNDARY) {
                                target.live.store(false, std::sync::atomic::Ordering::SeqCst);
                                let _ = sub.disconnect.try_send(());
                            } else if let Some(lag) = &target.lag {
                                lag.mark(runtime.store.workspace_id());
                            }
                            continue
                        };
                        if crate::persistence::correlated_diagnostics() && let Envelope::Delta { workspace_id, authored_at_ns, .. } = &*envelope {
                            tracing::info!(phase="server_delta_publication", workspace_id, authored_at_ns, recipient=%hex::encode(&recipient[..4]));
                        }
                        let delivery = StateDelivery {
                            envelope: envelope.clone(), cleanup: commit.pending.remove(recipient), source: source.clone(),
                            _budget: budget.clone(), live: target.live.clone(),
                        };
                        match target.tx.try_send(delivery) {
                            Ok(()) => {
                                delivered = delivered.saturating_add(1);
                                if target.peer.role == crate::auth::PeerRole::Device && let Some(channel) = checkpoint_channel {
                                    runtime.mark_checkpoint_served(recipient, channel);
                                }
                            }
                            // Ordinary live traffic retains replay coverage under
                            // backpressure. A selected activation is a multi-frame
                            // basis whose exact retry is suppressed after CURRENT;
                            // a partial recipient must reconnect to recover it.
                            Err(mpsc::error::TrySendError::Full(delivery)) => {
                                if activating
                                    || matches!(&delivery.envelope, LiveEnvelope::Selected(_))
                                {
                                    let _ = sub.disconnect.try_send(());
                                    runtime.remove_subscriber_session(recipient, original);
                                    continue;
                                }
                                if replay_phase == Some(ReplayCatchup::BOUNDARY) {
                                    target
                                        .live
                                        .store(false, std::sync::atomic::Ordering::SeqCst);
                                    let _ = sub.disconnect.try_send(());
                                } else if let Some(lag) = &target.lag {
                                    lag.mark(runtime.store.workspace_id());
                                }
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                let _ = sub.disconnect.try_send(());
                                runtime.remove_subscriber_session(recipient, original);
                            }
                        }
                    }
                    #[cfg(test)]
                    crate::store::test_barrier::signal(runtime.store.root(), "live-publication-complete");
                }
                Ok(())
                }.await;
                metrics.add_deltas_fanned_out(delivered);
                if let Err(error) = publication_result {
                    if activating || !replay_targets.is_empty() {
                        // A partial activation cannot leave a ready subscriber
                        // waiting forever for its missing exact half. Fence only
                        // the original session; reconnect uses durable full basis.
                        let mut runtime = workspace.lock().await;
                        if activating {
                            for (recipient, original) in &commit.sessions {
                                if recovering && !captured_live_current(&commit.session_lives, &runtime, recipient, original) { continue; }
                                if let Some(sub) = runtime.subscribers.get(recipient)
                                    && sub.tx.same_channel(original) {
                                    let _ = sub.disconnect.try_send(());
                                    runtime.remove_subscriber_session(recipient, original);
                                }
                            }
                        } else {
                            for target in &replay_targets {
                                if target.current(&runtime)
                                    && let Some(sub) = runtime.subscribers.get(&target.device)
                                {
                                    let outbound = sub.tx.clone();
                                    let _ = sub.disconnect.try_send(());
                                    runtime.remove_subscriber_session(&target.device, &outbound);
                                }
                            }
                        }
                    }
                    return Err(error);
                }
                Ok(commit)
            }
            .await;
            if let Some(trace) = &completion_trace {
                trace.emit(if result.is_ok() {
                    "persistence_completed"
                } else {
                    "persistence_failed"
                });
            }
            if crate::persistence::correlated_diagnostics() && let Err(error) = &result {
                tracing::info!(phase = "state_commit_error", error = %error);
            }
            if result.is_err()
                && let Some(source) = failure
            {
                let _ = source.disconnect.try_send(());
            }
            result
        },
    ))
    .await;
    wait_trace.0 = None;
    let completion = match completion {
        Ok(completion) => {
            if let Some(trace) = &admission_trace {
                trace.emit("persistence_admitted");
            }
            completion
        }
        Err(error) => {
            if let Some(trace) = &admission_trace {
                trace.emit("persistence_admission_failed");
            }
            return Err(error.into());
        }
    };
    Ok(AdmittedState {
        completion,
        sequence: durable,
    })
}

/// `envelope` in the framing `session` negotiated: on the compact protocol a
/// zero-signature delta goes without its signature.
fn encode_for_session(
    envelope: &Envelope,
    session: &SessionCtx<'_>,
) -> Result<Vec<u8>, postcard::Error> {
    tilo_delivery_protocol::transport_v2::encode_state_framed(envelope, session.framing)
}

/// [`encode_for_session`] for a frame about to be written, charged to its
/// room's egress. `live` marks live fan-out (a delta or presence forwarded as
/// it arrived), as opposed to replay, replies and positions.
fn encode_charged(
    envelope: &Envelope,
    session: &SessionCtx<'_>,
    live: bool,
) -> Result<Vec<u8>, postcard::Error> {
    let frame = encode_for_session(envelope, session)?;
    if let Some(room) = envelope.workspace_id() {
        session
            .metrics
            .add_room_egress(room, u64::try_from(frame.len()).unwrap_or(u64::MAX), live);
    }
    Ok(frame)
}

/// A live delta about to be written, charged to its room's live egress: on
/// `tilo-relay/7` coded against what the session last received from the
/// same author in the same room (`DeltaEncoder`), otherwise as
/// [`encode_charged`] writes it. A delta the encoder does not code (signed,
/// or larger than it keeps) goes whole and changes no slot.
fn encode_delta_charged(
    envelope: &Envelope,
    session: &SessionCtx<'_>,
    deltas: &mut tilo_delivery_protocol::transport_v2::DeltaEncoder,
) -> Result<Vec<u8>, postcard::Error> {
    if session.framing.codes_deltas()
        && let Some(frame) = deltas.encode(envelope)?
    {
        if let Some(room) = envelope.workspace_id() {
            session.metrics.add_room_egress(
                room,
                u64::try_from(frame.len()).unwrap_or(u64::MAX),
                true,
            );
        }
        return Ok(frame);
    }
    encode_charged(envelope, session, true)
}

/// Presence items one `PresenceBatch` frame carries at most.
const PRESENCE_BATCH_ITEMS: usize = 64;

/// One connection's writer-side slot coding: each half sees every frame of
/// its kind the writer writes, in order, as the reader's half does.
#[derive(Default)]
struct SessionCoding {
    /// Presence batches (`tilo-relay/6`, TIL-710).
    presence: tilo_delivery_protocol::transport_v2::PresenceEncoder,
    /// Live deltas (`tilo-relay/7`, TIL-755).
    deltas: tilo_delivery_protocol::transport_v2::DeltaEncoder,
}

/// Write the presence due for this connection, with everything else that
/// may ride along (`PresenceMailbox::take`), in one write.
async fn write_presence<W: tokio::io::AsyncWrite + Unpin>(
    mailbox: &crate::presence_mailbox::PresenceMailbox,
    slots: &mut tilo_delivery_protocol::transport_v2::PresenceEncoder,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    write: &mut W,
) -> Result<(), PeerError> {
    let out = encode_presence(
        mailbox.take(tokio::time::Instant::now(), true),
        slots,
        peer,
        session,
    )
    .await?;
    if !out.is_empty() {
        write.write_all(&out).await?;
    }
    Ok(())
}

/// Encode `due` presence for this connection, each room's authorised once:
/// on `tilo-relay/6` as slot-coded `PresenceBatch` frames, on older
/// protocols as one `Presence` frame per payload. Charged to each room's
/// live egress.
async fn encode_presence(
    due: Vec<crate::presence_mailbox::DuePresence>,
    slots: &mut tilo_delivery_protocol::transport_v2::PresenceEncoder,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Vec<u8>, PeerError> {
    let mut out = Vec::new();
    for run in due.chunk_by(|a, b| a.workspace_id == b.workspace_id) {
        let Some(room) = run.first().map(|presence| presence.workspace_id) else {
            continue;
        };
        authorize_state_recipient(room, true, peer, session).await?;
        let before = out.len();
        if session.framing.batches_presence() {
            for chunk in run.chunks(PRESENCE_BATCH_ITEMS) {
                let items: Vec<_> = chunk
                    .iter()
                    .map(|presence| slots.item(presence.key, &presence.payload))
                    .collect();
                out.extend(tilo_delivery_protocol::transport_v2::encode_presence_batch(
                    room, &items,
                )?);
            }
        } else {
            for presence in run {
                let frame = Envelope::Presence {
                    workspace_id: room,
                    payload: presence.payload.to_vec(),
                };
                out.extend(encode_for_session(&frame, session)?);
            }
        }
        let bytes = u64::try_from(out.len().saturating_sub(before)).unwrap_or(u64::MAX);
        session.metrics.add_room_egress(room, bytes, true);
    }
    Ok(out)
}

#[expect(
    clippy::significant_drop_tightening,
    reason = "the flagged value is a shared workspace handle (an Arc clone); dropping it earlier releases no lock"
)]
async fn write_state_delivery<W: tokio::io::AsyncWrite + Unpin>(
    delivery: StateDelivery,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    coding: &mut SessionCoding,
    presence: &crate::presence_mailbox::PresenceMailbox,
    write: &mut W,
) -> Result<(), PeerError> {
    let Envelope::Delta { workspace_id, .. } = &*delivery.envelope else {
        return Ok(());
    };
    let workspace = session
        .workspaces
        .lock()
        .await
        .get(workspace_id)
        .cloned()
        .ok_or_else(|| PeerError::Protocol("state workspace disappeared".into()))?;
    if crate::persistence::correlated_diagnostics()
        && let Envelope::Delta {
            workspace_id,
            authored_at_ns,
            ..
        } = &*delivery.envelope
    {
        tracing::info!(phase="server_delta_writer", workspace_id, authored_at_ns, recipient=%hex::encode(&peer.device_public_key[..4]));
    }
    let mut runtime = crate::workspace_io::fresh_membership(&workspace).await?;
    // A connection can unsubscribe and subscribe again on the same channel.
    // Check the captured lease after the membership wait, before either writing
    // or fencing: stale work must not affect the replacement subscription.
    if !session.admits(&runtime, peer, RoomOperation::State)
        || !delivery.current_subscription(&runtime, peer, session.outbound)
    {
        return Ok(());
    }
    if delivery
        .source
        .as_ref()
        .is_some_and(|source| !source.authorized(&runtime))
    {
        if matches!(&delivery.envelope, LiveEnvelope::Selected(_)) {
            // CURRENT already committed this activation. A missing half cannot
            // be retried live after semantic suppression; reconnect needs the
            // durable basis. The exact recipient lease was checked above.
            let _ = session.disconnect.try_send(());
            runtime.remove_subscriber_session(&peer.device_public_key, session.outbound);
        }
        return Ok(());
    }
    drop(runtime);
    if crate::persistence::correlated_diagnostics()
        && let Envelope::Delta {
            workspace_id,
            authored_at_ns,
            ..
        } = &*delivery.envelope
    {
        tracing::info!(phase="server_delta_wire", workspace_id, authored_at_ns, recipient=%hex::encode(&peer.device_public_key[..4]));
    }
    // One write, so one packet where it fits: the delta, its position, and
    // the presence that may ride along, above all the author's caret and
    // typing for this keystroke, held for it (TIL-776).
    let mut out = encode_delta_charged(&delivery.envelope, session, &mut coding.deltas)?;
    if let Some(position) = resume_record(session, *workspace_id, [&*delivery.envelope]) {
        out.extend(encode_charged(&position, session, false)?);
    }
    out.extend(
        encode_presence(
            presence.take(tokio::time::Instant::now(), true),
            &mut coding.presence,
            peer,
            session,
        )
        .await?,
    );
    write.write_all(&out).await?;
    trace_native_publication("written", &delivery.envelope, Some(&peer.device_public_key));
    if let Some(path) = &delivery.cleanup {
        let path = path.clone();
        // Enqueue is never receipt. Exact cleanup is admitted only after the
        // complete frame write, and is ordered against commits/replay.
        let owner = crate::persistence::handle(&workspace).await;
        let runtime = crate::workspace_io::fresh_membership(&workspace).await?;
        if session.admits(&runtime, peer, RoomOperation::State)
            && delivery.current_subscription(&runtime, peer, session.outbound)
        {
            owner.read(1, move |_| {
                if !delivery.live.load(std::sync::atomic::Ordering::SeqCst) {
                    return Ok(());
                }
                match std::fs::remove_file(path) {
                    Ok(()) => Ok(()),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(e) => Err(e.into()),
                }
            })?;
        }
    }
    Ok(())
}

async fn handle_delta(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    env: Envelope,
) -> Result<Option<Envelope>, PeerError> {
    let Envelope::Delta {
        workspace_id,
        author_device_public_key,
        ..
    } = &env
    else {
        return Err(PeerError::Protocol(
            "handle_delta called with a non-Delta envelope".into(),
        ));
    };
    let workspace_id = *workspace_id;
    if !subscribed.contains(&workspace_id) {
        return Err(PeerError::Protocol(
            "delta sent before subscribing to workspace".into(),
        ));
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            reason: "workspace not bound on this relay".into(),
        }));
    };
    if peer.role == crate::auth::PeerRole::Device
        && author_device_public_key != &peer.device_public_key
    {
        return Err(PeerError::Protocol(
            "author does not match authenticated device".into(),
        ));
    }
    let Some(source) = authorized_state_source(&ws, peer, session).await? else {
        trace_native_publication("server_source_refused", &env, None);
        // A live revoke already pushed NotAuthorised and dropped this
        // subscriber; a follow-up write must close rather than refuse twice.
        if !current_session(&*ws.lock().await, peer, session.outbound) {
            return Err(PeerError::Protocol(
                "subscriber authorization changed".into(),
            ));
        }
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            reason: "workspace membership changed".into(),
        }));
    };
    trace_native_publication("server_authorized", &env, None);
    // The change itself must say what it does and stay inside what its app
    // publishes, before the relay keeps or forwards it.
    let standings = ws.lock().await.standings.clone();
    if let Envelope::Delta { delta, .. } = &env
        && let Some(reason) = crate::surfaces::refusal(delta, standings.as_deref())
    {
        // Neither kept nor forwarded. The session stays: one bad change
        // is not a membership decision.
        tracing::warn!(%reason, device = %hex::encode(peer.device_public_key), "refused a document change");
        return Ok(None);
    }
    let observed = match (&env, session.compactor) {
        (Envelope::Delta { delta, .. }, Some(_)) => crate::compaction::Compactor::observe(delta),
        _ => None,
    };
    let admitting = session
        .compactor
        .zip(observed)
        .map(|(compactor, observed)| compactor.admitting(workspace_id, observed));
    // The tap hands the body to another task, so it needs the one owned copy;
    // it is taken after the workspace lock is released.
    let governor_tap = {
        let mut runtime = ws.lock().await;
        if peer.role == crate::auth::PeerRole::Device {
            // The caret and typing this device sends next wait for this delta.
            runtime.note_delta(peer.device_public_key, tokio::time::Instant::now());
        }
        runtime.governor_tap.clone()
    };
    let governor = governor_tap.and_then(|tap| {
        let Envelope::Delta { delta, .. } = &env else {
            return None;
        };
        let (_, _, body) = crate::coedit::retention_view(delta)?;
        Some((tap, body.to_vec()))
    });
    enqueue_state(&ws, env, Some(source), peer.role, session.metrics.clone()).await?;
    if let Some((tap, body)) = governor {
        let _ = tap.try_send((peer.device_public_key, body));
    }
    if let (Some(compactor), Some(observed)) = (session.compactor, observed) {
        compactor.note(&ws, workspace_id, observed);
    }
    drop(admitting);

    Ok(None)
}

/// A member volunteering to compact sealed routes, or its coverage
/// statement for one: the host removes the entries the statement names. A
/// refused statement removes nothing and only costs the member its snapshot
/// upload.
async fn handle_compaction(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    env: Envelope,
) {
    let Some(compactor) = session.compactor else {
        return;
    };
    let (Envelope::CompactVolunteer { workspace_id } | Envelope::Compacted { workspace_id, .. }) =
        env
    else {
        return;
    };
    if !subscribed.contains(&workspace_id) || peer.role != crate::auth::PeerRole::Device {
        return;
    }
    let Envelope::Compacted {
        statement,
        signature,
        ..
    } = env
    else {
        compactor.volunteer(workspace_id, peer.device_public_key);
        return;
    };
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return;
    };
    if let Err(error) = compactor
        .compacted(
            &ws,
            workspace_id,
            peer.device_public_key,
            &statement,
            &signature,
        )
        .await
    {
        tracing::warn!(workspace_id, %error, "refused a compaction statement");
    }
}

/// Handle an inbound `Commit`: admit it exactly like a `Delta` (the reply
/// waits until the host persisted it), then tell the author its host order.
/// A commit the host cannot admit is refused to the author without closing
/// the session. A resent commit the host already admitted is answered with
/// its original order from the workspace's commit ledger and neither
/// persisted nor fanned out again.
async fn handle_commit(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    env: Envelope,
) -> Result<Option<Envelope>, PeerError> {
    let Envelope::Commit {
        workspace_id,
        op,
        author_device_public_key,
        authored_at_ns,
        delta,
    } = env
    else {
        return Err(PeerError::Protocol(
            "handle_commit called with a non-Commit envelope".into(),
        ));
    };
    let refused = |reason: &str| {
        Ok(Some(Envelope::CommitRefused {
            workspace_id,
            op,
            reason: reason.into(),
        }))
    };
    if !subscribed.contains(&workspace_id) {
        return refused("commit sent before subscribing to workspace");
    }
    // The relay's own device takes part in its rooms' channels like any
    // other member: its self-connection commits as that device.
    let device =
        peer.role == crate::auth::PeerRole::Device || &peer.cert == session.own_device_cert;
    if !device || author_device_public_key != peer.device_public_key {
        return refused("author does not match authenticated device");
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return refused("workspace not bound on this relay");
    };
    let Some(source) = authorized_state_source(&ws, peer, session).await? else {
        return refused("workspace membership changed");
    };
    let digest = *blake3::hash(&delta).as_bytes();
    let gate = ws.lock().await.commit_gate.clone();
    let _admitting = gate.lock().await;
    let admitted = ws
        .lock()
        .await
        .admitted_commit(&author_device_public_key, &op);
    if let Some(admitted) = admitted {
        return Ok(Some(resent_commit(workspace_id, admitted, digest)));
    }
    let observed = session
        .compactor
        .and_then(|_| crate::compaction::Compactor::observe(&delta));
    let admitting = session
        .compactor
        .zip(observed)
        .map(|(compactor, observed)| compactor.admitting(workspace_id, observed));
    let delta = Envelope::Delta {
        workspace_id,
        author_device_public_key,
        authored_at_ns,
        delta,
        signature: [0; 64],
    };
    let authored = delta.clone();
    let admitted = Box::pin(enqueue_state_inner(
        &ws,
        delta,
        Some(source),
        peer.role,
        session.metrics.clone(),
        false,
    ))
    .await?;
    // What an author is told is committed never depends on work that can
    // still fail: wait for the change to be durable and published.
    crate::persistence::receipt(admitted.completion).await?;
    let durable = admitted.sequence.load(std::sync::atomic::Ordering::SeqCst);
    if let (Some(compactor), Some(observed)) = (session.compactor, observed) {
        compactor.note(&ws, workspace_id, observed);
    }
    drop(admitting);
    // The author holds what it committed; a resume never replays it to it.
    if let Some(position) = resume_record(session, workspace_id, [&authored]) {
        let _ = session.outbound.try_send(position);
    }
    let seq = remember_commit(
        &ws,
        crate::store::CommitRecord {
            author: author_device_public_key,
            op,
            seq: durable,
            digest,
        },
    )
    .await?;
    Ok(Some(Envelope::Committed {
        workspace_id,
        op,
        seq,
    }))
}

/// The answer to a resent commit the ledger already holds: its original
/// host order, or a refusal when the same operation id carries other content.
fn resent_commit(
    workspace_id: u64,
    admitted: crate::store::CommitRecord,
    digest: [u8; 32],
) -> Envelope {
    if admitted.digest != digest {
        return Envelope::CommitRefused {
            workspace_id,
            op: admitted.op,
            reason: "operation id already committed with different content".into(),
        };
    }
    tracing::debug!(
        workspace_id,
        seq = admitted.seq,
        "resent commit answered from the ledger"
    );
    Envelope::Committed {
        workspace_id,
        op: admitted.op,
        seq: admitted.seq,
    }
}

/// Record an admitted commit in the workspace's commit ledger and return the
/// host order it is acknowledged at: the durable sequence it was published
/// in (`record.seq`), or the next one when persistence assigned none. Every
/// member then receives commits in the order their authors were told.
async fn remember_commit(
    ws: &SharedWorkspace,
    mut record: crate::store::CommitRecord,
) -> Result<u64, PeerError> {
    let (ledger, store) = {
        let mut runtime = ws.lock().await;
        if record.seq == 0 {
            record.seq = runtime.next_seq(unix_now_ns());
        }
        (runtime.remember_commit(record), runtime.store.clone())
    };
    tokio::task::spawn_blocking(move || ledger.persist(&store))
        .await
        .map_err(|_| PeerError::Io(std::io::Error::other("commit ledger write panicked")))?;
    Ok(record.seq)
}

/// Handle `LiveJoin`: add a subscribed device to the workspace's live plane
/// and send it one `LiveMember` per member already present.
async fn handle_live_join(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    workspace_id: u64,
) -> Result<Option<Envelope>, PeerError> {
    let refused = || {
        Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            reason: "live join requires a subscription".into(),
        }))
    };
    if !subscribed.contains(&workspace_id) {
        return refused();
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return refused();
    };
    let Some(members) = ws
        .lock()
        .await
        .live_join(peer.device_public_key, workspace_id)
    else {
        return refused();
    };
    for device in members {
        // Live state is never authority: a member notice lost to a full
        // queue costs only a late cursor.
        let _ = session.outbound.try_send(Envelope::LiveMember {
            workspace_id,
            device_public_key: device,
            active: true,
        });
    }
    Ok(None)
}

/// Handle a call envelope for a subscribed workspace whose room this relay
/// governs; see [`crate::rooms`].
#[allow(
    clippy::significant_drop_tightening,
    reason = "the guard only lives for the lookup; the value kept is a clone"
)]
async fn handle_voice(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    env: Envelope,
) -> Result<Option<Envelope>, PeerError> {
    let workspace_id = match &env {
        Envelope::VoiceCall { workspace_id, .. }
        | Envelope::VoiceJoin { workspace_id, .. }
        | Envelope::VoiceLeave { workspace_id, .. }
        | Envelope::VoiceCalls { workspace_id } => *workspace_id,
        _ => return Ok(None),
    };
    let authority = if subscribed.contains(&workspace_id) {
        let ws = session.workspaces.lock().await.get(&workspace_id).cloned();
        match ws {
            Some(ws) => ws.lock().await.room_authority.clone(),
            None => None,
        }
    } else {
        None
    };
    let rooms = authority
        .as_ref()
        .and_then(|authority| Some((authority.rooms()?, authority.room().to_owned())));
    let refusal = "calls require a subscription to a room this host governs";
    Ok(match env {
        Envelope::VoiceCall {
            request, operation, ..
        } => {
            let result = match &rooms {
                Some((rooms, room)) => rooms.voice_call(room, peer, &operation).await,
                None => Err(refusal.into()),
            };
            Some(Envelope::VoiceCallResult {
                workspace_id,
                request,
                result,
            })
        }
        Envelope::VoiceJoin {
            call,
            participation,
            ..
        } => {
            let result = match &rooms {
                Some((rooms, room)) => {
                    rooms
                        .voice_join(room, peer, call, participation, session.outbound.clone())
                        .await
                }
                None => Err(refusal.into()),
            };
            Some(match result {
                Ok(grant) => Envelope::VoiceGranted {
                    workspace_id,
                    grant: Box::new(grant),
                },
                Err(reason) => Envelope::VoiceRefused {
                    workspace_id,
                    call,
                    reason,
                },
            })
        }
        Envelope::VoiceLeave {
            call,
            participation,
            ..
        } => {
            if let Some((rooms, _)) = &rooms {
                rooms.voice_leave(peer, call, participation);
            }
            None
        }
        Envelope::VoiceCalls { .. } => Some(Envelope::VoiceCallList {
            workspace_id,
            result: match &rooms {
                Some((rooms, room)) => rooms.voice_calls(room, peer).await,
                None => Err(refusal.into()),
            },
        }),
        _ => None,
    })
}

/// Handle `LiveLeave`.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the guard only lives for the lookup; the value kept is a clone"
)]
async fn handle_live_leave(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    workspace_id: u64,
) -> Result<Option<Envelope>, PeerError> {
    let ws = session.workspaces.lock().await.get(&workspace_id).cloned();
    if let Some(ws) = ws {
        let mut ws = ws.lock().await;
        ws.live_leave(&peer.device_public_key, workspace_id);
        drop(ws);
    }
    Ok(None)
}

/// Handle an inbound `Live` frame: decode it canonically, stamp the
/// authenticated sender and relay it within the sender's rate budget.
/// Invalid, over-budget or non-member frames are dropped: live traffic is
/// never authority, and dropping it costs only a stale cursor.
async fn handle_live(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    workspace_id: u64,
    frame: Vec<u8>,
) -> Result<Option<Envelope>, PeerError> {
    if !subscribed.contains(&workspace_id) {
        return Ok(None);
    }
    // Only a sealed frame is relayed; its content is never read here.
    let Ok(sealed) = crate::live::SealedLiveFrame::decode(&frame) else {
        return Ok(None);
    };
    let presence = sealed.kind == crate::live::LiveKind::Presence;
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return Ok(None);
    };
    if let Some(bridge) = session.live_bridge
        && ws.lock().await.is_live_member(&peer.device_public_key)
    {
        bridge.live(workspace_id, peer.device_public_key, &frame);
    }
    let stamped = Envelope::Live {
        workspace_id,
        from_device_public_key: peer.device_public_key,
        frame,
    };
    ws.lock()
        .await
        .fanout_live(&peer.device_public_key, presence, &stamped, unix_now_ns());
    Ok(None)
}

async fn handle_delta_prefix(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    envelopes: Vec<Envelope>,
    inbound_budget: Vec<Option<Arc<tokio::sync::OwnedSemaphorePermit>>>,
) -> Result<Option<Envelope>, PeerError> {
    let Some(Envelope::Delta {
        workspace_id,
        author_device_public_key,
        ..
    }) = envelopes.first()
    else {
        return Err(PeerError::Protocol(
            "checkpoint prefix started with a non-delta".into(),
        ));
    };
    if !subscribed.contains(workspace_id) {
        return Err(PeerError::Protocol(
            "delta sent before subscribing to workspace".into(),
        ));
    }
    let Some(workspace) = session.workspaces.lock().await.get(workspace_id).cloned() else {
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id: *workspace_id,
            reason: "workspace not bound on this relay".into(),
        }));
    };
    if peer.role == crate::auth::PeerRole::Device
        && author_device_public_key != &peer.device_public_key
    {
        return Err(PeerError::Protocol(
            "author does not match authenticated device".into(),
        ));
    }
    let Some(source) = authorized_state_source(&workspace, peer, session).await? else {
        for envelope in &envelopes {
            trace_native_publication("server_source_refused", envelope, None);
        }
        if !current_session(&*workspace.lock().await, peer, session.outbound) {
            return Err(PeerError::Protocol(
                "subscriber authorization changed".into(),
            ));
        }
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id: *workspace_id,
            reason: "workspace membership changed".into(),
        }));
    };
    for envelope in &envelopes {
        trace_native_publication("server_authorized", envelope, None);
    }
    let governed_source = {
        let runtime = workspace.lock().await;
        checkpoint_prefix_recovery_allowed(&runtime, peer, *author_device_public_key, &source)
    };
    if !governed_source {
        for (envelope, budget) in envelopes.into_iter().zip(inbound_budget) {
            enqueue_state(
                &workspace,
                envelope,
                Some(source.clone()),
                peer.role,
                session.metrics.clone(),
            )
            .await?;
            drop(budget);
        }
        return Ok(None);
    }
    enqueue_checkpoint_prefix(
        &workspace,
        envelopes,
        source,
        peer.role,
        session.metrics.clone(),
        inbound_budget,
    )
    .await?;
    Ok(None)
}

async fn authorized_state_source(
    workspace: &SharedWorkspace,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<StateSource>, PeerError> {
    let source = StateSource::new(peer, session);
    let runtime = crate::workspace_io::fresh_membership(workspace).await?;
    Ok(source.authorized(&runtime).then_some(source))
}

fn checkpoint_prefix_recovery_allowed(
    runtime: &crate::workspace::WorkspaceRuntime,
    peer: &AuthenticatedPeer,
    author_device_public_key: [u8; 32],
    source: &StateSource,
) -> bool {
    runtime.checkpoint_prefix_recovery
        == crate::config::CheckpointPrefixRecovery::NativeOwnerDurableBaseline
        && peer.role == crate::auth::PeerRole::Device
        && author_device_public_key == peer.device_public_key
        && runtime
            .store
            .shell_authority()
            .is_some_and(|authority| authority.governor() == &peer.cert)
        && source.authorized(runtime)
}

/// The coalescing slot of a co-edit presence body (TIL-710): latest-state
/// presence (a pointer, a caret) has one, events do not. Only the two
/// `NeighbourMsg` variants that can have one are decoded, to tell which;
/// the body is forwarded as it came.
fn presence_slot(body: &[u8]) -> Option<tilo_peer_state::PresenceSlot> {
    if !body
        .first()
        .is_some_and(|tag| tilo_peer_state::NeighbourMsg::SLOTTED_PRESENCE_TAGS.contains(tag))
    {
        return None;
    }
    postcard::from_bytes::<tilo_peer_state::NeighbourMsg>(body)
        .ok()?
        .presence_slot()
}

/// Handle an inbound `Presence`: fan it out to every other subscriber.
/// Ephemeral, with device attribution stamped from the authenticated session.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the flagged value is a shared workspace handle (an Arc clone); dropping it earlier releases no lock"
)]
async fn handle_presence(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    mut env: Envelope,
) -> Result<Option<Envelope>, PeerError> {
    let mut slot = None;
    let Envelope::Presence { workspace_id, .. } = &env else {
        return Err(PeerError::Protocol(
            "handle_presence called with a non-Presence envelope".into(),
        ));
    };
    let workspace_id = *workspace_id;
    if !subscribed.contains(&workspace_id) {
        return Err(PeerError::Protocol(
            "presence sent before subscribing to workspace".into(),
        ));
    }
    if peer.role == crate::auth::PeerRole::Device
        && let Envelope::Presence { payload, .. } = &mut env
        && let Some(mut attributed) = crate::AttributedPresence::decode(payload)
    {
        // Co-edit receivers recover the author from this opaque wrapper.
        // A sender's claim is not transport evidence, even if that other
        // device has a valid certificate in the same workspace.
        attributed.author = peer.device_public_key;
        slot = presence_slot(&attributed.body);
        *payload = attributed
            .encode()
            .map_err(|error| PeerError::Protocol(error.to_string()))?;
    }
    let ws = {
        let map = session.workspaces.lock().await;
        map.get(&workspace_id).cloned()
    };
    let mut legacy_writer = false;
    if let Some(ws) = ws {
        if session.live_bridge.is_some() {
            let authority = {
                let runtime = ws.lock().await;
                if runtime.has_subscriber(&peer.device_public_key)
                    && session_peer_authorized(peer, session)
                {
                    legacy_writer = true;
                }
                runtime.room_authority.clone()
            };
            if let Some(authority) = authority {
                legacy_writer &= match authority.rooms() {
                    Some(rooms) => rooms
                        .legacy_writer_role(authority.room(), peer.device_public_key)
                        .await
                        .is_some_and(room_access::role_writes),
                    None => false,
                };
            }
        }
        let mut wr = ws.lock().await;
        // A device sends presence only to say hello, to ask for a baseline
        // (`SyncReq`) or after rejecting a checkpoint — each means it needs the
        // next live checkpoint (the per-device checkpoint gate in
        // `fanout_delta`).
        if peer.role == crate::auth::PeerRole::Device {
            wr.request_checkpoint(&peer.device_public_key);
        }
        wr.fanout_presence(&peer.device_public_key, peer.role, &env, slot);
    }
    if legacy_writer
        && let (Some(bridge), crate::auth::PeerRole::Device, Envelope::Presence { payload, .. }) =
            (session.live_bridge, peer.role, &env)
    {
        bridge.presence(
            workspace_id,
            peer.device_public_key,
            peer.user_public_key,
            payload,
        );
    }
    Ok(None)
}

/// Where an inbound [`Envelope::RemoteAction`] asked to go.
struct RemoteActionRoute {
    workspace_id: u64,
    version: u16,
    to: [u8; 32],
}

/// Handle an inbound `RemoteAction`: stamp the authenticated sender, then
/// hand it to the addressed device's live connection, or — when addressed to
/// this relay — to the app-serving plane. Never stored, never executed by the
/// relay itself; every failure is answered as undeliverable so the sender
/// need not wait out a timeout, with a reason that leads with
/// [`UnavailableReason::wire_token`].
///
/// Both ends are re-authorised on every frame under a fresh membership read
/// with [`action_peer_admitted`], the rule the v2 private lane uses: the
/// connection-local `subscribed` list alone goes stale the moment a device is
/// removed, revoked, or its agent delegation lapses.
async fn handle_remote_action(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    route: RemoteActionRoute,
    frame: Vec<u8>,
) -> Option<Envelope> {
    use UnavailableReason as U;
    let RemoteActionRoute {
        workspace_id,
        version,
        to,
    } = route;
    let refuse = |reason: UnavailableReason, detail: &str| {
        Some(Envelope::RemoteActionUndeliverable {
            workspace_id,
            to_device_public_key: to,
            reason: format!("{}: {detail}", reason.wire_token()),
        })
    };
    if version != crate::transport::REMOTE_ACTION_ENVELOPE_VERSION {
        return refuse(
            U::InvalidRequest,
            "unsupported remote action envelope version",
        );
    }
    if !subscribed.contains(&workspace_id) {
        return refuse(
            U::NotAuthorised,
            "sender is not subscribed to the workspace",
        );
    }
    if frame.len() > tilo_actions::remote::MAX_REMOTE_ACTION_FRAME_BYTES {
        return refuse(U::InvalidRequest, "remote action frame exceeds the limit");
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return refuse(U::NotAuthorised, "workspace is not bound on this relay");
    };
    let Ok(mut wr) = crate::workspace_io::fresh_membership(&ws).await else {
        return refuse(
            U::AdmissionUnavailable,
            "workspace membership is unreadable",
        );
    };
    if action_peer_admitted(&wr, peer, session).is_err()
        || !current_session(&wr, peer, session.outbound)
    {
        return refuse(U::NotAuthorised, "sender may not act in the workspace");
    }
    if to == *session.own_device_public_key {
        // Never hold the room across the plane: it may take its time.
        drop(wr);
        let Some(plane) = session.app_serving else {
            return refuse(U::UnsupportedPeer, "this host does not serve apps");
        };
        return serve_locally(peer, session, plane.as_ref(), workspace_id, &frame)
            .unwrap_or_else(|(reason, detail)| refuse(reason, detail));
    }
    // Every production subscription registers its authenticated peer beside
    // its subscriber, so the addressee is re-authorised from it.
    let Some(target) = wr.state_sessions.get(&to) else {
        return refuse(
            unavailable_target(&wr, &to),
            "addressed device is not connected to the workspace",
        );
    };
    if action_peer_admitted(&wr, &target.peer, session).is_err() {
        return refuse(
            U::NotAuthorised,
            "addressed device may not act in the workspace",
        );
    }
    let stamped = Envelope::RemoteAction {
        workspace_id,
        version,
        to_device_public_key: to,
        from_device_public_key: peer.device_public_key,
        from_user_public_key: peer.user_public_key,
        frame,
    };
    let delivered = wr.route_directed(&to, stamped);
    drop(wr);
    if delivered {
        None
    } else {
        refuse(
            U::Offline,
            "addressed device is not connected to the workspace",
        )
    }
}

/// Hand a piece of a record's content to the one device it is addressed
/// to, stamped with the authenticated sender. Never stored, never fanned
/// out, never answered: a piece that cannot be delivered is dropped, and
/// the device that wanted it asks again.
async fn route_record_content(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    envelope: Envelope,
) {
    let Envelope::RecordContent {
        workspace_id,
        to_device_public_key: to,
        frame,
        ..
    } = envelope
    else {
        return;
    };
    if !subscribed.contains(&workspace_id)
        || frame.len() > crate::transport::MAX_RECORD_CONTENT_FRAME_BYTES
    {
        return;
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return;
    };
    let stamped = Envelope::RecordContent {
        workspace_id,
        to_device_public_key: to,
        from_device_public_key: peer.device_public_key,
        frame,
    };
    ws.lock().await.route_lossy(&to, stamped);
}

/// Answer a frame addressed to this relay through its app-serving plane,
/// with the caller the connection authenticated. The answer is written to
/// the connection whenever the plane has it, so a request that needs an
/// application's help never stalls this peer's other traffic. `Err` when
/// the frame cannot be used. The caller was already admitted for the room
/// by [`handle_remote_action`], agent delegation included.
fn serve_locally(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    plane: &dyn crate::app_serving::AppServingPlane,
    workspace_id: u64,
    frame: &[u8],
) -> Result<Option<Envelope>, (UnavailableReason, &'static str)> {
    use tilo_actions::remote::{Caller, RemoteActionFrame};
    let request = RemoteActionFrame::decode(frame).map_err(|_| {
        (
            UnavailableReason::InvalidRequest,
            "malformed remote action frame",
        )
    })?;
    let agent = session
        .agents
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&peer.device_public_key)
        .cloned();
    let caller = match agent {
        Some(Some(delegation)) => Caller::delegated_agent(
            delegation.agent_id,
            peer.user_public_key,
            peer.device_public_key,
        ),
        Some(None) => {
            return Err((UnavailableReason::NotAuthorised, "the agent was withdrawn"));
        }
        None => Caller::authenticated(peer.user_public_key, peer.device_public_key),
    };
    let Some(reply) = plane.serve(workspace_id, &caller, request.message) else {
        return Ok(None);
    };
    let outbound = session.outbound.clone();
    let to_device_public_key = peer.device_public_key;
    let from_device_public_key = *session.own_device_public_key;
    let from_user_public_key = *session.own_user_public_key;
    reply.then(move |reply| {
        let Some(frame) = reply.and_then(|reply| RemoteActionFrame::new(reply).encode().ok())
        else {
            return;
        };
        let _ = outbound.try_send(Envelope::RemoteAction {
            workspace_id,
            version: crate::transport::REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key,
            from_device_public_key,
            from_user_public_key,
            frame,
        });
    });
    Ok(None)
}

/// Handle `PeerRelayHello`: record the dialer as a federated peer
/// relay for every workspace we both host.
async fn handle_peer_relay_hello(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    hosted_workspaces: Vec<u64>,
) -> Result<Option<Envelope>, PeerError> {
    // Peer-relay handshake: silently ignore if the peer's cert is not
    // host-capable — otherwise a plain device could try to smuggle
    // its way into the federation set.
    if peer.role != crate::auth::PeerRole::PeerRelay
        || peer.user_public_key != *session.own_user_public_key
    {
        return Ok(None);
    }
    // Compute the intersection with what THIS relay hosts and record
    // the dialer's device pubkey as a federated peer relay for each
    // shared workspace. On next `Subscribe` (which the peer relay is
    // about to send for each intersection id), the existing
    // subscriber path enrols it with role `PeerRelay` and the
    // fan-out filter takes care of the rest.
    let mut hosted_here: Vec<u64> = Vec::new();
    let wsmap = session.workspaces.lock().await;
    for id in &hosted_workspaces {
        if let Some(ws) = wsmap.get(id).cloned() {
            ws.lock().await.record_peer_relay(peer.device_public_key);
            hosted_here.push(*id);
        }
    }
    drop(wsmap);
    Ok(Some(Envelope::PeerRelayHello {
        hosted_workspaces: hosted_here,
    }))
}

/// Handle `SyncRequest`: anti-entropy reply with exactly the changes
/// the requester is missing, computed against this host's
/// materialized pinned doc. Read-only — does not touch subscriptions
/// or the pending queues, so a peer can reconcile whether or not it
/// is currently subscribed. Membership gates device requesters the
/// same way `Subscribe` does; peer relays are gated by cross-user
/// auth + host capability.
#[expect(
    clippy::significant_drop_tightening,
    reason = "the membership guard keeps the roster fixed until the reply is classified"
)]
async fn handle_sync_request(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    workspace_id: u64,
    have_heads: Vec<[u8; 32]>,
) -> Result<Option<Envelope>, PeerError> {
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            reason: "workspace not bound on this relay".into(),
        }));
    };
    let wr = crate::workspace_io::fresh_membership(&ws).await?;
    if !session.admits(&wr, peer, RoomOperation::State) {
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            reason: "device not in workspace membership".into(),
        }));
    }
    let expected = wr
        .subscribers
        .get(&peer.device_public_key)
        .map(|sub| sub.tx.clone());
    let source = StateSource::new(peer, session);
    if let Some(mailbox) = &wr.sync_refresh
        && session.connection_live.is_some()
        && expected
            .as_ref()
            .is_some_and(|tx| tx.same_channel(session.outbound))
        && source.authorized(&wr)
    {
        mailbox.enqueue(workspace_id, ws.clone(), source.clone());
    }
    drop(wr);
    let owner = crate::persistence::handle(&ws).await;
    owner
        .submit_prepared_wait(
            have_heads.len().saturating_mul(32),
            async { Ok(()) },
            move |state, ()| Ok(state.changes(&have_heads)),
            move |result| async move {
                let changes = result?;
                let wr = crate::workspace_io::fresh_membership(&ws).await?;
                let same = match (
                    expected.as_ref(),
                    wr.subscribers.get(&source.peer.device_public_key),
                ) {
                    (None, None) => true,
                    (Some(expected), Some(current)) => expected.same_channel(&current.tx),
                    _ => false,
                };
                if source.alive()
                    && source.admits(&wr, &source.peer)
                    && same
                    && source
                        .tx
                        .try_send(Envelope::SyncResponse {
                            workspace_id,
                            delta_since: changes.clone(),
                        })
                        .is_err()
                {
                    let _ = source.disconnect.try_send(());
                }
                Ok(changes)
            },
        )
        .await?;
    Ok(None)
}

pub(crate) async fn get_or_autobind_workspace(
    workspaces: &WorkspaceRegistry,
    autobind: Option<&WorkspaceAutobind>,
    workspace_id: u64,
) -> Result<Option<SharedWorkspace>, StoreError> {
    let existing = workspaces.lock().await.get(&workspace_id).cloned();
    if let Some(workspace) = existing {
        return Ok(Some(workspace));
    }
    let Some(autobind) = autobind else {
        return Ok(None);
    };
    crate::workspace_io::get_or_initialize(
        workspaces,
        autobind.state_root.clone(),
        workspace_id,
        true,
        crate::room_access::AdmissionMode::SameOwner,
        MAX_AUTOBOUND_WORKSPACES,
    )
    .await
    .map(Some)
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
mod autobind_tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn autobind_open_releases_registry_lock_and_shares_one_runtime() {
        let tmp = tempfile::tempdir().unwrap();
        let registry = Arc::new(AsyncMutex::new(HashMap::new()));
        let autobind = WorkspaceAutobind {
            state_root: tmp.path().to_path_buf(),
        };
        let (release, reached) =
            crate::store::test_barrier::install(&tmp.path().join("workspaces/7"), "open");
        let observer_registry = registry.clone();
        let handle = tokio::runtime::Handle::current();
        let observer = std::thread::spawn(move || {
            handle.block_on(async move {
                reached.await.unwrap();
                let unlocked =
                    tokio::time::timeout(tilo_test_env::hang_bound(), observer_registry.lock())
                        .await
                        .is_ok();
                drop(release);
                unlocked
            })
        });
        let (first, second) = tokio::join!(
            get_or_autobind_workspace(&registry, Some(&autobind), 7),
            get_or_autobind_workspace(&registry, Some(&autobind), 7)
        );
        assert!(
            observer.join().unwrap(),
            "workspace open held registry lock"
        );
        assert!(Arc::ptr_eq(
            &first.unwrap().unwrap(),
            &second.unwrap().unwrap()
        ));
        assert_eq!(registry.lock().await.len(), 1);
    }

    #[test]
    fn automatic_workspace_creation_has_a_hard_capacity_bound() {
        assert!(autobind_has_capacity(MAX_AUTOBOUND_WORKSPACES - 1));
        assert!(!autobind_has_capacity(MAX_AUTOBOUND_WORKSPACES));
        assert!(!autobind_has_capacity(usize::MAX));
    }
}

/// A private queue belongs to one authenticated state session and exact room.
#[derive(Clone)]
pub(crate) struct ActionTarget {
    broker_host: Option<uuid::Uuid>,
    broker_protocol: bool,
    room: uuid::Uuid,
    peer: AuthenticatedPeer,
    state_tx: mpsc::Sender<Envelope>,
    tx: mpsc::Sender<PrivateOutbound>,
}

pub(crate) enum PrivateOutbound {
    Reply(EnvelopeV2),
    Delivery(PrivateDelivery),
}

#[derive(Clone)]
pub(crate) struct PrivateDelivery {
    certified: bool,
    action: DirectedAction,
    /// The source device's signature over `action`, forwarded unchanged.
    /// Boxed so a queued reply is not as large as a signed delivery.
    signed: Option<Box<SignedDirectedAction>>,
    source: AuthenticatedPeer,
    source_tx: mpsc::Sender<Envelope>,
    reply: mpsc::Sender<PrivateOutbound>,
    // Capture the originating session, never re-resolve by device key: that
    // key may already identify a newer replacement connection at delivery.
    source_disconnect: mpsc::Sender<()>,
}

fn current_action_peer(peer: &AuthenticatedPeer, session: &SessionCtx<'_>) -> bool {
    session_peer_authorized(peer, session)
        && (peer.role == crate::auth::PeerRole::Device || &peer.cert == session.own_device_cert)
}

fn session_peer_authorized(peer: &AuthenticatedPeer, session: &SessionCtx<'_>) -> bool {
    session.trust().cert(peer, unix_now_ns()).is_ok()
}

/// May this connection originate (or receive) a remote action in this room
/// right now? One rule for v1 `RemoteAction` and v2 `Directed`: a current,
/// unrevoked, trusted device certificate; an agent only inside its live
/// delegation; and current room membership. Whether it is the caller's own
/// session is a separate check, since only the caller's channel is known.
fn action_peer_admitted(
    wr: &crate::workspace::WorkspaceRuntime,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<(), UnavailableReason> {
    // Private authority is explicit and current: a same-owner room enrols an
    // owner device on Subscribe, never here.
    if !current_action_peer(peer, session) || !session.admits(wr, peer, RoomOperation::RemoteAction)
    {
        return Err(UnavailableReason::NotAuthorised);
    }
    Ok(())
}

fn unavailable_target(
    wr: &crate::workspace::WorkspaceRuntime,
    target: &[u8; 32],
) -> UnavailableReason {
    if wr.membership.contains(target) {
        UnavailableReason::Offline
    } else {
        UnavailableReason::NotAuthorised
    }
}

fn current_session(
    wr: &crate::workspace::WorkspaceRuntime,
    peer: &AuthenticatedPeer,
    tx: &mpsc::Sender<Envelope>,
) -> bool {
    wr.subscribers
        .get(&peer.device_public_key)
        .is_some_and(|sub| sub.tx.same_channel(tx))
}

// The UUID is carried on an authenticated private lane. Select only a live
// subscription on its derived shard; never infer membership from a numeric alias.
async fn private_workspace_binding(
    room: uuid::Uuid,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<(u64, SharedWorkspace)>, PeerError> {
    let id = crate::room_workspace_id(&room.to_string())
        .map_err(|_| PeerError::Protocol("invalid room".into()))?;
    let Some(workspace) = session.workspaces.lock().await.get(&id).cloned() else {
        return Ok(None);
    };
    let runtime = workspace.lock().await;
    if runtime.store.workspace_id() != id || !current_session(&runtime, peer, session.outbound) {
        return Ok(None);
    }
    drop(runtime);
    Ok(Some((id, workspace)))
}

async fn private_workspace(
    room: uuid::Uuid,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<SharedWorkspace>, PeerError> {
    Ok(private_workspace_binding(room, peer, session)
        .await?
        .map(|(_, workspace)| workspace))
}

async fn private_membership<'a>(
    workspace: &'a SharedWorkspace,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    failure: &str,
) -> Result<tokio::sync::MutexGuard<'a, crate::workspace::WorkspaceRuntime>, PeerError> {
    let runtime = crate::workspace_io::fresh_membership(workspace).await?;
    if action_peer_admitted(&runtime, peer, session).is_err()
        || !current_session(&runtime, peer, session.outbound)
    {
        return Err(PeerError::Protocol(failure.into()));
    }
    Ok(runtime)
}

async fn handle_private(
    frame: EnvelopeV2,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<EnvelopeV2>, PeerError> {
    match frame {
        EnvelopeV2::State(Envelope::Ping) => Ok(Some(EnvelopeV2::State(Envelope::Pong))),
        EnvelopeV2::EnableActions { room } => {
            if room.is_nil() || !current_action_peer(peer, session) {
                return Err(PeerError::Protocol(
                    "private capability not authorised".into(),
                ));
            }
            let Some(ws) = private_workspace(room, peer, session).await? else {
                return Err(PeerError::Protocol("private room not subscribed".into()));
            };
            let mut wr =
                private_membership(&ws, peer, session, "private room not subscribed").await?;
            wr.action_targets.insert(
                peer.device_public_key,
                ActionTarget {
                    broker_host: None,
                    broker_protocol: false,
                    room,
                    peer: peer.clone(),
                    state_tx: session.outbound.clone(),
                    tx: session.private_tx.clone(),
                },
            );
            drop(wr);
            Ok(Some(EnvelopeV2::ActionsReady {
                room,
                capability: ActionCapability::TransportOnly,
            }))
        }
        EnvelopeV2::EnableBrokerClient { room } => {
            if !current_action_peer(peer, session) {
                return Err(PeerError::Protocol("broker protocol not authorised".into()));
            }
            let ws = private_workspace(room, peer, session)
                .await?
                .ok_or_else(|| PeerError::Protocol("room not subscribed".into()))?;
            let mut wr =
                private_membership(&ws, peer, session, "broker protocol not authorised").await?;
            let target = wr
                .action_targets
                .get_mut(&peer.device_public_key)
                .filter(|t| t.room == room && t.state_tx.same_channel(session.outbound))
                .ok_or_else(|| PeerError::Protocol("private lane not enabled".into()))?;
            target.broker_protocol = true;
            drop(wr);
            Ok(None)
        }
        EnvelopeV2::RegisterBroker { room, host } => {
            if host.is_nil() || !current_action_peer(peer, session) {
                return Err(PeerError::Protocol("broker not authorised".into()));
            }
            let ws = private_workspace(room, peer, session)
                .await?
                .ok_or_else(|| PeerError::Protocol("room not subscribed".into()))?;
            let mut wr = private_membership(&ws, peer, session, "broker not authorised").await?;
            let target = wr
                .action_targets
                .get_mut(&peer.device_public_key)
                .filter(|t| t.room == room && t.state_tx.same_channel(session.outbound))
                .ok_or_else(|| PeerError::Protocol("private lane not enabled".into()))?;
            target.broker_host = Some(host);
            target.broker_protocol = true;
            drop(wr);
            Ok(Some(EnvelopeV2::ActionsReady {
                room,
                capability: ActionCapability::DurableBroker { host },
            }))
        }
        EnvelopeV2::Directed(action) => {
            let reason = route_private(&action, peer, session).await?;
            Ok(reason.map(|reason| EnvelopeV2::unavailable(&action, reason)))
        }
        // The relay forwards the source's signed bytes; it never signs. A
        // signature that is not this session's device's is refused here, so
        // it cannot reach (and close) the target's attachment.
        EnvelopeV2::SignedDirected(signed) => {
            let action = signed.action();
            if signed.verify(&peer.device_public_key).is_err() {
                private_phase(action, "route_rejected_signature");
                return Ok(Some(EnvelopeV2::unavailable(
                    action,
                    UnavailableReason::NotAuthorised,
                )));
            }
            let reason = route_private_signed(action, Some(&signed), peer, session).await?;
            Ok(reason.map(|reason| EnvelopeV2::unavailable(action, reason)))
        }
        // Peers may send private results only inside a source-bound Directed
        // frame. Relay control responses can never be forged by a peer.
        _ => Err(PeerError::Protocol(
            "unexpected private control frame".into(),
        )),
    }
}

fn private_phase(action: &DirectedAction, phase: &'static str) {
    private_workspace_phase(action, None, phase);
}

fn private_workspace_phase(
    action: &DirectedAction,
    workspace_id: Option<u64>,
    phase: &'static str,
) {
    if crate::persistence::diagnostics() {
        tracing::info!(
            request_id = %action.request_id,
            workspace_id = ?workspace_id,
            phase,
            timestamp_ns = unix_now_ns()
        );
    }
}

struct DeliveryAuthorizationTrace<'a> {
    action: &'a DirectedAction,
    workspace_id: Option<u64>,
    finished: bool,
}

impl<'a> DeliveryAuthorizationTrace<'a> {
    fn new(action: &'a DirectedAction) -> Self {
        Self {
            action,
            workspace_id: None,
            finished: false,
        }
    }

    fn select_workspace(&mut self, workspace_id: u64) {
        self.workspace_id = Some(workspace_id);
    }

    fn phase(&self, phase: &'static str) {
        private_workspace_phase(self.action, self.workspace_id, phase);
    }

    fn reject(
        &mut self,
        phase: &'static str,
        reason: UnavailableReason,
    ) -> Result<(), UnavailableReason> {
        self.phase(phase);
        self.finished = true;
        Err(reason)
    }

    fn success(&mut self) {
        self.phase("delivery_authorization_success");
        self.finished = true;
    }
}

impl Drop for DeliveryAuthorizationTrace<'_> {
    fn drop(&mut self) {
        if !self.finished {
            self.phase("delivery_authorization_cancelled");
        }
    }
}

async fn route_private(
    action: &DirectedAction,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<UnavailableReason>, PeerError> {
    route_private_signed(action, None, peer, session).await
}

/// Route `action`, carrying the source's verified signature over it when
/// there is one so a target that negotiated signed actions receives it.
async fn route_private_signed(
    action: &DirectedAction,
    signed: Option<&SignedDirectedAction>,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<UnavailableReason>, PeerError> {
    use UnavailableReason as U;
    private_phase(action, "route_enter");
    if action.source != peer.device_public_key || !current_action_peer(peer, session) {
        private_phase(action, "route_rejected_source");
        return Ok(Some(U::NotAuthorised));
    }
    if !action.validate() {
        private_phase(action, "route_rejected_message");
        return Ok(Some(U::InvalidRequest));
    }
    if tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::Directed(action.clone())).is_err()
    {
        private_phase(action, "route_rejected_encoding");
        return Ok(Some(U::InvalidRequest));
    }
    let Some((_, ws)) = private_workspace_binding(action.room, peer, session).await? else {
        return Ok(Some(U::NotAuthorised));
    };
    #[cfg(test)]
    {
        let root = ws.lock().await.store.root().to_owned();
        crate::store::test_barrier::pause(&root, "private-authorization");
    }
    let wr = crate::workspace_io::fresh_membership(&ws).await?;
    private_phase(action, "route_lock_acquired");
    if action_peer_admitted(&wr, peer, session).is_err()
        || !current_session(&wr, peer, session.outbound)
        || !wr
            .action_targets
            .get(&peer.device_public_key)
            .is_some_and(|target| {
                target.room == action.room && target.state_tx.same_channel(session.outbound)
            })
    {
        return Ok(Some(U::NotAuthorised));
    }
    let Some(sub) = wr.subscribers.get(&action.target) else {
        return Ok(Some(unavailable_target(&wr, &action.target)));
    };
    let Some(target) = wr.action_targets.get(&action.target) else {
        return Ok(Some(U::UnsupportedPeer));
    };
    if action_peer_admitted(&wr, &target.peer, session).is_err()
        || target.room != action.room
        || !target.state_tx.same_channel(&sub.tx)
    {
        return Ok(Some(U::NotAuthorised));
    }
    // Invocations require the exact host identity the recipient advertised.
    if matches!(&action.message, ActionMessage::InvokeBound { host, .. } if target.broker_host != Some(*host))
    {
        return Ok(Some(U::AdmissionUnavailable));
    }
    let source_extended = wr
        .action_targets
        .get(&peer.device_public_key)
        .is_some_and(|t| t.broker_protocol);
    let extension = matches!(
        action.message,
        ActionMessage::InvokeBound { .. }
            | ActionMessage::CommittedDesktop { .. }
            | ActionMessage::BrokerCatalogue { .. }
            | ActionMessage::Observation { .. }
            | ActionMessage::Rejected { .. }
    );
    if (target.broker_host.is_some() && !source_extended)
        || (extension && (!source_extended || !target.broker_protocol))
    {
        return Ok(Some(U::UnsupportedPeer));
    }
    // Checked above under the same membership read; keep the same refusal.
    if !wr.action_targets.contains_key(&peer.device_public_key) {
        return Ok(Some(U::NotAuthorised));
    }
    private_phase(action, "route_authorized");
    let delivery = PrivateDelivery {
        action: action.clone(),
        signed: signed.cloned().map(Box::new),
        source: peer.clone(),
        source_tx: session.outbound.clone(),
        reply: session.private_tx.clone(),
        source_disconnect: session.disconnect.clone(),
        certified: target.broker_host.is_some(),
    };
    let result = target
        .tx
        .try_send(PrivateOutbound::Delivery(delivery))
        .err()
        .map(|error| match error {
            mpsc::error::TrySendError::Full(_) => U::Backpressure,
            mpsc::error::TrySendError::Closed(_) => U::Offline,
        });
    drop(wr);
    if result.is_none() {
        private_phase(action, "route_enqueued");
    }
    Ok(result)
}

async fn write_private_delivery<W: tokio::io::AsyncWrite + Unpin>(
    delivery: &PrivateDelivery,
    recipient: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    write: &mut W,
) -> Result<(), PeerError> {
    private_phase(&delivery.action, "delivery_authorization_enter");
    match authorize_delivery(delivery, recipient, session).await {
        Ok(()) => {
            let frame = if delivery.certified {
                let source_certificate = serde_json::to_vec(&delivery.source.cert)
                    .map_err(|_| PeerError::Protocol("certificate encoding failed".into()))?;
                match &delivery.signed {
                    // Only a session that negotiated signed actions can decode
                    // them; an older target gets the unsigned frame it knows.
                    Some(signed) if session.signed_actions => {
                        EnvelopeV2::CertifiedSignedDelivered {
                            action: SignedDirectedAction::clone(signed),
                            source_certificate,
                        }
                    }
                    _ => EnvelopeV2::CertifiedDelivered {
                        action: delivery.action.clone(),
                        source_certificate,
                    },
                }
            } else {
                EnvelopeV2::Delivered {
                    action: delivery.action.clone(),
                    source_user: delivery.source.user_public_key,
                }
            };
            let Ok(bytes) = tilo_delivery_protocol::transport_v2::encode(&frame) else {
                private_phase(&delivery.action, "delivery_encoding_rejected");
                let _ = delivery.source_disconnect.try_send(());
                return Ok(());
            };
            private_phase(&delivery.action, "write_authorized");
            write.write_all(&bytes).await?;
            private_phase(&delivery.action, "write_complete");
        }
        Err(reason) => {
            let unavailable =
                delivery
                    .reply
                    .try_send(PrivateOutbound::Reply(EnvelopeV2::unavailable(
                        &delivery.action,
                        reason,
                    )));
            if unavailable.is_err() {
                private_phase(&delivery.action, "delivery_unavailable_queue_failed");
                // A bounded failure queue cannot silently erase the outcome.
                // Its originating session must observe transport loss instead.
                let _ = delivery.source_disconnect.try_send(());
            } else {
                private_phase(&delivery.action, "delivery_unavailable_queued");
            }
        }
    }
    Ok(())
}

async fn authorize_delivery(
    delivery: &PrivateDelivery,
    recipient: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<(), UnavailableReason> {
    use UnavailableReason as U;
    let mut trace = DeliveryAuthorizationTrace::new(&delivery.action);
    if !current_action_peer(&delivery.source, session) {
        return trace.reject("delivery_rejected_source_cert_initial", U::NotAuthorised);
    }
    if !current_action_peer(recipient, session) {
        return trace.reject("delivery_rejected_recipient_cert_initial", U::NotAuthorised);
    }
    trace.phase("delivery_registry_wait");
    let (workspace_id, ws) =
        match private_workspace_binding(delivery.action.room, recipient, session).await {
            Ok(Some(binding)) => binding,
            Ok(None) => {
                return trace.reject("delivery_rejected_workspace_missing", U::NotAuthorised);
            }
            Err(_) => {
                return trace.reject("delivery_rejected_workspace_lookup", U::NotAuthorised);
            }
        };
    trace.select_workspace(workspace_id);
    trace.phase("delivery_registry_acquired");
    let Ok(wr) = crate::workspace_io::fresh_membership_for_delivery(
        &ws,
        workspace_id,
        delivery.action.request_id,
    )
    .await
    else {
        return trace.reject("delivery_rejected_membership_error", U::NotAuthorised);
    };
    if !current_action_peer(&delivery.source, session) {
        return trace.reject("delivery_rejected_source_cert_current", U::NotAuthorised);
    }
    if !current_action_peer(recipient, session) {
        return trace.reject("delivery_rejected_recipient_cert_current", U::NotAuthorised);
    }
    if !session.admits(&wr, &delivery.source, RoomOperation::RemoteAction) {
        return trace.reject("delivery_rejected_source_member", U::NotAuthorised);
    }
    if !session.admits(&wr, recipient, RoomOperation::RemoteAction) {
        return trace.reject("delivery_rejected_recipient_member", U::NotAuthorised);
    }
    if delivery.action.target != recipient.device_public_key {
        return trace.reject("delivery_rejected_target_mismatch", U::NotAuthorised);
    }
    if !current_session(&wr, &delivery.source, &delivery.source_tx) {
        return trace.reject("delivery_rejected_source_session", U::NotAuthorised);
    }
    if !wr
        .action_targets
        .get(&delivery.source.device_public_key)
        .is_some_and(|target| {
            target.room == delivery.action.room && target.state_tx.same_channel(&delivery.source_tx)
        })
    {
        return trace.reject("delivery_rejected_source_action_target", U::NotAuthorised);
    }
    if !current_session(&wr, recipient, session.outbound) {
        return trace.reject("delivery_rejected_recipient_session", U::NotAuthorised);
    }
    if !wr
        .action_targets
        .get(&recipient.device_public_key)
        .is_some_and(|target| {
            target.room == delivery.action.room && target.state_tx.same_channel(session.outbound)
        })
    {
        return trace.reject(
            "delivery_rejected_recipient_action_target",
            U::NotAuthorised,
        );
    }
    // Both were checked above under the same membership read.
    if !wr
        .action_targets
        .contains_key(&delivery.source.device_public_key)
    {
        return trace.reject("delivery_rejected_source_action_target", U::NotAuthorised);
    }
    drop(wr);
    trace.success();
    Ok(())
}

#[cfg(test)]
#[allow(
    clippy::significant_drop_tightening,
    reason = "relays, fixtures and guards deliberately stay alive until each test ends"
)]
#[allow(
    clippy::similar_names,
    reason = "paired test fixtures are named after the roles they mirror"
)]
#[allow(
    clippy::too_many_lines,
    reason = "each test spells out one end-to-end scenario"
)]
mod private_transport_tests {
    use super::*;
    use tilo_user_identity::{
        DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, UserIdentity,
        sign_device_cert,
    };

    pub(super) fn certified_device(
        root: &ed25519_dalek::SigningKey,
        user: [u8; 32],
        label: &str,
    ) -> (AuthenticatedPeer, ed25519_dalek::SigningKey) {
        let (key, device) = DeviceIdentity::generate(label, 1);
        let mut cert = DeviceCert {
            user_public_key: user,
            device_public_key: device.public_key,
            device_label: label.into(),
            capabilities: DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        sign_device_cert(&mut cert, root).unwrap();
        // Exercise certificate signature and nonce proof, matching the relay
        // handshake rather than constructing unchecked AuthenticatedPeer data.
        let hello = crate::auth::Hello {
            cert,
            client_nonce: [2; 32],
        };
        let peer = crate::auth::accept_hello(
            &hello,
            &user,
            &crate::policy::TrustPolicy::AllowMatchingUser,
            &RevocationList::new(),
            unix_now_ns(),
        )
        .unwrap();
        let proof = crate::auth::build_hello_ack2(&key, &[1; 32]);
        crate::auth::verify_hello_ack2(&proof, &peer.device_public_key, &[1; 32]).unwrap();
        (peer, key)
    }

    #[test]
    fn checkpoint_prefix_recovery_requires_enabled_current_exact_governor_session() {
        let (root, user) = UserIdentity::generate("owner", 1);
        let (governor, _) = certified_device(&root, user.public_key, "native");
        let (other, _) = certified_device(&root, user.public_key, "browser");
        let room = uuid::Uuid::from_u128(0x51).to_string();
        let id = crate::room_workspace_id(&room).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut runtime = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        runtime
            .store
            .bind_shell_authority(
                tilo_peer_state::retained_proof::RetentionAuthority::new(
                    &room,
                    governor.cert.clone(),
                )
                .unwrap(),
            )
            .unwrap();
        runtime.admit_device(governor.device_public_key).unwrap();
        runtime.admit_device(other.device_public_key).unwrap();
        let (governor_tx, _governor_rx) = mpsc::channel(2);
        let (governor_disconnect, _governor_disconnected) = mpsc::channel(1);
        runtime.add_subscriber(Subscriber {
            device_public_key: governor.device_public_key,
            role: governor.role,
            tx: governor_tx.clone(),
            disconnect: governor_disconnect.clone(),
        });
        let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
        let source = StateSource {
            peer: governor.clone(),
            tx: governor_tx,
            disconnect: governor_disconnect,
            revocations: revocations.clone(),
            policy: Arc::new(std::sync::RwLock::new(
                crate::policy::TrustPolicy::AllowMatchingUser,
            )),
            own_user: user.public_key,
            agents: AgentRegistry::default(),
            federation: None,
            session_live: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
        };
        assert!(
            !checkpoint_prefix_recovery_allowed(
                &runtime,
                &governor,
                governor.device_public_key,
                &source,
            ),
            "exact governor stays serial unless its local host opts in"
        );
        runtime.checkpoint_prefix_recovery =
            crate::config::CheckpointPrefixRecovery::NativeOwnerDurableBaseline;
        assert!(checkpoint_prefix_recovery_allowed(
            &runtime,
            &governor,
            governor.device_public_key,
            &source,
        ));

        let (other_tx, _other_rx) = mpsc::channel(2);
        let (other_disconnect, _other_disconnected) = mpsc::channel(1);
        runtime.add_subscriber(Subscriber {
            device_public_key: other.device_public_key,
            role: other.role,
            tx: other_tx.clone(),
            disconnect: other_disconnect.clone(),
        });
        let other_source = StateSource {
            peer: other.clone(),
            tx: other_tx,
            disconnect: other_disconnect,
            revocations,
            policy: source.policy.clone(),
            own_user: user.public_key,
            agents: AgentRegistry::default(),
            federation: None,
            session_live: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
        };
        assert!(!checkpoint_prefix_recovery_allowed(
            &runtime,
            &other,
            other.device_public_key,
            &other_source,
        ));
        let mut relay_peer = governor.clone();
        relay_peer.role = crate::auth::PeerRole::PeerRelay;
        assert!(!checkpoint_prefix_recovery_allowed(
            &runtime,
            &relay_peer,
            governor.device_public_key,
            &source,
        ));

        let (replacement, _replacement_rx) = mpsc::channel(2);
        runtime.add_subscriber(Subscriber {
            device_public_key: governor.device_public_key,
            role: governor.role,
            tx: replacement,
            disconnect: source.disconnect.clone(),
        });
        assert!(!checkpoint_prefix_recovery_allowed(
            &runtime,
            &governor,
            governor.device_public_key,
            &source,
        ));
    }

    #[tokio::test]
    async fn exact_history_checkpoint_prefix_production_path_runs_one_selection() {
        use crate::store::{
            bounded::{Step, WorkLimit},
            shell_selection::tests::{producer_frame, producer_metadata},
            shell_selection::{self, Selection},
        };
        let meta = producer_metadata();
        let authority = tilo_peer_state::retained_proof::RetentionAuthority::new(
            meta["room"].as_str().unwrap(),
            serde_json::from_value(meta["authority"].clone()).unwrap(),
        )
        .unwrap();
        let id = crate::room_workspace_id(authority.room()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut runtime = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        runtime.store.bind_shell_authority(authority).unwrap();
        runtime.checkpoint_prefix_recovery =
            crate::config::CheckpointPrefixRecovery::NativeOwnerDurableBaseline;
        for row in meta["baseline"].as_array().unwrap() {
            let raw = producer_frame(&meta, row, 1);
            let Envelope::Delta { delta, .. } = crate::transport::decode(&raw[4..]).unwrap() else {
                panic!()
            };
            let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
                crate::coedit::CoeditPayload::decode(&delta).unwrap()
            else {
                panic!()
            };
            runtime.store.retain_checkpoint(&channel, &raw).unwrap();
        }
        let scratch = runtime.store.root().join("store-work-v1").join(format!(
            "retain-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        let mut initial = Selection::new(&runtime.store, scratch, None).unwrap();
        while matches!(initial.step(WorkLimit::default()).unwrap(), Step::More) {}
        let store_root = runtime.store.root().to_path_buf();
        let selected = store_root.join("shell-selected-v1/CURRENT");
        let selected_before = std::fs::read(&selected).unwrap();

        let cert: tilo_user_identity::DeviceCert =
            serde_json::from_value(meta["authority"].clone()).unwrap();
        let peer = AuthenticatedPeer {
            user_public_key: cert.user_public_key,
            device_public_key: cert.device_public_key,
            cert,
            role: crate::auth::PeerRole::Device,
        };
        runtime.admit_device(peer.device_public_key).unwrap();
        let (tx, _rx) = mpsc::channel(32);
        let (disconnect, _disconnected) = mpsc::channel(1);
        runtime.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            role: peer.role,
            tx: tx.clone(),
            disconnect: disconnect.clone(),
        });
        let workspace = Arc::new(AsyncMutex::new(runtime));
        let source = StateSource {
            peer,
            tx,
            disconnect,
            revocations: Arc::new(std::sync::RwLock::new(
                tilo_user_identity::RevocationList::new(),
            )),
            policy: Arc::new(std::sync::RwLock::new(
                crate::policy::TrustPolicy::AllowMatchingUser,
            )),
            own_user: serde_json::from_value::<tilo_user_identity::DeviceCert>(
                meta["authority"].clone(),
            )
            .unwrap()
            .user_public_key,
            agents: AgentRegistry::default(),
            federation: None,
            session_live: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
        };
        let envelopes = meta["baseline"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let raw = producer_frame(&meta, row, 2);
                crate::transport::decode(&raw[4..]).unwrap()
            })
            .collect::<Vec<_>>();
        let _selection_count = shell_selection::track_selection_constructions(&store_root);
        enqueue_checkpoint_prefix(
            &workspace,
            envelopes,
            source.clone(),
            crate::auth::PeerRole::Device,
            Arc::new(crate::metrics::Metrics::default()),
            vec![None, None, None, None],
        )
        .await
        .unwrap();
        let owner = crate::persistence::handle(&workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        assert_eq!(shell_selection::selection_constructions(&store_root), 1);
        assert_ne!(std::fs::read(&selected).unwrap(), selected_before);

        let selected_before_advance = std::fs::read(&selected).unwrap();
        let advancing = meta["checkpoint2"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let raw = producer_frame(&meta, row, 3);
                crate::transport::decode(&raw[4..]).unwrap()
            })
            .collect::<Vec<_>>();
        let _selection_count = shell_selection::track_selection_constructions(&store_root);
        enqueue_checkpoint_prefix(
            &workspace,
            advancing,
            source,
            crate::auth::PeerRole::Device,
            Arc::new(crate::metrics::Metrics::default()),
            vec![None, None, None, None],
        )
        .await
        .unwrap();
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        assert_eq!(shell_selection::selection_constructions(&store_root), 1);
        assert_ne!(std::fs::read(selected).unwrap(), selected_before_advance);
    }

    // In an unoptimized build, polling this scenario through the persistence
    // owner nests deeper than the default 2 MiB test-thread stack (it needs
    // more than 4 MiB when it runs alone). Run it on its own thread with an
    // explicit stack instead of depending on RUST_MIN_STACK.
    #[test]
    fn route_prefix_beyond_the_frame_cap_uses_one_all_original_selection_then_converges() {
        std::thread::Builder::new()
            .name("route-prefix-beyond-cap".into())
            .stack_size(16 * 1024 * 1024)
            .spawn(|| {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
                    .block_on(Box::pin(route_prefix_beyond_cap_scenario()));
            })
            .unwrap()
            .join()
            .unwrap();
    }

    /// A checkpoint prefix of `PREFIX_ROUTES` record frames, followed by the
    /// two records that did not fit. Production caps a prefix at
    /// `RETENTION_PREFIX_FRAME_CAP`; selection treats every prefix size
    /// alike, so a small cap exercises the same cut at a fraction of the
    /// signing and retention work.
    async fn route_prefix_beyond_cap_scenario() {
        const PREFIX_ROUTES: usize = 4;
        use crate::store::{
            bounded::{Step, WorkLimit},
            shell_selection::tests::replay_frames,
            shell_selection::{self, SelectedRead, Selection},
        };
        use automerge::transaction::Transactable;

        fn frame(
            authority: &tilo_peer_state::retained_proof::RetentionAuthority,
            message: &tilo_peer_state::NeighbourMsg,
        ) -> Vec<u8> {
            let channel = tilo_peer_state::transport::retention(message, true)
                .unwrap()
                .0;
            let body = postcard::to_allocvec(message).unwrap();
            let source = match message {
                tilo_peer_state::NeighbourMsg::AppDocument { envelope, .. } => {
                    *envelope.author.as_bytes()
                }
                _ => authority.governor().device_public_key,
            };
            crate::transport::encode(&Envelope::Delta {
                workspace_id: crate::room_workspace_id(authority.room()).unwrap(),
                author_device_public_key: source,
                authored_at_ns: 1,
                signature: [0; 64],
                delta: crate::coedit::CoeditPayload::Checkpoint { channel, body }
                    .encode()
                    .unwrap(),
            })
            .unwrap()
        }

        let root_key = ed25519_dalek::SigningKey::from_bytes(&[51; 32]);
        let key = ed25519_dalek::SigningKey::from_bytes(&[52; 32]);
        let mut cert = tilo_user_identity::DeviceCert {
            user_public_key: root_key.verifying_key().to_bytes(),
            device_public_key: key.verifying_key().to_bytes(),
            device_label: "route prefix beyond cap".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root_key).unwrap();
        let (observer, _observer_key) = certified_device(
            &root_key,
            root_key.verifying_key().to_bytes(),
            "route prefix observer",
        );
        let (fenced_observer, _fenced_observer_key) = certified_device(
            &root_key,
            root_key.verifying_key().to_bytes(),
            "route prefix fenced observer",
        );
        let now = web_time::SystemTime::UNIX_EPOCH
            .checked_add(web_time::Duration::from_secs(4000))
            .unwrap();
        let room = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
        let mut publisher = tilo_peer_state::StatePeer::new(
            room,
            cert.clone(),
            vec![],
            tilo_user_identity::RevocationList::new(),
            now,
        )
        .unwrap();
        let documents = (0..PREFIX_ROUTES + 2)
            .map(|index| {
                let mut document = automerge::AutoCommit::new();
                document
                    .put(automerge::ROOT, "index", u64::try_from(index).unwrap())
                    .unwrap();
                tilo_peer_state::SeedDocument {
                    app_id: "threshold.app".into(),
                    record_id: format!("record-{index:02}"),
                    kind: "test".into(),
                    bytes: document.save(),
                }
            })
            .collect();
        publisher
            .install_seed(
                tilo_peer_state::SeedState {
                    room_changes: tilo_peer_state::seed_room(
                        room,
                        &cert,
                        &[
                            cert.clone(),
                            observer.cert.clone(),
                            fenced_observer.cert.clone(),
                        ],
                    )
                    .unwrap(),
                    documents,
                },
                &key,
                now,
            )
            .unwrap();
        let authority =
            tilo_peer_state::retained_proof::RetentionAuthority::new(room, cert.clone()).unwrap();
        let baseline = publisher
            .durable_bootstrap_messages(&key, now, 1)
            .unwrap()
            .iter()
            .map(|message| frame(&authority, message))
            .collect::<Vec<_>>();
        let next = publisher
            .durable_bootstrap_messages(&key, now, 2)
            .unwrap()
            .iter()
            .map(|message| frame(&authority, message))
            .collect::<Vec<_>>();
        let third = publisher
            .durable_bootstrap_messages(&key, now, 3)
            .unwrap()
            .iter()
            .map(|message| frame(&authority, message))
            .collect::<Vec<_>>();
        assert_eq!(baseline.len(), PREFIX_ROUTES + 3);
        assert_eq!(next.len(), PREFIX_ROUTES + 3);
        assert_eq!(third.len(), PREFIX_ROUTES + 3);

        let id = crate::room_workspace_id(room).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut runtime = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        runtime.store.bind_shell_authority(authority).unwrap();
        runtime.checkpoint_prefix_recovery =
            crate::config::CheckpointPrefixRecovery::NativeOwnerDurableBaseline;
        for raw in &baseline {
            let Envelope::Delta { delta, .. } = crate::transport::decode(&raw[4..]).unwrap() else {
                panic!()
            };
            let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
                crate::coedit::CoeditPayload::decode(&delta).unwrap()
            else {
                panic!()
            };
            runtime.store.retain_checkpoint(&channel, raw).unwrap();
        }
        let scratch = runtime.store.root().join("store-work-v1").join(format!(
            "retain-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        let mut initial = Selection::new(&runtime.store, scratch, None).unwrap();
        while matches!(initial.step(WorkLimit::default()).unwrap(), Step::More) {}
        let store_root = runtime.store.root().to_path_buf();

        let peer = AuthenticatedPeer {
            user_public_key: cert.user_public_key,
            device_public_key: cert.device_public_key,
            cert,
            role: crate::auth::PeerRole::Device,
        };
        runtime.admit_device(peer.device_public_key).unwrap();
        let (tx, _source_rx) = mpsc::channel(512);
        let (disconnect, mut source_disconnected) = mpsc::channel(1);
        runtime.add_subscriber(Subscriber {
            device_public_key: peer.device_public_key,
            role: peer.role,
            tx: tx.clone(),
            disconnect: disconnect.clone(),
        });
        runtime.admit_device(observer.device_public_key).unwrap();
        let (observer_tx, _observer_rx) = mpsc::channel(512);
        let (observer_state_tx, mut observer_state_rx) = mpsc::channel(512);
        let (observer_disconnect, mut observer_disconnected) = mpsc::channel(1);
        runtime.add_subscriber(Subscriber {
            device_public_key: observer.device_public_key,
            role: observer.role,
            tx: observer_tx.clone(),
            disconnect: observer_disconnect,
        });
        runtime.state_sessions.insert(
            observer.device_public_key,
            StateSession {
                peer: observer.clone(),
                tx: observer_state_tx,
                live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                replay: None,
                lag: None,
            },
        );
        runtime
            .admit_device(fenced_observer.device_public_key)
            .unwrap();
        let (fenced_observer_tx, _fenced_observer_rx) = mpsc::channel(512);
        let (fenced_observer_state_tx, _fenced_observer_state_rx) = mpsc::channel(512);
        let (fenced_observer_disconnect, _fenced_observer_disconnected) = mpsc::channel(1);
        runtime.add_subscriber(Subscriber {
            device_public_key: fenced_observer.device_public_key,
            role: fenced_observer.role,
            tx: fenced_observer_tx,
            disconnect: fenced_observer_disconnect,
        });
        runtime.state_sessions.insert(
            fenced_observer.device_public_key,
            StateSession {
                peer: fenced_observer.clone(),
                tx: fenced_observer_state_tx,
                live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                replay: None,
                lag: None,
            },
        );
        let workspace = Arc::new(AsyncMutex::new(runtime));
        let source = StateSource {
            peer,
            tx,
            disconnect,
            revocations: Arc::new(std::sync::RwLock::new(
                tilo_user_identity::RevocationList::new(),
            )),
            policy: Arc::new(std::sync::RwLock::new(
                crate::policy::TrustPolicy::AllowMatchingUser,
            )),
            own_user: root_key.verifying_key().to_bytes(),
            agents: AgentRegistry::default(),
            federation: None,
            session_live: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
        };
        let owner = crate::persistence::handle(&workspace).await;

        enqueue_checkpoint_prefix(
            &workspace,
            vec![crate::transport::decode(&next[0][4..]).unwrap()],
            source.clone(),
            crate::auth::PeerRole::Device,
            Arc::new(crate::metrics::Metrics::default()),
            vec![None],
        )
        .await
        .unwrap();
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        let room_delivery =
            tilo_test_env::within("a message on observer_state_rx", observer_state_rx.recv())
                .await
                .unwrap();
        assert!(
            crate::transport::encode(&room_delivery.envelope).unwrap() == next[0],
            "room Original was not published before its record checkpoint prefix"
        );
        let selection_count = shell_selection::track_selection_constructions(&store_root);
        let batch_modes = track_checkpoint_batch_modes(id);

        // Bounded, payload-free diagnostic: one count and total microseconds per
        // fixed selection phase, observable without taking the owner state lock.
        let progress = Arc::new(std::sync::Mutex::new(CheckpointProgress::default()));
        CHECKPOINT_PROGRESS
            .get_or_init(Default::default)
            .lock()
            .unwrap()
            .insert(id, progress.clone());

        // This prefix is the verified FinalBasisOmission case: one projected
        // Selection has switched CURRENT and the first exact Original is
        // already published before the injected error. Replace one captured
        // session while paused to prove that the immutable fence only removes
        // exact old senders.
        let fault =
            crate::store::test_barrier::fail(&store_root, "checkpoint-prefix-after-first-original");
        let (release, reached) = crate::store::test_barrier::install(
            &store_root,
            "checkpoint-prefix-after-first-original",
        );
        enqueue_checkpoint_prefix(
            &workspace,
            next.iter()
                .skip(1)
                .take(PREFIX_ROUTES)
                .map(|raw| crate::transport::decode(&raw[4..]).unwrap())
                .collect(),
            source.clone(),
            crate::auth::PeerRole::Device,
            Arc::new(crate::metrics::Metrics::default()),
            vec![None; PREFIX_ROUTES],
        )
        .await
        .unwrap();
        // A liveness guard, not a latency budget: a busy machine must not
        // fail this on timing.
        tokio::time::timeout(tilo_test_env::hang_bound(), reached)
            .await
            .unwrap_or_else(|_| {
                let source_disconnect = match source_disconnected.try_recv() {
                    Ok(()) => "signalled",
                    Err(mpsc::error::TryRecvError::Empty) => "open",
                    Err(mpsc::error::TryRecvError::Disconnected) => "closed",
                };
                let modes = batch_modes.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
                panic!(
                    "all-original prefix never reached the post-first-original barrier: selection_count={} batch_modes={modes:?} source_disconnect={source_disconnect} progress={:?}",
                    selection_count.load(std::sync::atomic::Ordering::SeqCst),
                    progress.lock().unwrap_or_else(std::sync::PoisonError::into_inner),
                );
            })
            .unwrap();
        let delivered_before_failure = tilo_test_env::within(
            "first exact Original was not visible before the injected failure",
            observer_state_rx.recv(),
        )
        .await
        .unwrap();
        assert!(
            crate::transport::encode(&delivered_before_failure.envelope).unwrap() == next[1],
            "injected failure occurred before the first exact Original publication"
        );
        let (replacement_tx, _replacement_rx) = mpsc::channel(512);
        let (replacement_state_tx, _replacement_state_rx) = mpsc::channel(512);
        let (replacement_disconnect, mut replacement_disconnected) = mpsc::channel(1);
        {
            let mut runtime = workspace.lock().await;
            runtime.add_subscriber(Subscriber {
                device_public_key: fenced_observer.device_public_key,
                role: fenced_observer.role,
                tx: replacement_tx.clone(),
                disconnect: replacement_disconnect,
            });
            runtime.state_sessions.insert(
                fenced_observer.device_public_key,
                StateSession {
                    peer: fenced_observer.clone(),
                    tx: replacement_state_tx,
                    live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                    replay: None,
                    lag: None,
                },
            );
        }
        drop(release);
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        tilo_test_env::within(
            "post-CURRENT failure did not fence the captured observer",
            observer_disconnected.recv(),
        )
        .await
        .expect("captured observer disconnect channel closed before fencing");
        assert!(
            replacement_disconnected.try_recv().is_err(),
            "immutable-session fence disconnected the replacement session"
        );
        {
            let runtime = workspace.lock().await;
            assert!(
                !runtime
                    .state_sessions
                    .contains_key(&observer.device_public_key),
                "captured observer state session survived the post-CURRENT failure"
            );
            assert!(
                runtime.subscribers[&fenced_observer.device_public_key]
                    .tx
                    .same_channel(&replacement_tx),
                "post-CURRENT fence removed a newer replacement session"
            );
        }
        drop(fault);

        // A reconnect receives the same durable prefix without a selected
        // basis repair. Its successful suffix must converge normally.
        let (reopened_tx, _reopened_rx) = mpsc::channel(512);
        let (reopened_state_tx, mut reopened_state_rx) = mpsc::channel(512);
        let (reopened_disconnect, mut reopened_disconnected) = mpsc::channel(1);
        {
            let mut runtime = workspace.lock().await;
            runtime.add_subscriber(Subscriber {
                device_public_key: observer.device_public_key,
                role: observer.role,
                tx: reopened_tx.clone(),
                disconnect: reopened_disconnect,
            });
            runtime.state_sessions.insert(
                observer.device_public_key,
                StateSession {
                    peer: observer.clone(),
                    tx: reopened_state_tx,
                    live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                    replay: None,
                    lag: None,
                },
            );
        }
        enqueue_checkpoint_prefix(
            &workspace,
            next.iter()
                .skip(1)
                .take(PREFIX_ROUTES)
                .map(|raw| crate::transport::decode(&raw[4..]).unwrap())
                .collect(),
            source.clone(),
            crate::auth::PeerRole::Device,
            Arc::new(crate::metrics::Metrics::default()),
            vec![None; PREFIX_ROUTES],
        )
        .await
        .unwrap();
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        assert_eq!(
            selection_count.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "failed prefix and reconnect did not each use one global Selection"
        );
        assert_eq!(
            *batch_modes.lock().unwrap(),
            [
                CheckpointBatchMode::AllOriginal,
                CheckpointBatchMode::AllOriginal,
            ],
            "the failed prefix or reconnect did not take the guarded all-Original path"
        );
        for (index, expected) in next.iter().skip(1).take(PREFIX_ROUTES).enumerate() {
            let observed =
                tilo_test_env::within("a message on reopened_state_rx", reopened_state_rx.recv())
                    .await
                    .unwrap();
            assert!(
                crate::transport::encode(&observed.envelope).unwrap() == *expected,
                "all-Original checkpoint publication changed wire order at input {index}"
            );
        }
        assert!(
            replacement_disconnected.try_recv().is_err()
                && reopened_disconnected.try_recv().is_err(),
            "successful reconnect recovery disconnected a current session"
        );
        let retained_after_prefix = {
            let runtime = workspace.lock().await;
            replay_frames(&runtime.store)
                .into_iter()
                .map(|frame| crate::transport::encode(&frame).unwrap())
                .collect::<Vec<_>>()
        };
        for raw in baseline.iter().chain(next.iter().take(PREFIX_ROUTES + 1)) {
            assert!(
                retained_after_prefix.contains(raw),
                "selected CURRENT omitted old basis or an exact Original prefix frame"
            );
        }

        enqueue_checkpoint_prefix(
            &workspace,
            next.iter()
                .skip(PREFIX_ROUTES + 1)
                .map(|raw| crate::transport::decode(&raw[4..]).unwrap())
                .collect(),
            source.clone(),
            crate::auth::PeerRole::Device,
            Arc::new(crate::metrics::Metrics::default()),
            vec![None; 2],
        )
        .await
        .unwrap();
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        assert_eq!(
            *batch_modes.lock().unwrap(),
            [
                CheckpointBatchMode::AllOriginal,
                CheckpointBatchMode::AllOriginal,
                CheckpointBatchMode::Represented
            ],
            "the remaining checkpoints did not complete the represented selected basis"
        );
        let after_reconnect = {
            let runtime = workspace.lock().await;
            replay_frames(&runtime.store)
                .into_iter()
                .map(|frame| crate::transport::encode(&frame).unwrap())
                .collect::<Vec<_>>()
        };
        for raw in &next {
            assert!(
                after_reconnect.contains(raw),
                "reconnect suffix did not converge to the complete next checkpoint generation"
            );
        }
        let chunks = [
            (third.iter().take(1).collect::<Vec<_>>(), vec![None]),
            (
                third.iter().skip(1).take(PREFIX_ROUTES).collect::<Vec<_>>(),
                vec![None; PREFIX_ROUTES],
            ),
            (
                third.iter().skip(PREFIX_ROUTES + 1).collect::<Vec<_>>(),
                vec![None; 2],
            ),
        ];
        for (chunk, budgets) in chunks {
            enqueue_checkpoint_prefix(
                &workspace,
                chunk
                    .into_iter()
                    .map(|raw| crate::transport::decode(&raw[4..]).unwrap())
                    .collect(),
                source.clone(),
                crate::auth::PeerRole::Device,
                Arc::new(crate::metrics::Metrics::default()),
                budgets,
            )
            .await
            .unwrap();
            crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
                .await
                .unwrap();
        }
        assert!(
            replacement_disconnected.try_recv().is_err()
                && reopened_disconnected.try_recv().is_err(),
            "later automatic recovery disconnected a current session"
        );
        let runtime = workspace.lock().await;
        let final_frames = replay_frames(&runtime.store)
            .into_iter()
            .map(|frame| crate::transport::encode(&frame).unwrap())
            .collect::<Vec<_>>();
        for raw in &third {
            assert!(
                final_frames.contains(raw),
                "reconnect recovery did not retain the next complete checkpoint generation"
            );
        }
        let mut selected = SelectedRead::new(&runtime.store).unwrap().unwrap();
        while matches!(selected.step().unwrap(), Step::More) {}
    }

    #[tokio::test]
    async fn checkpoint_stage_allocation_error_registers_candidate_for_owner_cleanup() {
        use crate::store::shell_selection::tests::{producer_frame, producer_metadata};
        let meta = producer_metadata();
        let authority = tilo_peer_state::retained_proof::RetentionAuthority::new(
            meta["room"].as_str().unwrap(),
            serde_json::from_value(meta["authority"].clone()).unwrap(),
        )
        .unwrap();
        let id = crate::room_workspace_id(authority.room()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut runtime = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        runtime.store.bind_shell_authority(authority).unwrap();
        for row in meta["baseline"].as_array().unwrap() {
            let raw = producer_frame(&meta, row, 1);
            let Envelope::Delta { delta, .. } = crate::transport::decode(&raw[4..]).unwrap() else {
                panic!()
            };
            let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
                crate::coedit::CoeditPayload::decode(&delta).unwrap()
            else {
                panic!()
            };
            runtime.store.retain_checkpoint(&channel, &raw).unwrap();
        }
        let raw = producer_frame(&meta, &meta["baseline"][0], 2);
        let envelope = crate::transport::decode(&raw[4..]).unwrap();
        let Envelope::Delta {
            author_device_public_key,
            delta,
            ..
        } = &envelope
        else {
            panic!()
        };
        let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
            crate::coedit::CoeditPayload::decode(delta).unwrap()
        else {
            panic!()
        };
        let lineage = runtime.store.root().join("retained/v2").join(format!(
            "{}-{}",
            hex::encode(author_device_public_key),
            hex::encode(channel)
        ));
        let current_before = std::fs::read(lineage.join("CURRENT")).unwrap();
        let entries_before = std::fs::read_dir(&lineage).unwrap().count();
        let fault = crate::store::test_barrier::fail(&lineage, "checkpoint-stage-after-allocation");
        let workspace = Arc::new(AsyncMutex::new(runtime));
        let owner = crate::persistence::handle(&workspace).await;
        let result = owner
            .submit_sequence_wait(1, raw.len(), move |state, _| async move {
                let mut state = state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let mut batch = new_checkpoint_batch(&mut state, vec![envelope], None)?;
                loop {
                    match step_checkpoint_batch(&mut state, &mut batch)? {
                        Step::More => {}
                        Step::Done(()) => return Ok(()),
                    }
                }
            })
            .await
            .unwrap();
        assert!(crate::persistence::receipt(result).await.is_err());
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        drop(fault);
        assert_eq!(
            std::fs::read(lineage.join("CURRENT")).unwrap(),
            current_before
        );
        assert_eq!(
            std::fs::read_dir(&lineage).unwrap().count(),
            entries_before,
            "owner cleanup left an inactive checkpoint candidate"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn reader_buffers_authenticated_checkpoint_suffix_into_one_owner_batch() {
        use crate::store::{
            bounded::{Step, WorkLimit},
            shell_selection::tests::{producer_frame, producer_metadata},
            shell_selection::{self, Selection},
        };
        let meta = producer_metadata();
        let authority = tilo_peer_state::retained_proof::RetentionAuthority::new(
            meta["room"].as_str().unwrap(),
            serde_json::from_value(meta["authority"].clone()).unwrap(),
        )
        .unwrap();
        let id = crate::room_workspace_id(authority.room()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut runtime = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        runtime.store.bind_shell_authority(authority).unwrap();
        runtime.checkpoint_prefix_recovery =
            crate::config::CheckpointPrefixRecovery::NativeOwnerDurableBaseline;
        for row in meta["baseline"].as_array().unwrap() {
            let raw = producer_frame(&meta, row, 1);
            let Envelope::Delta { delta, .. } = crate::transport::decode(&raw[4..]).unwrap() else {
                panic!()
            };
            let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
                crate::coedit::CoeditPayload::decode(&delta).unwrap()
            else {
                panic!()
            };
            runtime.store.retain_checkpoint(&channel, &raw).unwrap();
        }
        let scratch = runtime.store.root().join("store-work-v1").join(format!(
            "retain-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        let mut initial = Selection::new(&runtime.store, scratch, None).unwrap();
        while matches!(initial.step(WorkLimit::default()).unwrap(), Step::More) {}
        let store_root = runtime.store.root().to_path_buf();
        let cert: tilo_user_identity::DeviceCert =
            serde_json::from_value(meta["authority"].clone()).unwrap();
        let peer = AuthenticatedPeer {
            user_public_key: cert.user_public_key,
            device_public_key: cert.device_public_key,
            cert,
            role: crate::auth::PeerRole::Device,
        };
        runtime.admit_device(peer.device_public_key).unwrap();
        // This test occupies every normal-message permit below; give the
        // workspace its own admission budgets so that saturation cannot
        // starve concurrently running tests that share the process-wide ones.
        crate::persistence::isolate_test_admission_with_workers(
            &runtime,
            crate::persistence::isolated_test_workers(),
        );
        let workspace = Arc::new(AsyncMutex::new(runtime));
        let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, workspace.clone())])));
        let metrics = Arc::new(crate::metrics::Metrics::default());
        let (client, server) = tokio::io::duplex(1024 * 1024);
        let (read, write) = tokio::io::split(server);
        let task = tokio::spawn(run_peer_stream(
            peer.clone(),
            read,
            write,
            PeerContext {
                workspaces: registry,
                autobind: None,
                revocations: Arc::new(std::sync::RwLock::new(RevocationList::new())),
                trust_policy: Arc::new(std::sync::RwLock::new(
                    crate::policy::TrustPolicy::AllowMatchingUser,
                )),
                own_user_public_key: peer.user_public_key,
                own_device_cert: peer.cert.clone(),
                own_device_public_key: peer.cert.device_public_key,
                app_serving: None,
                metrics,
                live_bridge: None,
                agents: crate::peer::AgentRegistry::default(),
                positions: None,
                compactor: None,
            },
            SessionProtocol::V2,
            std::future::pending::<()>(),
        ));
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut client_reader = SessionReader::new(client_read, true);
        client_write
            .write_all(
                &tilo_delivery_protocol::transport_v2::encode_state(
                    &Envelope::Subscribe { workspace_id: id },
                    true,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        loop {
            if matches!(
                client_reader.read_next().await.unwrap(),
                Some(EnvelopeV2::State(Envelope::Ready { .. }))
            ) {
                break;
            }
        }
        let drain = tokio::spawn(async move {
            while client_reader
                .read_next()
                .await
                .is_ok_and(|frame| frame.is_some())
            {}
        });

        // Occupy all normal-message permits in one already-admitted owner job.
        // The reader can still acquire and retain delivery budgets for later
        // frames while the first Delta waits for normal admission.
        let owner = crate::persistence::handle(&workspace).await;
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let held = owner
            .submit_sequence_wait(96, 1, move |_, _| async move {
                let _ = entered_tx.send(());
                let _ = release_rx.await;
                Ok(())
            })
            .await
            .unwrap();
        entered_rx.await.unwrap();
        let observed = track_reader_backlog(id);
        let selection_count = shell_selection::track_selection_constructions(&store_root);
        let blocker_raw = producer_frame(&meta, &meta["baseline"][0], 1);
        let blocker = crate::transport::decode(&blocker_raw[4..]).unwrap();
        let envelopes = meta["checkpoint2"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let raw = producer_frame(&meta, row, 2);
                crate::transport::decode(&raw[4..]).unwrap()
            })
            .collect::<Vec<_>>();
        client_write
            .write_all(&tilo_delivery_protocol::transport_v2::encode_state(&blocker, true).unwrap())
            .await
            .unwrap();
        for envelope in &envelopes {
            client_write
                .write_all(
                    &tilo_delivery_protocol::transport_v2::encode_state(envelope, true).unwrap(),
                )
                .await
                .unwrap();
        }
        while observed.load(std::sync::atomic::Ordering::SeqCst) < envelopes.len() {
            assert!(
                !task.is_finished(),
                "reader ended before retaining the authenticated suffix: {:?}",
                task.await
            );
            tokio::task::yield_now().await;
        }
        // A clean FIN after complete authenticated frames must not discard
        // those frames while the preceding admission is backpressured.
        client_write.shutdown().await.unwrap();
        release_tx.send(()).unwrap();
        crate::persistence::receipt(held).await.unwrap();
        while selection_count.load(std::sync::atomic::Ordering::SeqCst) < 1 {
            assert!(
                !task.is_finished(),
                "reader suffix failed after {} selections: {:?}",
                selection_count.load(std::sync::atomic::Ordering::SeqCst),
                task.await
            );
            tokio::task::yield_now().await;
        }
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        let tail = envelopes.last().unwrap();
        tilo_test_env::within("the buffered checkpoint tail to become durable", async {
            loop {
                let retained = workspace.lock().await.store.load_retained().unwrap();
                if retained
                    .iter()
                    .any(|raw| crate::transport::decode(&raw[4..]).unwrap() == *tail)
                {
                    break;
                }
                assert!(
                    !task.is_finished(),
                    "peer ended before persisting the buffered tail"
                );
                tokio::task::yield_now().await;
            }
        })
        .await;
        stop_tracking_reader_backlog(id);
        assert_eq!(shell_selection::selection_constructions(&store_root), 1);
        tilo_test_env::within("peer to finish after clean FIN", task)
            .await
            .unwrap()
            .unwrap();
        drain.abort();
        let _ = drain.await;
    }

    async fn checkpoint_prefix_activation_failure_fences_original_sessions_and_retry(
        operation: &'static str,
        force_serial_finish_gc: bool,
    ) {
        use crate::store::{
            bounded::{Step, WorkLimit},
            shell_selection::Selection,
            shell_selection::tests::{producer_frame, producer_metadata},
        };
        let meta = producer_metadata();
        let authority = tilo_peer_state::retained_proof::RetentionAuthority::new(
            meta["room"].as_str().unwrap(),
            serde_json::from_value(meta["authority"].clone()).unwrap(),
        )
        .unwrap();
        let id = crate::room_workspace_id(authority.room()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut runtime = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        runtime.store.bind_shell_authority(authority).unwrap();
        runtime.checkpoint_prefix_recovery =
            crate::config::CheckpointPrefixRecovery::NativeOwnerDurableBaseline;
        for row in meta["baseline"].as_array().unwrap() {
            let raw = producer_frame(&meta, row, 1);
            let Envelope::Delta { delta, .. } = crate::transport::decode(&raw[4..]).unwrap() else {
                panic!()
            };
            let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
                crate::coedit::CoeditPayload::decode(&delta).unwrap()
            else {
                panic!()
            };
            runtime.store.retain_checkpoint(&channel, &raw).unwrap();
        }
        let scratch = runtime.store.root().join("store-work-v1").join(format!(
            "retain-{}",
            hex::encode(rand::random::<[u8; 16]>())
        ));
        let mut initial = Selection::new(&runtime.store, scratch, None).unwrap();
        while matches!(initial.step(WorkLimit::default()).unwrap(), Step::More) {}
        let store = runtime.store.clone();

        let cert: tilo_user_identity::DeviceCert =
            serde_json::from_value(meta["authority"].clone()).unwrap();
        let source_peer = AuthenticatedPeer {
            user_public_key: cert.user_public_key,
            device_public_key: cert.device_public_key,
            cert,
            role: crate::auth::PeerRole::Device,
        };
        runtime.admit_device(source_peer.device_public_key).unwrap();
        let (source_tx, _source_rx) = mpsc::channel(32);
        let (source_disconnect, _source_disconnected) = mpsc::channel(1);
        runtime.add_subscriber(Subscriber {
            device_public_key: source_peer.device_public_key,
            role: source_peer.role,
            tx: source_tx.clone(),
            disconnect: source_disconnect.clone(),
        });

        let (root, user) = UserIdentity::generate("checkpoint prefix recipients", 1);
        let mut peers = Vec::new();
        let mut disconnects = Vec::new();
        for label in ["original", "replaced"] {
            let (peer, _) = certified_device(&root, user.public_key, label);
            runtime.admit_device(peer.device_public_key).unwrap();
            let (tx, _rx) = mpsc::channel(32);
            let (disconnect, disconnected) = mpsc::channel(1);
            let (state_tx, _state_rx) = mpsc::channel(32);
            runtime.add_subscriber(Subscriber {
                device_public_key: peer.device_public_key,
                role: peer.role,
                tx,
                disconnect,
            });
            runtime.state_sessions.insert(
                peer.device_public_key,
                StateSession {
                    peer: peer.clone(),
                    tx: state_tx,
                    live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                    replay: None,
                    lag: None,
                },
            );
            peers.push(peer);
            disconnects.push(disconnected);
        }
        let workspace = Arc::new(AsyncMutex::new(runtime));
        let source = StateSource {
            peer: source_peer,
            tx: source_tx,
            disconnect: source_disconnect,
            revocations: Arc::new(std::sync::RwLock::new(RevocationList::new())),
            policy: Arc::new(std::sync::RwLock::new(
                crate::policy::TrustPolicy::AllowMatchingUser,
            )),
            own_user: serde_json::from_value::<tilo_user_identity::DeviceCert>(
                meta["authority"].clone(),
            )
            .unwrap()
            .user_public_key,
            agents: AgentRegistry::default(),
            federation: None,
            session_live: Some(Arc::new(std::sync::atomic::AtomicBool::new(true))),
        };
        let real_envelopes = meta["checkpoint2"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let raw = producer_frame(&meta, row, 2);
                crate::transport::decode(&raw[4..]).unwrap()
            })
            .collect::<Vec<_>>();
        let mut envelopes = real_envelopes.clone();
        let fault_root = if force_serial_finish_gc {
            let Envelope::Delta {
                workspace_id,
                author_device_public_key,
                authored_at_ns,
                signature,
                delta,
            } = envelopes[0].clone()
            else {
                unreachable!()
            };
            let crate::coedit::CoeditPayload::Checkpoint { body, .. } =
                crate::coedit::CoeditPayload::decode(&delta).unwrap()
            else {
                unreachable!()
            };
            envelopes.push(Envelope::Delta {
                workspace_id,
                author_device_public_key,
                authored_at_ns,
                signature,
                delta: crate::coedit::CoeditPayload::Checkpoint {
                    channel: [0xee; 32],
                    body,
                }
                .encode()
                .unwrap(),
            });
            let Envelope::Delta {
                author_device_public_key,
                delta,
                ..
            } = real_envelopes.last().unwrap()
            else {
                unreachable!()
            };
            let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
                crate::coedit::CoeditPayload::decode(delta).unwrap()
            else {
                unreachable!()
            };
            store.root().join("retained/v2").join(format!(
                "{}-{}",
                hex::encode(author_device_public_key),
                hex::encode(channel)
            ))
        } else {
            store.root().to_path_buf()
        };
        let fault = crate::store::test_barrier::fail(&fault_root, operation);
        let (release, reached) = crate::store::test_barrier::install(&fault_root, operation);
        enqueue_checkpoint_prefix(
            &workspace,
            envelopes.clone(),
            source.clone(),
            crate::auth::PeerRole::Device,
            Arc::new(crate::metrics::Metrics::default()),
            vec![None; envelopes.len()],
        )
        .await
        .unwrap();
        // Event-driven: a prefix that never reaches the fault hangs here and
        // nextest's slow-timeout reports it; load cannot fail a wall-clock budget.
        reached
            .await
            .expect("checkpoint prefix never reached its post-CURRENT fault");

        let (new_tx, _new_rx) = mpsc::channel(32);
        let (new_disconnect, mut new_disconnected) = mpsc::channel(1);
        let (new_state_tx, _new_frames) = mpsc::channel(32);
        {
            let peer = &peers[1];
            let mut runtime = workspace.lock().await;
            runtime.add_subscriber(Subscriber {
                device_public_key: peer.device_public_key,
                role: peer.role,
                tx: new_tx.clone(),
                disconnect: new_disconnect,
            });
            runtime.state_sessions.insert(
                peer.device_public_key,
                StateSession {
                    peer: peer.clone(),
                    tx: new_state_tx,
                    live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                    replay: None,
                    lag: None,
                },
            );
        }
        drop(release);
        let owner = crate::persistence::handle(&workspace).await;
        // The owner runs jobs in order: this receipt completes once the
        // checkpoint prefix before it has drained.
        let done = owner
            .submit_resumable_wait(
                1,
                async { Ok(()) },
                |_, _| Ok(Step::Done(())),
                |result| async { result },
            )
            .await
            .unwrap();
        crate::persistence::receipt(done)
            .await
            .expect("checkpoint prefix owner did not drain");
        if force_serial_finish_gc {
            let promoted =
                String::from_utf8(std::fs::read(fault_root.join("CURRENT")).unwrap()).unwrap();
            assert!(
                fault_root.join(promoted).is_dir(),
                "cleanup removed the promoted lineage generation"
            );
            let selected = store.root().join("shell-selected-v1");
            let current_name =
                String::from_utf8(std::fs::read(selected.join("CURRENT")).unwrap()).unwrap();
            let current = selected.join(current_name);
            let entries = std::fs::read_dir(&selected)
                .unwrap()
                .map(|entry| entry.unwrap().path())
                .collect::<Vec<_>>();
            assert_eq!(
                entries.len(),
                2,
                "abandoned projected or serial selected generation was not reclaimed: {entries:?}"
            );
            assert!(entries.contains(&selected.join("CURRENT")));
            assert!(entries.contains(&current));
        }
        drop(fault);
        enqueue_checkpoint_prefix(
            &workspace,
            real_envelopes.clone(),
            source,
            crate::auth::PeerRole::Device,
            Arc::new(crate::metrics::Metrics::default()),
            vec![None; real_envelopes.len()],
        )
        .await
        .unwrap();
        // The owner runs jobs in order: this receipt completes once the
        // checkpoint prefix before it has drained.
        let done = owner
            .submit_resumable_wait(
                1,
                async { Ok(()) },
                |_, _| Ok(Step::Done(())),
                |result| async { result },
            )
            .await
            .unwrap();
        crate::persistence::receipt(done)
            .await
            .expect("checkpoint prefix owner did not drain");

        assert!(
            disconnects[0].try_recv().is_ok(),
            "post-CURRENT {operation} failure must fence the captured prefix recipient"
        );
        {
            let runtime = workspace.lock().await;
            assert!(
                !runtime
                    .state_sessions
                    .contains_key(&peers[0].device_public_key)
            );
            assert!(
                runtime.subscribers[&peers[1].device_public_key]
                    .tx
                    .same_channel(&new_tx)
            );
        }
        assert!(
            new_disconnected.try_recv().is_err(),
            "replacement session must survive the old prefix failure"
        );
    }

    #[tokio::test]
    async fn checkpoint_prefix_post_current_failure_fences_only_original_sessions() {
        Box::pin(
            checkpoint_prefix_activation_failure_fences_original_sessions_and_retry(
                "shell-after-current",
                false,
            ),
        )
        .await;
    }

    #[tokio::test]
    async fn checkpoint_prefix_cursor_open_failure_fences_only_original_sessions() {
        Box::pin(
            checkpoint_prefix_activation_failure_fences_original_sessions_and_retry(
                "shell-publication-open",
                false,
            ),
        )
        .await;
    }

    #[tokio::test]
    async fn checkpoint_prefix_serial_gc_failure_fences_only_original_sessions() {
        Box::pin(
            checkpoint_prefix_activation_failure_fences_original_sessions_and_retry(
                "checkpoint-stage-finish-gc",
                true,
            ),
        )
        .await;
    }

    async fn activation_failure_fences_original_sessions_and_retry(operation: &'static str) {
        use crate::store::shell_selection::tests::{
            admit_frames, producer_fixture, producer_frame, producer_metadata, replay_frames,
        };
        let meta = producer_metadata();
        let authority = tilo_peer_state::retained_proof::RetentionAuthority::new(
            meta["room"].as_str().unwrap(),
            serde_json::from_value(meta["authority"].clone()).unwrap(),
        )
        .unwrap();
        let id = crate::room_workspace_id(authority.room()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut runtime = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        runtime.store.bind_shell_authority(authority).unwrap();
        // Both captured sessions are already ready on this durable old basis.
        for row in meta["baseline"].as_array().unwrap() {
            let raw = producer_frame(&meta, row, 1);
            let Envelope::Delta { delta, .. } = crate::transport::decode(&raw[4..]).unwrap() else {
                panic!()
            };
            let crate::coedit::CoeditPayload::Checkpoint { channel, .. } =
                crate::coedit::CoeditPayload::decode(&delta).unwrap()
            else {
                panic!()
            };
            runtime.store.retain_checkpoint(&channel, &raw).unwrap();
        }
        let store = runtime.store.clone();
        let (root, user) = UserIdentity::generate("activation fault recipients", 1);
        let mut peers = Vec::new();
        #[expect(
            clippy::collection_is_never_read,
            reason = "holds the receivers open so their senders stay connected"
        )]
        let mut ordinary_receivers = Vec::new();
        let mut disconnects = Vec::new();
        let mut frames = Vec::new();
        for label in ["original", "replaced"] {
            let (peer, _) = certified_device(&root, user.public_key, label);
            runtime.admit_device(peer.device_public_key).unwrap();
            let (tx, rx) = mpsc::channel(32);
            let (disconnect, disconnected) = mpsc::channel(1);
            let (state_tx, state_rx) = mpsc::channel(32);
            runtime.add_subscriber(Subscriber {
                device_public_key: peer.device_public_key,
                role: peer.role,
                tx,
                disconnect,
            });
            runtime.state_sessions.insert(
                peer.device_public_key,
                StateSession {
                    peer: peer.clone(),
                    tx: state_tx,
                    live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                    replay: None,
                    lag: None,
                },
            );
            peers.push(peer);
            ordinary_receivers.push(rx);
            disconnects.push(disconnected);
            frames.push(state_rx);
        }
        let workspace = Arc::new(AsyncMutex::new(runtime));
        let metrics = Arc::new(crate::metrics::Metrics::default());
        let room_half = producer_frame(&meta, &meta["tail"][0], 2);
        enqueue_state(
            &workspace,
            crate::transport::decode(&room_half[4..]).unwrap(),
            None,
            crate::auth::PeerRole::Device,
            metrics.clone(),
        )
        .await
        .unwrap();
        let owner = crate::persistence::handle(&workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        let current = store.root().join("shell-selected-v1/CURRENT");
        let before = std::fs::read(&current).unwrap();
        let raw = producer_frame(&meta, &meta["tail"][1], 3);
        let fault = crate::store::test_barrier::fail(store.root(), operation);
        let (release, reached) = crate::store::test_barrier::install(store.root(), operation);
        enqueue_state(
            &workspace,
            crate::transport::decode(&raw[4..]).unwrap(),
            None,
            crate::auth::PeerRole::Device,
            metrics.clone(),
        )
        .await
        .unwrap();
        tilo_test_env::within(
            "selected activation never reached its fault barrier",
            reached,
        )
        .await
        .unwrap();
        assert_ne!(
            std::fs::read(&current).unwrap(),
            before,
            "injection must follow CURRENT switch"
        );
        // A newer session replaces one captured channel while the original job
        // is paused. Error fencing must never disconnect this replacement.
        let (new_tx, _new_rx) = mpsc::channel(32);
        let (new_disconnect, mut new_disconnected) = mpsc::channel(1);
        let (new_state_tx, mut new_frames) = mpsc::channel(32);
        {
            let peer = &peers[1];
            let mut runtime = workspace.lock().await;
            runtime.add_subscriber(Subscriber {
                device_public_key: peer.device_public_key,
                role: peer.role,
                tx: new_tx.clone(),
                disconnect: new_disconnect,
            });
            runtime.state_sessions.insert(
                peer.device_public_key,
                StateSession {
                    peer: peer.clone(),
                    tx: new_state_tx,
                    live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                    replay: None,
                    lag: None,
                },
            );
        }
        drop(release);
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        drop(fault);
        // Retry identical bytes after the durable cut: suppression must not
        // leave an old ready recipient connected forever without activation.
        enqueue_state(
            &workspace,
            crate::transport::decode(&raw[4..]).unwrap(),
            None,
            crate::auth::PeerRole::Device,
            metrics,
        )
        .await
        .unwrap();
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        assert!(
            disconnects[0].try_recv().is_ok(),
            "post-cut {operation} error must fence captured recipient before exact retry suppresses it"
        );
        assert!(frames[0].try_recv().is_err());
        {
            let runtime = workspace.lock().await;
            assert!(
                !runtime
                    .state_sessions
                    .contains_key(&peers[0].device_public_key)
            );
            assert!(
                runtime.subscribers[&peers[1].device_public_key]
                    .tx
                    .same_channel(&new_tx)
            );
        }
        assert!(
            new_disconnected.try_recv().is_err(),
            "replacement channel must survive old job failure"
        );
        assert!(
            new_frames.try_recv().is_err(),
            "exact retry remains semantically suppressed"
        );
        // Reconnect captures the real durable basis after resumed GC, admitting
        // every frame and converging to the producer's exact app heads.
        let peer = admit_frames(
            &meta,
            &producer_fixture().join("baseline-peer.postcard"),
            replay_frames(&store),
        );
        for row in meta["rows"].as_array().unwrap() {
            assert_eq!(
                peer.document_heads(
                    row["app_id"].as_str().unwrap(),
                    row["record_id"].as_str().unwrap()
                )
                .unwrap()
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
                serde_json::from_value::<Vec<String>>(row["heads"].clone()).unwrap()
            );
        }
    }

    #[tokio::test]
    async fn selected_activation_post_current_failure_fences_original_sessions_on_exact_retry() {
        activation_failure_fences_original_sessions_and_retry("shell-after-current").await;
    }
    #[tokio::test]
    async fn selected_activation_cursor_open_failure_fences_original_sessions_on_exact_retry() {
        activation_failure_fences_original_sessions_and_retry("shell-publication-open").await;
    }

    #[tokio::test]
    async fn selected_shell_activation_publishes_complete_ordered_pair_and_suppresses_delayed_checkpoint()
     {
        use crate::store::shell_selection::tests::{
            admit_frames, producer_fixture, producer_frame, producer_metadata,
        };
        let meta = producer_metadata();
        let authority = tilo_peer_state::retained_proof::RetentionAuthority::new(
            meta["room"].as_str().unwrap(),
            serde_json::from_value(meta["authority"].clone()).unwrap(),
        )
        .unwrap();
        let id = crate::room_workspace_id(authority.room()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut runtime = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        runtime.store.bind_shell_authority(authority).unwrap();
        let (root, user) = UserIdentity::generate("activation recipient", 1);
        let (receiver, _) = certified_device(&root, user.public_key, "recipient");
        runtime.admit_device(receiver.device_public_key).unwrap();
        let (tx, _rx) = mpsc::channel(32);
        let (disconnect, _) = mpsc::channel(1);
        let (state_tx, mut state_rx) = mpsc::channel(32);
        runtime.add_subscriber(Subscriber {
            device_public_key: receiver.device_public_key,
            role: receiver.role,
            tx,
            disconnect,
        });
        runtime.state_sessions.insert(
            receiver.device_public_key,
            StateSession {
                peer: receiver,
                tx: state_tx,
                live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                replay: None,
                lag: None,
            },
        );
        let (slow, _) = certified_device(&root, user.public_key, "saturated recipient");
        runtime.admit_device(slow.device_public_key).unwrap();
        let (slow_tx, _slow_rx) = mpsc::channel(1);
        let (slow_disconnect, mut slow_disconnected) = mpsc::channel(1);
        let (slow_state, mut slow_frames) = mpsc::channel(1);
        runtime.add_subscriber(Subscriber {
            device_public_key: slow.device_public_key,
            role: slow.role,
            tx: slow_tx,
            disconnect: slow_disconnect,
        });
        runtime.state_sessions.insert(
            slow.device_public_key,
            StateSession {
                peer: slow,
                tx: slow_state,
                live: Arc::new(std::sync::atomic::AtomicBool::new(true)),
                replay: None,
                lag: None,
            },
        );
        let workspace = Arc::new(AsyncMutex::new(runtime));
        let metrics = Arc::new(crate::metrics::Metrics::default());
        let mut delivered_all = Vec::new();
        for (group, start, end, expected) in [
            ("baseline", 0, 4, 4),
            ("tail", 0, 2, 2),
            ("tail", 2, 3, 1),
            ("checkpoint2", 0, 4, 4),
        ] {
            for row in &meta[group].as_array().unwrap()[start..end] {
                let raw = producer_frame(&meta, row, 10);
                enqueue_state(
                    &workspace,
                    crate::transport::decode(&raw[4..]).unwrap(),
                    None,
                    crate::auth::PeerRole::Device,
                    metrics.clone(),
                )
                .await
                .unwrap();
            }
            let owner = crate::persistence::handle(&workspace).await;
            crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
                .await
                .unwrap();
            let mut frames = Vec::new();
            while let Ok(delivery) = state_rx.try_recv() {
                frames.push((*delivery.envelope).clone());
            }
            assert_eq!(
                frames.len(),
                expected,
                "activation preserves exact selective live output for {group}[{start}..{end}]"
            );
            delivered_all.extend(frames);
            admit_frames(
                &meta,
                &producer_fixture().join("baseline-peer.postcard"),
                delivered_all.clone(),
            );
        }
        assert!(
            slow_disconnected.try_recv().is_ok(),
            "bounded activation queue saturation fences partial recipient"
        );
        assert!(slow_frames.try_recv().is_ok());
        assert!(slow_frames.try_recv().is_err());
        for row in meta["baseline"].as_array().unwrap() {
            let raw = producer_frame(&meta, row, u64::MAX - 1);
            enqueue_state(
                &workspace,
                crate::transport::decode(&raw[4..]).unwrap(),
                None,
                crate::auth::PeerRole::Device,
                metrics.clone(),
            )
            .await
            .unwrap();
        }
        let owner = crate::persistence::handle(&workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        assert!(
            state_rx.try_recv().is_err(),
            "delayed CP1 must not invalidate a ready CP2 receiver"
        );
    }

    #[tokio::test]
    async fn shared_retained_metrics_count_only_newly_written_payload_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        let runtime = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            7,
            false,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        let workspace = Arc::new(AsyncMutex::new(runtime));
        let metrics = Arc::new(crate::metrics::Metrics::default());
        let envelope = Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: [1; 32],
            authored_at_ns: 1,
            signature: [0; 64],
            delta: crate::coedit::CoeditPayload::Checkpoint {
                channel: [2; 32],
                body: vec![3; 64],
            }
            .encode()
            .unwrap(),
        };
        let expected = crate::transport::encode(&envelope).unwrap().len() as u64;
        for _ in 0..2 {
            enqueue_state(
                &workspace,
                envelope.clone(),
                None,
                crate::auth::PeerRole::Device,
                metrics.clone(),
            )
            .await
            .unwrap();
        }
        let owner = crate::persistence::handle(&workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        assert_eq!(
            metrics.snapshot().bytes_queued,
            expected,
            "checkpoint retry was counted as another payload write"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn paused_state_persistence_releases_routing_lock() {
        paused_state_private_roundtrip(true, false, false, false, None).await;
    }

    /// A peer that pipelines more private requests than the reply queue holds,
    /// while its own replies are not yet read, is backpressured: the relay
    /// stops reading until the writer drains, and never disconnects it.
    #[tokio::test(start_paused = true)]
    async fn pipelined_private_requests_backpressure_instead_of_disconnecting() {
        const REQUESTS: usize = PEER_OUTBOUND_CAPACITY * 4;
        let (root, user) = UserIdentity::generate("alice", 1);
        let (a, _) = certified_device(&root, user.public_key, "a");
        let room = uuid::Uuid::from_u128(7);
        let id = crate::room_workspace_id(&room.to_string()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut runtime = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        runtime.admit_device(a.device_public_key).unwrap();
        let registry = Arc::new(AsyncMutex::new(HashMap::from([(
            id,
            Arc::new(AsyncMutex::new(runtime)),
        )])));
        let ctx = PeerContext {
            workspaces: registry,
            autobind: None,
            revocations: Arc::new(std::sync::RwLock::new(RevocationList::new())),
            trust_policy: Arc::new(std::sync::RwLock::new(
                crate::policy::TrustPolicy::AllowMatchingUser,
            )),
            own_user_public_key: user.public_key,
            own_device_cert: a.cert.clone(),
            own_device_public_key: a.cert.device_public_key,
            app_serving: None,
            metrics: Arc::new(crate::metrics::Metrics::default()),
            live_bridge: None,
            agents: crate::peer::AgentRegistry::default(),
            positions: None,
            compactor: None,
        };
        // Requests flow through a roomy pipe; replies through a small one, so
        // the relay's writer blocks until the client reads.
        let (mut requests, server_read) = tokio::io::duplex(1 << 20);
        let (server_write, replies) = tokio::io::duplex(256);
        let (server_read, _) = tokio::io::split(server_read);
        let (_, server_write) = tokio::io::split(server_write);
        let (replies, _) = tokio::io::split(replies);
        let relay = tokio::spawn(run_peer_stream(
            a.clone(),
            server_read,
            server_write,
            ctx,
            SessionProtocol::V2,
            std::future::pending::<()>(),
        ));
        let mut reader = SessionReader::new(replies, true);
        requests
            .write_all(
                &tilo_delivery_protocol::transport_v2::encode_state(
                    &Envelope::Subscribe { workspace_id: id },
                    true,
                )
                .unwrap(),
            )
            .await
            .unwrap();
        assert!(matches!(
            reader.read_next().await.unwrap(),
            Some(EnvelopeV2::State(Envelope::Ready { .. }))
        ));
        let enable =
            tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::EnableActions { room })
                .unwrap();
        for _ in 0..REQUESTS {
            requests.write_all(&enable).await.unwrap();
        }
        // The clock is paused: this returns only once the relay has nothing
        // left to do, its writer blocked on the unread replies.
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        assert!(
            !relay.is_finished(),
            "the relay dropped a backpressured peer"
        );
        for _ in 0..REQUESTS {
            assert!(matches!(
                reader.read_next().await.unwrap(),
                Some(EnvelopeV2::ActionsReady { .. })
            ));
        }
        assert!(!relay.is_finished());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn paused_state_persistence_allows_same_connection_private_roundtrip() {
        paused_state_private_roundtrip(false, false, false, false, None).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn paused_replay_cleanup_with_full_reply_queue_keeps_private_reader_and_writer_live() {
        paused_state_private_roundtrip(false, true, false, false, None).await;
    }

    #[tokio::test]
    async fn full_control_backlog_backpressures_and_recovers_after_held_writer() {
        paused_state_private_roundtrip(false, true, true, false, None).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn overtaking_private_keeps_held_state_authorization_future_pollable() {
        paused_state_private_roundtrip(false, false, false, true, None).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn plain_subscription_recovers_retained_frame_after_live_budget_exhaustion() {
        paused_state_private_roundtrip(false, false, false, false, Some(LagBoundary::Live)).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn plain_subscription_discards_stale_lag_after_explicit_unsubscribe() {
        paused_state_private_roundtrip(false, false, false, false, Some(LagBoundary::Unsubscribed))
            .await;
    }

    enum LagBoundary {
        Live,
        Unsubscribed,
    }

    #[expect(
        clippy::fn_params_excessive_bools,
        reason = "each flag selects one independent fault in the shared scenario"
    )]
    async fn paused_state_private_roundtrip(
        check_lock: bool,
        held_cleanup: bool,
        full_backlog: bool,
        held_authorization: bool,
        plain_lag: Option<LagBoundary>,
    ) {
        let (root, user) = UserIdentity::generate("alice", 1);
        let (a, _) = certified_device(&root, user.public_key, "a");
        let (b, _) = certified_device(&root, user.public_key, "b");
        let room = uuid::Uuid::from_u128(7);
        let id = crate::room_workspace_id(&room.to_string()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut runtime = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        for device in [a.device_public_key, b.device_public_key, [99; 32]] {
            runtime.admit_device(device).unwrap();
        }
        let store = runtime.store.clone();
        let workspace = Arc::new(AsyncMutex::new(runtime));
        let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, workspace.clone())])));
        let trust = Arc::new(std::sync::RwLock::new(
            crate::policy::TrustPolicy::AllowMatchingUser,
        ));
        let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
        let mut wires = Vec::new();
        let mut tasks = Vec::new();
        for peer in [&a, &b] {
            let (client, server) = tokio::io::duplex(65536);
            let (read, write) = tokio::io::split(server);
            let ctx = PeerContext {
                workspaces: registry.clone(),
                autobind: None,
                revocations: revocations.clone(),
                trust_policy: trust.clone(),
                own_user_public_key: user.public_key,
                own_device_cert: a.cert.clone(),
                own_device_public_key: a.cert.device_public_key,
                app_serving: None,
                metrics: Arc::new(crate::metrics::Metrics::default()),
                live_bridge: None,
                agents: crate::peer::AgentRegistry::default(),
                positions: None,
                compactor: None,
            };
            tasks.push(tokio::spawn(run_peer_stream(
                peer.clone(),
                read,
                write,
                ctx,
                SessionProtocol::V2,
                std::future::pending::<()>(),
            )));
            let (read, mut write) = tokio::io::split(client);
            let mut reader = SessionReader::new(read, true);
            write
                .write_all(
                    &tilo_delivery_protocol::transport_v2::encode_state(
                        &Envelope::Subscribe { workspace_id: id },
                        true,
                    )
                    .unwrap(),
                )
                .await
                .unwrap();
            assert!(matches!(
                reader.read_next().await.unwrap(),
                Some(EnvelopeV2::State(Envelope::Ready { .. }))
            ));
            write
                .write_all(
                    &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::EnableActions {
                        room,
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
            assert!(matches!(
                reader.read_next().await.unwrap(),
                Some(EnvelopeV2::ActionsReady { .. })
            ));
            wires.push((reader, write));
        }
        let (mut ar, mut aw) = wires.remove(0);
        let (mut br, mut bw) = wires.remove(0);
        if let Some(boundary) = plain_lag {
            let lag = workspace.lock().await.state_sessions[&b.device_public_key]
                .lag
                .as_ref()
                .unwrap()
                .clone();
            let envelope = Envelope::Delta {
                workspace_id: id,
                author_device_public_key: a.device_public_key,
                authored_at_ns: 1,
                signature: [0; 64],
                delta: crate::coedit::CoeditPayload::RetainedDelta {
                    channel: [3; 32],
                    body: vec![4; 4096],
                }
                .encode()
                .unwrap(),
            };
            let fault = crate::store::test_barrier::fail(store.root(), "live-delivery-budget");
            let (publication_release, published) =
                crate::store::test_barrier::install(store.root(), "live-publication-complete");
            let (release, reached) =
                crate::store::test_barrier::install(store.root(), "live-delivery-budget");
            aw.write_all(
                &tilo_delivery_protocol::transport_v2::encode_state(&envelope, true).unwrap(),
            )
            .await
            .unwrap();
            tilo_test_env::within("the retained publication to reach its live budget", reached)
                .await
                .expect("ordinary retained publication never reached live budget");
            drop(release);
            tilo_test_env::within("the missed live publication recipient fan-out", published)
                .await
                .expect("missed live publication never completed its recipient fan-out");
            drop(publication_release);
            let owner = crate::persistence::handle(&workspace).await;
            crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
                .await
                .unwrap();
            drop(fault);
            assert!(
                store
                    .load_retained()
                    .unwrap()
                    .iter()
                    .any(|bytes| { crate::transport::decode(&bytes[4..]).unwrap() == envelope }),
                "missed live frame must remain durably retained"
            );
            let unsubscribed = matches!(boundary, LagBoundary::Unsubscribed);
            if unsubscribed {
                for frame in [Envelope::Unsubscribe { workspace_id: id }, Envelope::Ping] {
                    bw.write_all(
                        &tilo_delivery_protocol::transport_v2::encode_state(&frame, true).unwrap(),
                    )
                    .await
                    .unwrap();
                }
                tilo_test_env::within("the explicit unsubscribe to complete", async {
                    loop {
                        match br.read_next().await.unwrap() {
                            Some(EnvelopeV2::State(Envelope::Pong)) => break,
                            Some(EnvelopeV2::State(
                                Envelope::Delta { .. } | Envelope::Ready { .. },
                            )) => {}
                            other => panic!("unexpected explicit-unsubscribe reply: {other:?}"),
                        }
                    }
                })
                .await;
                assert!(
                    !workspace
                        .lock()
                        .await
                        .state_sessions
                        .contains_key(&b.device_public_key)
                );
                // A queued hint may outlive the subscription that produced it.
                // Its ordering is fixed here after the real unsubscribe reply.
                lag.mark(id);
            }
            // Publication has completed, so the lag marker was made. Wait for
            // its actual consumption rather than guessing the polling interval.
            tilo_test_env::within("the peer to consume its lag marker", async {
                while !lag.is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await;
            bw.write_all(
                &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Ping, true).unwrap(),
            )
            .await
            .unwrap();
            let mut frames = Vec::new();
            let mut ready = 0_usize;
            tilo_test_env::within("retained catch-up before the next peer reply", async {
                loop {
                    match br.read_next().await.unwrap() {
                        Some(EnvelopeV2::State(Envelope::Pong)) => break,
                        Some(EnvelopeV2::State(Envelope::Ready { .. })) => {
                            ready = ready.checked_add(1).unwrap();
                        }
                        Some(EnvelopeV2::State(frame @ Envelope::Delta { .. })) => {
                            frames.push(frame);
                        }
                        other => panic!("unexpected lag-recovery frame: {other:?}"),
                    }
                }
            })
            .await;
            if !unsubscribed {
                assert!(
                    workspace.lock().await.state_sessions[&b.device_public_key]
                        .live
                        .load(std::sync::atomic::Ordering::SeqCst),
                    "ordinary byte backpressure must preserve the authorized subscription"
                );
            }
            for task in tasks {
                task.abort();
                let _ = task.await;
            }
            if unsubscribed {
                assert!(
                    frames.is_empty(),
                    "a stale lag hint reopened an explicitly closed subscription"
                );
                assert_eq!(
                    ready, 0,
                    "an explicitly closed subscription cannot become ready again"
                );
                assert!(
                    !workspace
                        .lock()
                        .await
                        .state_sessions
                        .contains_key(&b.device_public_key)
                );
            } else {
                assert_eq!(
                    frames,
                    vec![envelope],
                    "plain subscription lost its committed retained frame"
                );
                assert_eq!(
                    ready, 1,
                    "catch-up must complete its actual replay boundary"
                );
            }
            return;
        }
        let envelope = Envelope::Delta {
            workspace_id: id,
            author_device_public_key: a.device_public_key,
            authored_at_ns: 1,
            signature: [0; 64],
            delta: crate::coedit::CoeditPayload::Checkpoint {
                channel: [3; 32],
                body: vec![4; 4096],
            }
            .encode()
            .unwrap(),
        };
        let (release, reached) = crate::store::test_barrier::install(
            store.root(),
            if held_authorization {
                "membership-read"
            } else if held_cleanup {
                "replay-cleanup"
            } else {
                "state-persist"
            },
        );
        if held_cleanup {
            let pending = Envelope::Delta {
                workspace_id: id,
                author_device_public_key: b.device_public_key,
                authored_at_ns: 3,
                delta: vec![255; 64],
                signature: [0; 64],
            };
            store
                .enqueue_pending(
                    &a.device_public_key,
                    3,
                    &crate::transport::encode(&pending).unwrap(),
                )
                .unwrap();
            aw.write_all(
                &tilo_delivery_protocol::transport_v2::encode_state(
                    &Envelope::Subscribe { workspace_id: id },
                    true,
                )
                .unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(
                ar.read_next().await.unwrap(),
                Some(EnvelopeV2::State(pending))
            );
        } else {
            aw.write_all(
                &tilo_delivery_protocol::transport_v2::encode_state(&envelope, true).unwrap(),
            )
            .await
            .unwrap();
        }
        reached.await.unwrap();
        if held_cleanup {
            for _ in 0..if full_backlog { 64 } else { 12 } {
                aw.write_all(
                    &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Ping, true)
                        .unwrap(),
                )
                .await
                .unwrap();
            }
        }
        if full_backlog {
            // The writer is confirmed inside the held cleanup. No state reply
            // can drain yet; enough real input fills reply and control queues.
            let held =
                tokio::time::timeout(std::time::Duration::from_millis(100), ar.read_next()).await;
            drop(release);
            let owner = crate::persistence::handle(&workspace).await;
            crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
                .await
                .unwrap();
            if held.is_ok() {
                let ended = tasks.remove(0).await.unwrap();
                panic!(
                    "full control backlog disconnected a healthy held writer: {held:?}; server result: {ended:?}"
                );
            }
            assert!(matches!(
                ar.read_next().await.unwrap(),
                Some(EnvelopeV2::State(Envelope::Ready { .. }))
            ));
            for _ in 0..64 {
                assert_eq!(
                    ar.read_next().await.unwrap(),
                    Some(EnvelopeV2::State(Envelope::Pong))
                );
            }
            for task in tasks {
                task.abort();
            }
            return;
        }
        assert!(
            store.load_retained().unwrap().is_empty(),
            "state published before commit"
        );
        let request = DirectedAction {
            room,
            request_id: uuid::Uuid::from_u128(8),
            source: a.device_public_key,
            target: b.device_public_key,
            message: ActionMessage::CatalogueRefresh { scope: None },
        };
        if held_authorization {
            // The state handler holds the authorization lane while its real
            // filesystem read is paused. Confirm the nested private handler
            // has entered before releasing the read: no timing-based race.
            let (private_release, private_reached) =
                crate::store::test_barrier::install(store.root(), "private-authorization");
            aw.write_all(
                &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::Directed(
                    request.clone(),
                ))
                .unwrap(),
            )
            .await
            .unwrap();
            // Both waits are events, not wall-clock budgets: a private handler
            // that cannot enter lookahead, or a state future that stops being
            // polled, hangs here and nextest's slow-timeout reports it.
            private_reached
                .await
                .expect("private handler never entered lookahead");
            drop(release);
            drop(private_release);
            let mut state_seen = false;
            let mut private_seen = false;
            while !state_seen || !private_seen {
                match br.read_next().await.unwrap() {
                    Some(EnvelopeV2::State(actual)) => {
                        assert_eq!(actual, envelope);
                        assert!(!state_seen);
                        state_seen = true;
                    }
                    Some(EnvelopeV2::Delivered { action, .. }) => {
                        assert_eq!(action, request);
                        assert!(!private_seen);
                        private_seen = true;
                    }
                    other => panic!(
                        "overtaking private handler stopped polling the state future holding its authorization lane: {other:?}"
                    ),
                }
            }
            for task in tasks {
                task.abort();
                let _ = task.await;
            }
            return;
        }
        let progress = tokio::time::timeout(tilo_test_env::hang_bound(), async {
            if check_lock {
                drop(workspace.lock().await);
            }
            let encoded_request = tilo_delivery_protocol::transport_v2::encode(
                &EnvelopeV2::Directed(request.clone()),
            )
            .unwrap();
            if check_lock {
                aw.write_all(&encoded_request).await.unwrap();
            } else {
                // A partial header remains in the stateful reader while its
                // independent writer completes an unrelated private frame.
                aw.write_all(&encoded_request[..2]).await.unwrap();
                let probe = DirectedAction {
                    request_id: uuid::Uuid::from_u128(9),
                    source: b.device_public_key,
                    target: a.device_public_key,
                    ..request.clone()
                };
                bw.write_all(
                    &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::Directed(
                        probe.clone(),
                    ))
                    .unwrap(),
                )
                .await
                .unwrap();
                let Some(EnvelopeV2::Delivered { action, .. }) = ar.read_next().await.unwrap()
                else {
                    panic!("expected private frame during partial inbound header")
                };
                assert_eq!(action, probe);
                aw.write_all(&encoded_request[2..]).await.unwrap();
            }
            let Some(EnvelopeV2::Delivered { action, .. }) = br.read_next().await.unwrap() else {
                panic!("expected private request before any uncommitted state")
            };
            assert_eq!(action, request);
            let response = DirectedAction {
                source: b.device_public_key,
                target: a.device_public_key,
                ..request.clone()
            };
            bw.write_all(
                &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::Directed(
                    response.clone(),
                ))
                .unwrap(),
            )
            .await
            .unwrap();
            let Some(EnvelopeV2::Delivered { action, .. }) = ar.read_next().await.unwrap() else {
                panic!("expected private response")
            };
            assert_eq!(action, response);
        })
        .await;
        // Always release the real blocking disk operation before asserting RED.
        drop(release);
        let owner = crate::persistence::handle(&workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        for task in tasks {
            task.abort();
        }
        assert!(
            progress.is_ok(),
            "held state persistence blocked private request/response (routing-lock check: {check_lock})"
        );
    }

    #[tokio::test]
    async fn capability_controls_fence_dependent_private_during_held_operation() {
        for control in 0..3 {
            capability_control_lookahead(false, control).await;
        }
    }

    #[tokio::test]
    async fn capability_controls_fence_dependent_private_during_reply_wait() {
        for control in 0..3 {
            capability_control_lookahead(true, control).await;
        }
    }

    #[tokio::test]
    async fn heartbeat_keeps_progress_during_unrelated_workspace_admission() {
        capability_control_lookahead(false, 3).await;
    }

    #[tokio::test]
    async fn heartbeat_does_not_overtake_queued_capability_control() {
        capability_control_lookahead(false, 4).await;
    }

    async fn capability_control_lookahead(reply_wait: bool, control: u8) {
        let (root, user) = UserIdentity::generate("owner", 1);
        let (a, _) = certified_device(&root, user.public_key, "a");
        let (b, _) = certified_device(&root, user.public_key, "b");
        let rooms = [
            uuid::Uuid::from_u128(71),
            uuid::Uuid::from_u128(72),
            uuid::Uuid::from_u128(73),
        ];
        let ids = rooms.map(|room| crate::room_workspace_id(&room.to_string()).unwrap());
        let tmp = tempfile::tempdir().unwrap();
        let mut map = HashMap::new();
        for id in ids {
            let mut runtime = crate::workspace::WorkspaceRuntime::open(
                tmp.path(),
                id,
                true,
                crate::workspace::Membership::default(),
            )
            .unwrap();
            for peer in [&a, &b] {
                runtime.admit_device(peer.device_public_key).unwrap();
            }
            map.insert(id, Arc::new(AsyncMutex::new(runtime)));
        }
        let held_workspace = map.get(&ids[2]).unwrap().clone();
        let store = held_workspace.lock().await.store.clone();
        let registry = Arc::new(AsyncMutex::new(map));
        let trust = Arc::new(std::sync::RwLock::new(
            crate::policy::TrustPolicy::AllowMatchingUser,
        ));
        let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
        let host = uuid::Uuid::from_u128(74);
        let mut tasks = Vec::new();
        let mut wires = Vec::new();
        for (index, peer) in [&a, &b].into_iter().enumerate() {
            let (client, server) = tokio::io::duplex(65536);
            let (read, write) = tokio::io::split(server);
            tasks.push(tokio::spawn(run_peer_stream(
                peer.clone(),
                read,
                write,
                PeerContext {
                    workspaces: registry.clone(),
                    autobind: None,
                    revocations: revocations.clone(),
                    trust_policy: trust.clone(),
                    own_user_public_key: user.public_key,
                    own_device_cert: a.cert.clone(),
                    own_device_public_key: a.cert.device_public_key,
                    app_serving: None,
                    metrics: Arc::new(crate::metrics::Metrics::default()),
                    live_bridge: None,
                    agents: crate::peer::AgentRegistry::default(),
                    positions: None,
                    compactor: None,
                },
                SessionProtocol::V2,
                std::future::pending::<()>(),
            )));
            let (read, mut write) = tokio::io::split(client);
            let mut reader = SessionReader::new(read, true);
            for (position, room) in rooms.iter().enumerate() {
                write
                    .write_all(
                        &tilo_delivery_protocol::transport_v2::encode_state(
                            &Envelope::Subscribe {
                                workspace_id: ids[position],
                            },
                            true,
                        )
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                assert!(matches!(
                    reader.read_next().await.unwrap(),
                    Some(EnvelopeV2::State(Envelope::Ready { .. }))
                ));
                if position < 2 && !(index == 0 && position == 0 && control == 0) {
                    write
                        .write_all(
                            &tilo_delivery_protocol::transport_v2::encode(
                                &EnvelopeV2::EnableActions { room: *room },
                            )
                            .unwrap(),
                        )
                        .await
                        .unwrap();
                    assert!(matches!(
                        reader.read_next().await.unwrap(),
                        Some(EnvelopeV2::ActionsReady { .. })
                    ));
                }
            }
            if index == 1 && control > 0 {
                write
                    .write_all(
                        &tilo_delivery_protocol::transport_v2::encode(
                            &EnvelopeV2::RegisterBroker {
                                room: rooms[0],
                                host,
                            },
                        )
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                assert!(matches!(
                    reader.read_next().await.unwrap(),
                    Some(EnvelopeV2::ActionsReady { .. })
                ));
            }
            wires.push((reader, write));
        }
        let (mut ar, mut aw) = wires.remove(0);
        let (mut br, mut bw) = wires.remove(0);
        if control >= 3 {
            let gated = control == 4;
            let (release, reached) =
                crate::store::test_barrier::install(store.root(), "membership-read");
            let envelope = Envelope::Delta {
                workspace_id: ids[2],
                author_device_public_key: a.device_public_key,
                authored_at_ns: 1,
                signature: [0; 64],
                delta: crate::coedit::CoeditPayload::RetainedDelta {
                    channel: [3; 32],
                    body: vec![4; 4096],
                }
                .encode()
                .unwrap(),
            };
            aw.write_all(
                &tilo_delivery_protocol::transport_v2::encode_state(&envelope, true).unwrap(),
            )
            .await
            .unwrap();
            tilo_test_env::within("the state admission membership read", reached)
                .await
                .unwrap();
            assert!(store.load_retained().unwrap().is_empty());
            let request = DirectedAction {
                room: rooms[1],
                request_id: uuid::Uuid::from_u128(77),
                source: a.device_public_key,
                target: b.device_public_key,
                message: ActionMessage::CatalogueRefresh { scope: None },
            };
            if gated {
                aw.write_all(
                    &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::EnableActions {
                        room: rooms[0],
                    })
                    .unwrap(),
                )
                .await
                .unwrap();
            }
            for frame in [
                EnvelopeV2::State(Envelope::Ping),
                EnvelopeV2::Directed(request.clone()),
            ] {
                aw.write_all(&tilo_delivery_protocol::transport_v2::encode(&frame).unwrap())
                    .await
                    .unwrap();
            }
            let Some(EnvelopeV2::Delivered { action, .. }) =
                tilo_test_env::within("the unrelated-room private request", br.read_next())
                    .await
                    .unwrap()
            else {
                panic!("unrelated private request stalled during state admission")
            };
            assert_eq!(action, request);
            let response = DirectedAction {
                source: b.device_public_key,
                target: a.device_public_key,
                ..request
            };
            bw.write_all(
                &tilo_delivery_protocol::transport_v2::encode(&EnvelopeV2::Directed(
                    response.clone(),
                ))
                .unwrap(),
            )
            .await
            .unwrap();
            // A real later private response proves this reader and writer are
            // live. Only an earlier control may hold the heartbeat behind it.
            let first =
                tilo_test_env::within("the heartbeat or later private reply", ar.read_next())
                    .await
                    .unwrap();
            let later = if !gated && first == Some(EnvelopeV2::State(Envelope::Pong)) {
                tilo_test_env::within("the private response after the heartbeat", ar.read_next())
                    .await
                    .unwrap()
            } else {
                None
            };
            assert!(store.load_retained().unwrap().is_empty());
            drop(release);
            let owner = crate::persistence::handle(&held_workspace).await;
            crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
                .await
                .unwrap();
            let delivered_state =
                tilo_test_env::within("the state delivery after admission release", br.read_next())
                    .await
                    .unwrap();
            assert_eq!(delivered_state, Some(EnvelopeV2::State(envelope)));
            let after_control = if gated
                && matches!(&first, Some(EnvelopeV2::Delivered { action, .. }) if action == &response)
            {
                let ready = tilo_test_env::within("the queued capability reply", ar.read_next())
                    .await
                    .unwrap();
                let pong = tilo_test_env::within("the heartbeat after its control", ar.read_next())
                    .await
                    .unwrap();
                Some((ready, pong))
            } else {
                None
            };
            for task in tasks {
                task.abort();
                let _ = task.await;
            }
            if gated {
                assert!(
                    matches!(first, Some(EnvelopeV2::Delivered { action, .. }) if action == response),
                    "a heartbeat crossed its earlier capability control"
                );
                let (ready, pong) = after_control.unwrap();
                assert!(matches!(
                    ready,
                    Some(EnvelopeV2::ActionsReady { room, .. }) if room == rooms[0]
                ));
                assert_eq!(pong, Some(EnvelopeV2::State(Envelope::Pong)));
            } else {
                assert_eq!(
                    first,
                    Some(EnvelopeV2::State(Envelope::Pong)),
                    "a live transport withheld its heartbeat behind unrelated state admission"
                );
                assert!(
                    matches!(later, Some(EnvelopeV2::Delivered { action, .. }) if action == response),
                    "heartbeat progress lost the authorized private response"
                );
            }
            return;
        }
        let (release, reached) = crate::store::test_barrier::install(
            store.root(),
            if reply_wait {
                "replay-cleanup"
            } else {
                "replay-prepare"
            },
        );
        if reply_wait {
            let pending = Envelope::Delta {
                workspace_id: ids[2],
                author_device_public_key: b.device_public_key,
                authored_at_ns: 1,
                delta: vec![255; 8],
                signature: [0; 64],
            };
            store
                .enqueue_pending(
                    &a.device_public_key,
                    1,
                    &crate::transport::encode(&pending).unwrap(),
                )
                .unwrap();
        }
        aw.write_all(
            &tilo_delivery_protocol::transport_v2::encode_state(
                &Envelope::Subscribe {
                    workspace_id: ids[2],
                },
                true,
            )
            .unwrap(),
        )
        .await
        .unwrap();
        if reply_wait {
            assert!(matches!(
                ar.read_next().await.unwrap(),
                Some(EnvelopeV2::State(Envelope::Delta { .. }))
            ));
        }
        reached.await.unwrap();
        if reply_wait {
            for _ in 0..12 {
                aw.write_all(
                    &tilo_delivery_protocol::transport_v2::encode_state(&Envelope::Ping, true)
                        .unwrap(),
                )
                .await
                .unwrap();
            }
        }
        let capability = match control {
            0 => EnvelopeV2::EnableActions { room: rooms[0] },
            1 => EnvelopeV2::EnableBrokerClient { room: rooms[0] },
            _ => EnvelopeV2::RegisterBroker {
                room: rooms[0],
                host,
            },
        };
        let dependent = DirectedAction {
            room: rooms[0],
            request_id: uuid::Uuid::from_u128(75),
            source: a.device_public_key,
            target: b.device_public_key,
            message: ActionMessage::CatalogueRefresh { scope: None },
        };
        let unrelated = DirectedAction {
            room: rooms[1],
            request_id: uuid::Uuid::from_u128(76),
            ..dependent.clone()
        };
        for frame in [
            capability,
            EnvelopeV2::Directed(dependent.clone()),
            EnvelopeV2::Directed(unrelated.clone()),
        ] {
            aw.write_all(&tilo_delivery_protocol::transport_v2::encode(&frame).unwrap())
                .await
                .unwrap();
        }
        let progress = tokio::time::timeout(tilo_test_env::hang_bound(), br.read_next()).await;
        // Receiving this later unrelated frame proves the reader classified the
        // earlier capability and dependent frame while the real owner was held.
        let premature =
            tokio::time::timeout(std::time::Duration::from_millis(50), ar.read_next()).await;
        drop(release);
        let owner = crate::persistence::handle(&held_workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        assert!(
            matches!(progress, Ok(Ok(Some(EnvelopeV2::Delivered { action, .. }))) if action == unrelated),
            "unrelated-room private traffic stalled"
        );
        assert!(
            premature.is_err(),
            "dependent private frame overtook capability {control} (reply wait={reply_wait}): {premature:?}"
        );
        let delivered = tilo_test_env::within("the next frame from br", br.read_next())
            .await
            .unwrap()
            .unwrap();
        if control > 0 {
            assert!(matches!(&delivered, EnvelopeV2::CertifiedDelivered { .. }));
        }
        match delivered {
            EnvelopeV2::Delivered { action, .. }
            | EnvelopeV2::CertifiedDelivered { action, .. } => assert_eq!(action, dependent),
            other => panic!("dependent request did not observe capability transition: {other:?}"),
        }
        for task in tasks {
            task.abort();
        }
    }

    #[tokio::test]
    async fn retained_replay_failure_and_duplicate_retirement_are_recoverable() {
        let (root, user) = UserIdentity::generate("alice", 1);
        let (peer, _) = certified_device(&root, user.public_key, "reader");
        let tmp = tempfile::tempdir().unwrap();
        let mut runtime = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            7,
            false,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        runtime.admit_device(peer.device_public_key).unwrap();
        let envelope = |author, payload: crate::coedit::CoeditPayload| Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: [author; 32],
            authored_at_ns: 1,
            delta: payload.encode().unwrap(),
            signature: [0; 64],
        };
        let cp = envelope(
            1,
            crate::coedit::CoeditPayload::Checkpoint {
                channel: [9; 32],
                body: vec![1],
            },
        );
        let tail = envelope(
            1,
            crate::coedit::CoeditPayload::RetainedDelta {
                channel: [9; 32],
                body: vec![2],
            },
        );
        let other = envelope(
            2,
            crate::coedit::CoeditPayload::Checkpoint {
                channel: [9; 32],
                body: vec![3],
            },
        );
        let cp_bytes = crate::transport::encode(&cp).unwrap();
        runtime
            .store
            .retain_checkpoint(&[9; 32], &cp_bytes)
            .unwrap();
        runtime
            .store
            .retain_delta(&[9; 32], 2, &crate::transport::encode(&tail).unwrap())
            .unwrap();
        runtime
            .store
            .enqueue_pending(&peer.device_public_key, 1, &cp_bytes)
            .unwrap();
        runtime
            .store
            .enqueue_pending(
                &peer.device_public_key,
                2,
                &crate::transport::encode(&other).unwrap(),
            )
            .unwrap();
        runtime
            .store
            .enqueue_pending(&peer.device_public_key, 4, &cp_bytes)
            .unwrap();
        let opaque = b"unknown pending bytes";
        runtime
            .store
            .enqueue_pending(&peer.device_public_key, 3, opaque)
            .unwrap();
        // Matching undecodable retained bytes do NOT constitute replay coverage.
        let lineage = runtime.store.root().join("retained/v2").join(format!(
            "{}-{}",
            hex::encode([1; 32]),
            hex::encode([9; 32])
        ));
        let active = std::fs::read_to_string(lineage.join("CURRENT")).unwrap();
        std::fs::write(lineage.join(active.trim()).join("opaque.delta"), opaque).unwrap();
        let shared = Arc::new(AsyncMutex::new(runtime));
        let registry = Arc::new(AsyncMutex::new(HashMap::from([(7, shared.clone())])));
        let trust = Arc::new(std::sync::RwLock::new(
            crate::policy::TrustPolicy::AllowMatchingUser,
        ));
        let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
        let metrics = Arc::new(crate::metrics::Metrics::default());
        let (outbound, _rx) = mpsc::channel(8);
        let (disconnect, _) = mpsc::channel(1);
        let (private, _) = mpsc::channel(8);
        let session = SessionCtx {
            workspaces: &registry,
            autobind: None,
            outbound: &outbound,
            state_outbound: None,
            connection_live: None,
            own_user_public_key: &user.public_key,
            own_device_cert: &peer.cert,
            own_device_public_key: &peer.cert.device_public_key,
            app_serving: None,
            metrics: &metrics,
            trust_policy: &trust,
            disconnect: &disconnect,
            framing: tilo_delivery_protocol::transport_v2::StateFraming::V1,
            presence: None,
            signed_actions: false,
            private_tx: &private,
            revocations: &revocations,
            live_bridge: None,
            agents: Box::leak(Box::default()),
            positions: None,
            compactor: None,
            resumes: Box::leak(Box::default()),
            lag: Box::leak(Box::default()),
        };
        let (mut broken, reader) = tokio::io::duplex(1);
        drop(reader);
        assert!(
            Box::pin(handle_inbound_frame(
                Envelope::Subscribe { workspace_id: 7 },
                &peer,
                &session,
                &mut Vec::new(),
                &mut broken
            ))
            .await
            .is_err()
        );
        assert_eq!(
            shared
                .lock()
                .await
                .store
                .count_pending(&peer.device_public_key)
                .unwrap(),
            4
        );
        // Finish the old owner's cancelled-ticket disposal before emulating a
        // new process. Two owners over the same store are not a daemon restart.
        tilo_test_env::within("old owner did not release cancelled replay", async {
            loop {
                if std::fs::read_dir(shared.lock().await.store.root().join("store-work-v1"))
                    .unwrap()
                    .next()
                    .is_none()
                {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await;
        // Emulate a crash during resumable duplicate cleanup: write the complete
        // replay, remove only its first duplicate, then reopen before the rest
        // of the cleanup list can be processed.
        let store = shared.lock().await.store.clone();
        let mut capture = store
            .start_replay_capture(
                crate::store::replay::ReplayId(*uuid::Uuid::new_v4().as_bytes()),
                peer.device_public_key,
            )
            .unwrap();
        let mut snapshot = loop {
            if let Step::Done(snapshot) = store
                .capture_replay_step(&mut capture, WorkLimit::default())
                .unwrap()
            {
                break snapshot;
            }
        };
        while matches!(
            store
                .prepare_replay_step(&mut snapshot, WorkLimit::default())
                .unwrap(),
            Step::More
        ) {}
        let budget = crate::store::replay::ReplayBudget::new(1024 * 1024);
        loop {
            let frames = store
                .read_replay_batch(
                    &mut snapshot,
                    crate::store::replay::ReplayLimits::default(),
                    &budget,
                )
                .unwrap();
            if frames.eof {
                break;
            }
        }
        store
            .cleanup_replay_step(
                &mut snapshot,
                crate::store::replay::WorkLimit {
                    entries: 1,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(
            shared
                .lock()
                .await
                .store
                .count_pending(&peer.device_public_key)
                .unwrap(),
            3
        );
        drop(snapshot);
        drop(capture);
        // Reopen the actual disk state after the failed replay.
        *shared.lock().await = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            7,
            false,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        let mut wire = Vec::new();
        Box::pin(handle_inbound_frame(
            Envelope::Subscribe { workspace_id: 7 },
            &peer,
            &session,
            &mut Vec::new(),
            &mut wire,
        ))
        .await
        .unwrap();
        let mut decoded = Vec::new();
        let mut remaining = wire.as_slice();
        while !remaining.is_empty() {
            let length = u32::from_le_bytes(remaining[..4].try_into().unwrap()) as usize;
            decoded.push(crate::transport::decode(&remaining[4..4 + length]).unwrap());
            remaining = &remaining[4 + length..];
        }
        assert_eq!(
            decoded,
            vec![
                cp,
                tail,
                other,
                Envelope::Ready {
                    workspace_id: 7,
                    pending_deltas: 3
                }
            ]
        );
        let pending = shared
            .lock()
            .await
            .store
            .load_pending(&peer.device_public_key)
            .unwrap();
        assert_eq!(
            pending.len(),
            1,
            "unknown pending entry was incorrectly retired"
        );
        assert_eq!(pending[0].bytes, opaque);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fresh_membership_read_releases_routing_lock_and_fences_replaced_session() {
        let (root, user) = UserIdentity::generate("alice", 1);
        let (a, _a_key) = certified_device(&root, user.public_key, "a");
        let (b, _) = certified_device(&root, user.public_key, "b");
        let room = uuid::Uuid::from_u128(7);
        let id = crate::room_workspace_id(&room.to_string()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut wr = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        wr.admit_device(a.device_public_key).unwrap();
        wr.admit_device(b.device_public_key).unwrap();
        let (atx, _arx) = mpsc::channel(2);
        let (btx, _brx) = mpsc::channel(2);
        let (ap, _apr) = mpsc::channel(2);
        let (bp, mut bpr) = mpsc::channel(1);
        let (disconnect, _disconnect_rx) = mpsc::channel(1);
        let (recipient_disconnect, _recipient_disconnect_rx) = mpsc::channel(1);
        for (peer, tx, private, session_disconnect) in [
            (&a, &atx, &ap, &disconnect),
            (&b, &btx, &bp, &recipient_disconnect),
        ] {
            wr.add_subscriber(Subscriber {
                device_public_key: peer.device_public_key,
                role: peer.role,
                tx: tx.clone(),
                disconnect: session_disconnect.clone(),
            });
            wr.action_targets.insert(
                peer.device_public_key,
                ActionTarget {
                    broker_host: None,
                    broker_protocol: false,
                    room,
                    peer: peer.clone(),
                    state_tx: tx.clone(),
                    tx: private.clone(),
                },
            );
        }
        let wr = Arc::new(AsyncMutex::new(wr));
        let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, wr.clone())])));
        let trust = Arc::new(std::sync::RwLock::new(
            crate::policy::TrustPolicy::AllowMatchingUser,
        ));
        let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
        let metrics = Arc::new(crate::metrics::Metrics::default());
        let session = SessionCtx {
            workspaces: &registry,
            autobind: None,
            outbound: &atx,
            state_outbound: None,
            connection_live: None,
            own_user_public_key: &user.public_key,
            own_device_cert: &a.cert,
            own_device_public_key: &a.cert.device_public_key,
            app_serving: None,
            metrics: &metrics,
            trust_policy: &trust,
            disconnect: &disconnect,
            framing: tilo_delivery_protocol::transport_v2::StateFraming::V2,
            presence: None,
            signed_actions: false,
            private_tx: &ap,
            revocations: &revocations,
            live_bridge: None,
            agents: Box::leak(Box::default()),
            positions: None,
            compactor: None,
            resumes: Box::leak(Box::default()),
            lag: Box::leak(Box::default()),
        };
        let receiver = SessionCtx {
            outbound: &btx,
            private_tx: &bp,
            disconnect: &recipient_disconnect,
            ..session
        };
        let action = DirectedAction {
            room,
            request_id: uuid::Uuid::from_u128(8),
            source: a.device_public_key,
            target: b.device_public_key,
            message: ActionMessage::CatalogueRefresh { scope: None },
        };

        let root_path = wr.lock().await.store.root().to_path_buf();
        let (release, reached) = crate::store::test_barrier::install(&root_path, "membership-read");
        let observed = wr.clone();
        let device = a.device_public_key;
        let replacement_disconnect = disconnect.clone();
        let handle = tokio::runtime::Handle::current();
        let observer = std::thread::spawn(move || {
            handle.block_on(async move {
                reached.await.unwrap();
                let lock = tokio::time::timeout(tilo_test_env::hang_bound(), observed.lock()).await;
                let unlocked = if let Ok(mut runtime) = lock {
                    let (replacement, _rx) = mpsc::channel(2);
                    runtime.add_subscriber(Subscriber {
                        device_public_key: device,
                        role: crate::auth::PeerRole::Device,
                        tx: replacement,
                        disconnect: replacement_disconnect,
                    });
                    true
                } else {
                    false
                };
                drop(release);
                unlocked
            })
        });
        let result = route_private(&action, &a, &session).await.unwrap();
        assert!(
            observer.join().unwrap(),
            "fresh membership read held routing lock"
        );
        assert_eq!(result, Some(UnavailableReason::NotAuthorised));
        assert!(bpr.try_recv().is_err());
        let _ = receiver;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn authorization_rechecks_certificates_after_blocking_membership_read() {
        let (root, user) = UserIdentity::generate("alice", 1);
        let (a, _a_key) = certified_device(&root, user.public_key, "a");
        let (b, _) = certified_device(&root, user.public_key, "b");
        let room = uuid::Uuid::from_u128(7);
        let id = crate::room_workspace_id(&room.to_string()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut wr = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        wr.admit_device(a.device_public_key).unwrap();
        wr.admit_device(b.device_public_key).unwrap();
        let (atx, _arx) = mpsc::channel(2);
        let (btx, _brx) = mpsc::channel(2);
        let (ap, _apr) = mpsc::channel(2);
        let (bp, mut bpr) = mpsc::channel(1);
        let (disconnect, _disconnect_rx) = mpsc::channel(1);
        let (recipient_disconnect, _recipient_disconnect_rx) = mpsc::channel(1);
        for (peer, tx, private, session_disconnect) in [
            (&a, &atx, &ap, &disconnect),
            (&b, &btx, &bp, &recipient_disconnect),
        ] {
            wr.add_subscriber(Subscriber {
                device_public_key: peer.device_public_key,
                role: peer.role,
                tx: tx.clone(),
                disconnect: session_disconnect.clone(),
            });
            wr.action_targets.insert(
                peer.device_public_key,
                ActionTarget {
                    broker_host: None,
                    broker_protocol: false,
                    room,
                    peer: peer.clone(),
                    state_tx: tx.clone(),
                    tx: private.clone(),
                },
            );
        }
        let wr = Arc::new(AsyncMutex::new(wr));
        let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, wr.clone())])));
        let trust = Arc::new(std::sync::RwLock::new(
            crate::policy::TrustPolicy::AllowMatchingUser,
        ));
        let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
        let metrics = Arc::new(crate::metrics::Metrics::default());
        let session = SessionCtx {
            workspaces: &registry,
            autobind: None,
            outbound: &atx,
            state_outbound: None,
            connection_live: None,
            own_user_public_key: &user.public_key,
            own_device_cert: &a.cert,
            own_device_public_key: &a.cert.device_public_key,
            app_serving: None,
            metrics: &metrics,
            trust_policy: &trust,
            disconnect: &disconnect,
            framing: tilo_delivery_protocol::transport_v2::StateFraming::V2,
            presence: None,
            signed_actions: false,
            private_tx: &ap,
            revocations: &revocations,
            live_bridge: None,
            agents: Box::leak(Box::default()),
            positions: None,
            compactor: None,
            resumes: Box::leak(Box::default()),
            lag: Box::leak(Box::default()),
        };
        let receiver = SessionCtx {
            outbound: &btx,
            private_tx: &bp,
            disconnect: &recipient_disconnect,
            ..session
        };
        let action = DirectedAction {
            room,
            request_id: uuid::Uuid::from_u128(8),
            source: a.device_public_key,
            target: b.device_public_key,
            message: ActionMessage::CatalogueRefresh { scope: None },
        };

        assert_eq!(route_private(&action, &a, &session).await.unwrap(), None);
        let PrivateOutbound::Delivery(delivery) = bpr.recv().await.unwrap() else {
            panic!("delivery");
        };
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: user.public_key,
            revoked_device_public_key: a.device_public_key,
            revoked_at_ns: 1,
            reason: tilo_user_identity::RevocationReason::Lost,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
        let root_path = wr.lock().await.store.root().to_path_buf();
        for operation in 0..5 {
            *revocations.write().unwrap() = RevocationList::new();
            let (release, reached) =
                crate::store::test_barrier::install(&root_path, "membership-read");
            let list = revocations.clone();
            let entry = entry.clone();
            let handle = tokio::runtime::Handle::current();
            let revoke = std::thread::spawn(move || {
                handle.block_on(async move {
                    reached.await.unwrap();
                    list.write().unwrap().insert(entry).unwrap();
                    drop(release);
                });
            });
            let denied = match operation {
                0 => {
                    route_private(&action, &a, &session).await.unwrap()
                        == Some(UnavailableReason::NotAuthorised)
                }
                1 => {
                    authorize_delivery(&delivery, &b, &receiver).await
                        == Err(UnavailableReason::NotAuthorised)
                }
                2 => handle_private(EnvelopeV2::EnableActions { room }, &a, &session)
                    .await
                    .is_err(),
                3 => handle_private(EnvelopeV2::EnableBrokerClient { room }, &a, &session)
                    .await
                    .is_err(),
                _ => handle_private(
                    EnvelopeV2::RegisterBroker {
                        room,
                        host: uuid::Uuid::from_u128(99),
                    },
                    &a,
                    &session,
                )
                .await
                .is_err(),
            };
            revoke.join().unwrap();
            assert!(
                denied,
                "operation {operation} authorized a certificate revoked during its membership read"
            );
        }
    }

    /// Counts what reaches the app-serving plane; needs no reply.
    #[derive(Default)]
    struct CountingPlane(std::sync::atomic::AtomicUsize);

    impl crate::app_serving::AppServingPlane for CountingPlane {
        fn serve(
            &self,
            _workspace_id: u64,
            _caller: &tilo_actions::remote::Caller,
            _message: tilo_actions::remote::RemoteActionMessage,
        ) -> Option<tilo_actions::remote::Pending<tilo_actions::remote::RemoteActionMessage>>
        {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            None
        }
    }

    #[tokio::test]
    async fn revoked_device_remote_action_is_refused() {
        let (root, user) = UserIdentity::generate("alice", 1);
        let (host, _) = certified_device(&root, user.public_key, "host");
        let (a, _) = certified_device(&root, user.public_key, "a");
        let id = 11;
        let tmp = tempfile::tempdir().unwrap();
        let mut wr = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        wr.admit_device(a.device_public_key).unwrap();
        let (atx, _arx) = mpsc::channel(2);
        let (ap, _apr) = mpsc::channel(2);
        let (disconnect, _disconnect_rx) = mpsc::channel(1);
        wr.add_subscriber(Subscriber {
            device_public_key: a.device_public_key,
            role: a.role,
            tx: atx.clone(),
            disconnect: disconnect.clone(),
        });
        let registry = Arc::new(AsyncMutex::new(HashMap::from([(
            id,
            Arc::new(AsyncMutex::new(wr)),
        )])));
        let trust = Arc::new(std::sync::RwLock::new(
            crate::policy::TrustPolicy::AllowMatchingUser,
        ));
        let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
        let metrics = Arc::new(crate::metrics::Metrics::default());
        let counting = Arc::new(CountingPlane::default());
        let plane: Arc<dyn crate::app_serving::AppServingPlane> = counting.clone();
        let session = SessionCtx {
            workspaces: &registry,
            autobind: None,
            outbound: &atx,
            state_outbound: None,
            connection_live: None,
            own_user_public_key: &user.public_key,
            own_device_cert: &host.cert,
            own_device_public_key: &host.cert.device_public_key,
            app_serving: Some(&plane),
            metrics: &metrics,
            trust_policy: &trust,
            disconnect: &disconnect,
            framing: tilo_delivery_protocol::transport_v2::StateFraming::V1,
            presence: None,
            signed_actions: false,
            private_tx: &ap,
            revocations: &revocations,
            live_bridge: None,
            agents: Box::leak(Box::default()),
            positions: None,
            compactor: None,
            resumes: Box::leak(Box::default()),
            lag: Box::leak(Box::default()),
        };
        let frame = tilo_actions::remote::RemoteActionFrame::new(
            tilo_actions::remote::RemoteActionMessage::CatalogueRequest {
                correlation: 1,
                query: tilo_actions::remote::CatalogueQuery::default(),
            },
        )
        .encode()
        .unwrap();
        let route = || RemoteActionRoute {
            workspace_id: id,
            version: crate::transport::REMOTE_ACTION_ENVELOPE_VERSION,
            to: host.device_public_key,
        };
        let served = || counting.0.load(std::sync::atomic::Ordering::SeqCst);

        // A current member reaches the plane through this very path.
        assert_eq!(
            handle_remote_action(&a, &session, &[id], route(), frame.clone()).await,
            None
        );
        assert_eq!(served(), 1);

        // Revoked while its session (and its `subscribed` list) is still live.
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: user.public_key,
            revoked_device_public_key: a.device_public_key,
            revoked_at_ns: 1,
            reason: tilo_user_identity::RevocationReason::Lost,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
        revocations.write().unwrap().insert(entry).unwrap();
        let refused = handle_remote_action(&a, &session, &[id], route(), frame).await;
        let Some(Envelope::RemoteActionUndeliverable {
            reason,
            to_device_public_key,
            ..
        }) = refused
        else {
            panic!("a revoked device is answered as undeliverable, got {refused:?}");
        };
        assert_eq!(to_device_public_key, host.device_public_key);
        assert_eq!(
            UnavailableReason::from_wire_reason(&reason),
            Some(UnavailableReason::NotAuthorised),
            "{reason}"
        );
        assert_eq!(served(), 1, "a revoked device never reaches the plane");
    }

    #[tokio::test]
    async fn route_private_refuses_an_agent_whose_delegation_expired() {
        let (root, user) = UserIdentity::generate("alice", 1);
        let (a, _) = certified_device(&root, user.public_key, "a");
        let (b, _) = certified_device(&root, user.public_key, "b");
        let room = uuid::Uuid::from_u128(7);
        let id = crate::room_workspace_id(&room.to_string()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut wr = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        wr.admit_device(a.device_public_key).unwrap();
        wr.admit_device(b.device_public_key).unwrap();
        let (atx, _arx) = mpsc::channel(2);
        let (btx, _brx) = mpsc::channel(2);
        let (ap, _apr) = mpsc::channel(2);
        let (bp, mut bpr) = mpsc::channel(4);
        let (disconnect, _disconnect_rx) = mpsc::channel(1);
        let (recipient_disconnect, _recipient_disconnect_rx) = mpsc::channel(1);
        for (peer, tx, private, session_disconnect) in [
            (&a, &atx, &ap, &disconnect),
            (&b, &btx, &bp, &recipient_disconnect),
        ] {
            wr.add_subscriber(Subscriber {
                device_public_key: peer.device_public_key,
                role: peer.role,
                tx: tx.clone(),
                disconnect: session_disconnect.clone(),
            });
            wr.action_targets.insert(
                peer.device_public_key,
                ActionTarget {
                    broker_host: None,
                    broker_protocol: false,
                    room,
                    peer: peer.clone(),
                    state_tx: tx.clone(),
                    tx: private.clone(),
                },
            );
        }
        let registry = Arc::new(AsyncMutex::new(HashMap::from([(
            id,
            Arc::new(AsyncMutex::new(wr)),
        )])));
        let trust = Arc::new(std::sync::RwLock::new(
            crate::policy::TrustPolicy::AllowMatchingUser,
        ));
        let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
        let metrics = Arc::new(crate::metrics::Metrics::default());
        let agents = AgentRegistry::default();
        let session = SessionCtx {
            workspaces: &registry,
            autobind: None,
            outbound: &atx,
            state_outbound: None,
            connection_live: None,
            own_user_public_key: &user.public_key,
            own_device_cert: &a.cert,
            own_device_public_key: &a.cert.device_public_key,
            app_serving: None,
            metrics: &metrics,
            trust_policy: &trust,
            disconnect: &disconnect,
            framing: tilo_delivery_protocol::transport_v2::StateFraming::V2,
            presence: None,
            signed_actions: false,
            private_tx: &ap,
            revocations: &revocations,
            live_bridge: None,
            agents: &agents,
            positions: None,
            compactor: None,
            resumes: Box::leak(Box::default()),
            lag: Box::leak(Box::default()),
        };
        let action = DirectedAction {
            room,
            request_id: uuid::Uuid::from_u128(8),
            source: a.device_public_key,
            target: b.device_public_key,
            message: ActionMessage::CatalogueRefresh { scope: None },
        };
        // Signed for this room, already past its expiry: what a delegation
        // looks like once its window closed under a live session.
        let expired = |device: [u8; 32]| {
            tilo_relay_protocol::agents::AgentDelegation::sign(
                &root,
                device,
                "helper",
                "Helper",
                [1, 2, 3],
                tilo_relay_protocol::agents::AgentScope {
                    rooms: vec![room.to_string()],
                    apps: Vec::new(),
                },
                1,
                2,
            )
            .unwrap()
        };

        assert_eq!(route_private(&action, &a, &session).await.unwrap(), None);
        assert!(matches!(bpr.try_recv(), Ok(PrivateOutbound::Delivery(_))));

        for agent in [a.device_public_key, b.device_public_key] {
            agents.write().unwrap().clear();
            agents.write().unwrap().insert(agent, Some(expired(agent)));
            assert_eq!(
                route_private(&action, &a, &session).await.unwrap(),
                Some(UnavailableReason::NotAuthorised),
                "an expired agent at either end is refused per frame"
            );
            assert!(bpr.try_recv().is_err(), "nothing reaches the target");
        }
    }

    #[tokio::test]
    async fn replacement_action_capability_requires_enable_then_durable_registration() {
        let (root, user) = UserIdentity::generate("alice", 1);
        let (source, _) = certified_device(&root, user.public_key, "source");
        let (broker, _) = certified_device(&root, user.public_key, "broker");
        let room = uuid::Uuid::from_u128(7);
        let host = uuid::Uuid::from_u128(99);
        let id = crate::room_workspace_id(&room.to_string()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut runtime = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        runtime.admit_device(source.device_public_key).unwrap();
        runtime.admit_device(broker.device_public_key).unwrap();
        let (source_state, _source_state_rx) = mpsc::channel(2);
        let (old_broker_state, _old_broker_state_rx) = mpsc::channel(2);
        let (replacement_state, _replacement_state_rx) = mpsc::channel(2);
        let (source_private, _source_private_rx) = mpsc::channel(2);
        let (broker_private, mut broker_private_rx) = mpsc::channel(2);
        let (source_disconnect, _source_disconnect_rx) = mpsc::channel(1);
        let (broker_disconnect, _broker_disconnect_rx) = mpsc::channel(1);
        runtime.add_subscriber(Subscriber {
            device_public_key: source.device_public_key,
            role: source.role,
            tx: source_state.clone(),
            disconnect: source_disconnect.clone(),
        });
        runtime.add_subscriber(Subscriber {
            device_public_key: broker.device_public_key,
            role: broker.role,
            tx: old_broker_state.clone(),
            disconnect: broker_disconnect.clone(),
        });
        let runtime = Arc::new(AsyncMutex::new(runtime));
        let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, runtime.clone())])));
        let trust = Arc::new(std::sync::RwLock::new(
            crate::policy::TrustPolicy::AllowMatchingUser,
        ));
        let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
        let metrics = Arc::new(crate::metrics::Metrics::default());
        let source_session = SessionCtx {
            workspaces: &registry,
            autobind: None,
            outbound: &source_state,
            state_outbound: None,
            connection_live: None,
            own_user_public_key: &user.public_key,
            own_device_cert: &source.cert,
            own_device_public_key: &source.cert.device_public_key,
            app_serving: None,
            metrics: &metrics,
            trust_policy: &trust,
            disconnect: &source_disconnect,
            framing: tilo_delivery_protocol::transport_v2::StateFraming::V2,
            presence: None,
            signed_actions: false,
            private_tx: &source_private,
            revocations: &revocations,
            live_bridge: None,
            agents: Box::leak(Box::default()),
            positions: None,
            compactor: None,
            resumes: Box::leak(Box::default()),
            lag: Box::leak(Box::default()),
        };
        assert!(matches!(
            handle_private(EnvelopeV2::EnableActions { room }, &source, &source_session)
                .await
                .unwrap(),
            Some(EnvelopeV2::ActionsReady {
                capability: ActionCapability::TransportOnly,
                ..
            })
        ));
        assert!(
            handle_private(
                EnvelopeV2::EnableBrokerClient { room },
                &source,
                &source_session
            )
            .await
            .unwrap()
            .is_none()
        );

        runtime.lock().await.add_subscriber(Subscriber {
            device_public_key: broker.device_public_key,
            role: broker.role,
            tx: replacement_state.clone(),
            disconnect: broker_disconnect.clone(),
        });
        let replacement_session = SessionCtx {
            outbound: &replacement_state,
            private_tx: &broker_private,
            disconnect: &broker_disconnect,
            ..source_session
        };
        let action = DirectedAction {
            room,
            request_id: uuid::Uuid::from_u128(8),
            source: source.device_public_key,
            target: broker.device_public_key,
            message: ActionMessage::InvokeBound {
                host,
                invocation: tilo_actions::InvocationId::new(),
                app_peer: "1".into(),
                app_id: "so.tilo.test".into(),
                record_id: "record".into(),
                resource: "record".into(),
                action: "test".into(),
                argument: None,
                confirmation: None,
                revision: None,
            },
        };
        assert_eq!(
            route_private(&action, &source, &source_session)
                .await
                .unwrap(),
            Some(UnavailableReason::UnsupportedPeer)
        );
        assert!(broker_private_rx.try_recv().is_err());

        assert_eq!(
            handle_private(
                EnvelopeV2::EnableActions { room },
                &broker,
                &replacement_session
            )
            .await
            .unwrap(),
            Some(EnvelopeV2::ActionsReady {
                room,
                capability: ActionCapability::TransportOnly,
            })
        );
        assert_eq!(
            route_private(&action, &source, &source_session)
                .await
                .unwrap(),
            Some(UnavailableReason::AdmissionUnavailable)
        );
        assert!(broker_private_rx.try_recv().is_err());

        assert_eq!(
            handle_private(
                EnvelopeV2::RegisterBroker { room, host },
                &broker,
                &replacement_session
            )
            .await
            .unwrap(),
            Some(EnvelopeV2::ActionsReady {
                room,
                capability: ActionCapability::DurableBroker { host },
            })
        );
        assert_eq!(
            route_private(&action, &source, &source_session)
                .await
                .unwrap(),
            None
        );
        assert!(matches!(
            broker_private_rx.recv().await,
            Some(PrivateOutbound::Delivery(_))
        ));

        runtime
            .lock()
            .await
            .remove_subscriber_session(&broker.device_public_key, &old_broker_state);
        let current = runtime.lock().await;
        assert!(
            current
                .subscribers
                .get(&broker.device_public_key)
                .is_some_and(|subscriber| subscriber.tx.same_channel(&replacement_state))
        );
        assert!(
            current
                .action_targets
                .get(&broker.device_public_key)
                .is_some_and(|target| target.broker_host == Some(host)
                    && target.state_tx.same_channel(&replacement_state))
        );
        drop(current);
        assert_eq!(
            route_private(&action, &source, &source_session)
                .await
                .unwrap(),
            None
        );
        assert!(matches!(
            broker_private_rx.recv().await,
            Some(PrivateOutbound::Delivery(_))
        ));
    }

    #[tokio::test]
    async fn bounded_private_queue_and_delivery_recheck_never_retain_traffic() {
        Box::pin(bounded_private_queue_for_room_namespace()).await;
    }

    async fn bounded_private_queue_for_room_namespace() {
        let (root, user) = UserIdentity::generate("alice", 1);
        let (a, a_key) = certified_device(&root, user.public_key, "a");
        let (b, _) = certified_device(&root, user.public_key, "b");
        let room = uuid::Uuid::from_u128(7);
        let id = crate::room_workspace_id(&room.to_string()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut wr = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        wr.admit_device(a.device_public_key).unwrap();
        wr.admit_device(b.device_public_key).unwrap();
        let (atx, _arx) = mpsc::channel(2);
        let (btx, _brx) = mpsc::channel(2);
        let (ap, mut apr) = mpsc::channel(2);
        let (bp, mut bpr) = mpsc::channel(1);
        let (disconnect, mut disconnect_rx) = mpsc::channel(1);
        let (recipient_disconnect, mut recipient_disconnect_rx) = mpsc::channel(1);
        for (peer, tx, private, session_disconnect) in [
            (&a, &atx, &ap, &disconnect),
            (&b, &btx, &bp, &recipient_disconnect),
        ] {
            wr.add_subscriber(Subscriber {
                device_public_key: peer.device_public_key,
                role: peer.role,
                tx: tx.clone(),
                disconnect: session_disconnect.clone(),
            });
            wr.action_targets.insert(
                peer.device_public_key,
                ActionTarget {
                    broker_host: None,
                    broker_protocol: false,
                    room,
                    peer: peer.clone(),
                    state_tx: tx.clone(),
                    tx: private.clone(),
                },
            );
        }
        let wr = Arc::new(AsyncMutex::new(wr));
        let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, wr.clone())])));
        let trust = Arc::new(std::sync::RwLock::new(
            crate::policy::TrustPolicy::AllowMatchingUser,
        ));
        let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
        let metrics = Arc::new(crate::metrics::Metrics::default());
        let session = SessionCtx {
            workspaces: &registry,
            autobind: None,
            outbound: &atx,
            state_outbound: None,
            connection_live: None,
            own_user_public_key: &user.public_key,
            own_device_cert: &a.cert,
            own_device_public_key: &a.cert.device_public_key,
            app_serving: None,
            metrics: &metrics,
            trust_policy: &trust,
            disconnect: &disconnect,
            framing: tilo_delivery_protocol::transport_v2::StateFraming::V2,
            presence: None,
            signed_actions: false,
            private_tx: &ap,
            revocations: &revocations,
            live_bridge: None,
            agents: Box::leak(Box::default()),
            positions: None,
            compactor: None,
            resumes: Box::leak(Box::default()),
            lag: Box::leak(Box::default()),
        };
        let receiver = SessionCtx {
            outbound: &btx,
            private_tx: &bp,
            disconnect: &recipient_disconnect,
            ..session
        };
        let action = DirectedAction {
            room,
            request_id: uuid::Uuid::from_u128(8),
            source: a.device_public_key,
            target: b.device_public_key,
            message: ActionMessage::CatalogueRefresh { scope: None },
        };
        assert!(
            !private_may_overtake(
                &EnvelopeV2::Directed(action.clone()),
                Some(id),
                false,
                std::iter::empty(),
            ),
            "private traffic must not overtake its room's state control"
        );
        // An old-v2 caller never receives an unknown broker message, even
        // when it targets an endpoint that installed the extended facade.
        wr.lock()
            .await
            .action_targets
            .get_mut(&b.device_public_key)
            .unwrap()
            .broker_host = Some(uuid::Uuid::from_u128(99));
        assert_eq!(
            route_private(&action, &a, &session).await.unwrap(),
            Some(UnavailableReason::UnsupportedPeer)
        );
        assert!(bpr.try_recv().is_err());
        wr.lock()
            .await
            .action_targets
            .get_mut(&b.device_public_key)
            .unwrap()
            .broker_host = None;
        // State resubscribe is idempotent on the same connection. It must not
        // silently withdraw a separately negotiated private capability.
        wr.lock().await.add_subscriber(Subscriber {
            device_public_key: b.device_public_key,
            role: b.role,
            tx: btx.clone(),
            disconnect: recipient_disconnect.clone(),
        });
        assert_eq!(route_private(&action, &a, &session).await.unwrap(), None);
        assert_eq!(
            route_private(&action, &a, &session).await.unwrap(),
            Some(UnavailableReason::Backpressure)
        );
        let PrivateOutbound::Delivery(delivery) = bpr.recv().await.unwrap() else {
            panic!("expected private delivery")
        };
        assert_eq!(authorize_delivery(&delivery, &b, &receiver).await, Ok(()));
        // Sender removal also invalidates an already queued private result.
        wr.lock()
            .await
            .store
            .save_membership(&[b.device_public_key])
            .unwrap();
        assert_eq!(
            route_private(&action, &a, &session).await.unwrap(),
            Some(UnavailableReason::NotAuthorised)
        );
        let mut sender_removed_wire = Vec::new();
        write_private_delivery(&delivery, &b, &receiver, &mut sender_removed_wire)
            .await
            .unwrap();
        assert!(sender_removed_wire.is_empty());
        assert!(matches!(
            apr.recv().await,
            Some(PrivateOutbound::Reply(EnvelopeV2::Unavailable {
                reason: UnavailableReason::NotAuthorised,
                ..
            }))
        ));
        wr.lock()
            .await
            .store
            .save_membership(&[a.device_public_key, b.device_public_key])
            .unwrap();

        // Root-signed revocation between dispatch and actual writer delivery.
        let mut entry = tilo_user_identity::RevocationEntry {
            user_public_key: user.public_key,
            revoked_device_public_key: a.device_public_key,
            revoked_at_ns: 1,
            reason: tilo_user_identity::RevocationReason::Lost,
            signature: [0; 64],
        };
        tilo_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
        revocations.write().unwrap().insert(entry).unwrap();
        let mut revoked_wire = Vec::new();
        write_private_delivery(&delivery, &b, &receiver, &mut revoked_wire)
            .await
            .unwrap();
        assert!(revoked_wire.is_empty());
        assert!(matches!(
            apr.recv().await,
            Some(PrivateOutbound::Reply(EnvelopeV2::Unavailable {
                reason: UnavailableReason::NotAuthorised,
                ..
            }))
        ));
        *revocations.write().unwrap() = RevocationList::new();

        // Expiry is rechecked at the writer, after the authenticated source's
        // queued delivery was created. Re-sign the short-lived certificate so
        // the fixture still uses a valid root-issued chain.
        let mut expiring = delivery.clone();
        expiring.source.cert.expires_at_ns = Some(unix_now_ns().saturating_add(100_000_000));
        sign_device_cert(&mut expiring.source.cert, &root).unwrap();
        expiring.source = crate::auth::accept_hello(
            &crate::auth::Hello {
                cert: expiring.source.cert.clone(),
                client_nonce: [9; 32],
            },
            &user.public_key,
            &crate::policy::TrustPolicy::AllowMatchingUser,
            &RevocationList::new(),
            unix_now_ns(),
        )
        .unwrap();
        crate::auth::verify_hello_ack2(
            &crate::auth::build_hello_ack2(&a_key, &[8; 32]),
            &expiring.source.device_public_key,
            &[8; 32],
        )
        .unwrap();
        assert_eq!(authorize_delivery(&expiring, &b, &receiver).await, Ok(()));
        tokio::time::sleep(std::time::Duration::from_millis(110)).await;
        let mut expired_wire = Vec::new();
        write_private_delivery(&expiring, &b, &receiver, &mut expired_wire)
            .await
            .unwrap();
        assert!(expired_wire.is_empty());
        assert!(matches!(
            apr.recv().await,
            Some(PrivateOutbound::Reply(EnvelopeV2::Unavailable {
                reason: UnavailableReason::NotAuthorised,
                ..
            }))
        ));

        // Host certificates retain their PeerRelay role; no facade exception.
        let mut relay_peer = a.clone();
        relay_peer.cert.capabilities = DeviceCapabilities::host();
        sign_device_cert(&mut relay_peer.cert, &root).unwrap();
        relay_peer = crate::auth::accept_hello(
            &crate::auth::Hello {
                cert: relay_peer.cert.clone(),
                client_nonce: [7; 32],
            },
            &user.public_key,
            &crate::policy::TrustPolicy::AllowMatchingUser,
            &RevocationList::new(),
            unix_now_ns(),
        )
        .unwrap();
        crate::auth::verify_hello_ack2(
            &crate::auth::build_hello_ack2(&a_key, &[6; 32]),
            &relay_peer.device_public_key,
            &[6; 32],
        )
        .unwrap();
        assert_eq!(relay_peer.role, crate::auth::PeerRole::PeerRelay);
        assert_eq!(
            route_private(&action, &relay_peer, &session).await.unwrap(),
            Some(UnavailableReason::NotAuthorised)
        );

        // Removal after dispatch but before recipient wire delivery fails closed.
        wr.lock()
            .await
            .store
            .save_membership(&[a.device_public_key])
            .unwrap();
        assert_eq!(
            authorize_delivery(&delivery, &b, &receiver).await,
            Err(UnavailableReason::NotAuthorised)
        );
        wr.lock()
            .await
            .store
            .save_membership(&[a.device_public_key, b.device_public_key])
            .unwrap();
        // Exact room equality is checked in addition to the numeric shard.
        wr.lock()
            .await
            .action_targets
            .get_mut(&b.device_public_key)
            .unwrap()
            .room = uuid::Uuid::from_u128(9);
        assert_eq!(
            route_private(&action, &a, &session).await.unwrap(),
            Some(UnavailableReason::NotAuthorised)
        );
        wr.lock()
            .await
            .action_targets
            .get_mut(&b.device_public_key)
            .unwrap()
            .room = room;
        let (replacement, _rx) = mpsc::channel(2);
        wr.lock().await.add_subscriber(Subscriber {
            device_public_key: b.device_public_key,
            role: b.role,
            tx: replacement.clone(),
            disconnect: recipient_disconnect.clone(),
        });
        assert_eq!(
            authorize_delivery(&delivery, &b, &receiver).await,
            Err(UnavailableReason::NotAuthorised)
        );
        wr.lock()
            .await
            .remove_subscriber_session(&b.device_public_key, &btx);
        assert!(
            wr.lock()
                .await
                .subscribers
                .get(&b.device_public_key)
                .unwrap()
                .tx
                .same_channel(&replacement)
        );
        // A recipient-side rejection must reach the original source, or close
        // that exact session when its reply lane has no remaining capacity.
        let mut wire = Vec::new();
        write_private_delivery(&delivery, &b, &receiver, &mut wire)
            .await
            .unwrap();
        assert!(wire.is_empty());
        let Some(PrivateOutbound::Reply(reply)) = apr.recv().await else {
            panic!("missing rejection")
        };
        assert_eq!(
            reply,
            EnvelopeV2::unavailable(&action, UnavailableReason::NotAuthorised)
        );
        for _ in 0..2 {
            ap.try_send(PrivateOutbound::Reply(reply.clone()))
                .ok()
                .unwrap();
        }
        let (new_source, new_source_rx) = mpsc::channel(2);
        let (new_disconnect, mut new_disconnect_rx) = mpsc::channel(1);
        wr.lock().await.add_subscriber(Subscriber {
            device_public_key: a.device_public_key,
            role: a.role,
            tx: new_source.clone(),
            disconnect: new_disconnect,
        });
        write_private_delivery(&delivery, &b, &receiver, &mut wire)
            .await
            .unwrap();
        assert!(
            wire.is_empty(),
            "rejected private body must not reach recipient wire"
        );
        assert_eq!(
            disconnect_rx.try_recv(),
            Ok(()),
            "a saturated rejection queue must terminate its original source session"
        );
        assert!(
            matches!(
                new_disconnect_rx.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ),
            "replacement source session must not be terminated"
        );
        assert!(!new_source_rx.is_closed());
        assert!(
            matches!(
                recipient_disconnect_rx.try_recv(),
                Err(mpsc::error::TryRecvError::Empty)
            ),
            "recipient session must not be terminated for source backpressure"
        );
        let wr = wr.lock().await;
        assert_eq!(wr.store.count_pending(&a.device_public_key).unwrap(), 0);
        assert_eq!(wr.store.count_pending(&b.device_public_key).unwrap(), 0);
        assert!(wr.store.load_retained().unwrap().is_empty());
        assert!(wr.store.load_snapshot().unwrap().is_none());
    }

    #[tokio::test]
    async fn a_device_presence_frame_rearms_its_live_checkpoint() {
        // Wiring for the per-device live checkpoint gate: after a device has been served one live
        // checkpoint the relay stops fanning checkpoints to it; a presence frame
        // from that device (the browser's hello / SyncReq / rejected-checkpoint
        // path) must re-arm exactly that device so the native's fresh
        // checkpoint reaches it.
        let (root, user) = UserIdentity::generate("alice", 1);
        let (native, _) = certified_device(&root, user.public_key, "native");
        let (browser, _) = certified_device(&root, user.public_key, "browser");
        let room = uuid::Uuid::from_u128(7);
        let id = crate::room_workspace_id(&room.to_string()).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let mut wr = crate::workspace::WorkspaceRuntime::open(
            tmp.path(),
            id,
            false,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        wr.admit_device(native.device_public_key).unwrap();
        wr.admit_device(browser.device_public_key).unwrap();
        let (btx, mut brx) = mpsc::channel(4);
        let (bdis, _bdis_rx) = mpsc::channel(1);
        wr.add_subscriber(Subscriber {
            device_public_key: browser.device_public_key,
            role: browser.role,
            tx: btx.clone(),
            disconnect: bdis.clone(),
        });
        // Serve the subscribe-time need with one checkpoint from the native.
        let raw = crate::coedit::CoeditPayload::Checkpoint {
            channel: [1; 32],
            body: vec![1],
        }
        .encode()
        .unwrap();
        let checkpoint_at = |authored_at_ns: u64| Envelope::Delta {
            workspace_id: id,
            author_device_public_key: native.device_public_key,
            authored_at_ns,
            delta: raw.clone(),
            signature: [0; 64],
        };
        let env = checkpoint_at(1);
        wr.fanout_delta(&native.device_public_key, native.role, &env, &raw, 1)
            .unwrap();
        brx.try_recv().unwrap();
        assert!(!wr.needs_checkpoint(&browser.device_public_key, &[1; 32]));

        let wr = Arc::new(AsyncMutex::new(wr));
        let registry = Arc::new(AsyncMutex::new(HashMap::from([(id, wr.clone())])));
        let trust = Arc::new(std::sync::RwLock::new(
            crate::policy::TrustPolicy::AllowMatchingUser,
        ));
        let revocations = Arc::new(std::sync::RwLock::new(RevocationList::new()));
        let metrics = Arc::new(crate::metrics::Metrics::default());
        let (ptx, _prx) = mpsc::channel(1);
        let session = SessionCtx {
            workspaces: &registry,
            autobind: None,
            outbound: &btx,
            state_outbound: None,
            connection_live: None,
            own_user_public_key: &user.public_key,
            own_device_cert: &browser.cert,
            own_device_public_key: &browser.cert.device_public_key,
            app_serving: None,
            metrics: &metrics,
            trust_policy: &trust,
            disconnect: &bdis,
            framing: tilo_delivery_protocol::transport_v2::StateFraming::V1,
            presence: None,
            signed_actions: false,
            private_tx: &ptx,
            revocations: &revocations,
            live_bridge: None,
            agents: Box::leak(Box::default()),
            positions: None,
            compactor: None,
            resumes: Box::leak(Box::default()),
            lag: Box::leak(Box::default()),
        };
        // The browser sends a SyncReq as presence.
        handle_presence(
            &browser,
            &session,
            &[id],
            Envelope::Presence {
                workspace_id: id,
                payload: vec![9, 9],
            },
        )
        .await
        .unwrap();
        assert!(
            wr.lock()
                .await
                .needs_checkpoint(&browser.device_public_key, &[1; 32]),
            "a device presence frame must re-arm its live checkpoint"
        );
        // The native's fresh checkpoint now reaches it, once.
        let env2 = checkpoint_at(2);
        wr.lock()
            .await
            .fanout_delta(&native.device_public_key, native.role, &env2, &raw, 2)
            .unwrap();
        assert!(brx.try_recv().is_ok());
        let env3 = checkpoint_at(3);
        wr.lock()
            .await
            .fanout_delta(&native.device_public_key, native.role, &env3, &raw, 3)
            .unwrap();
        assert!(brx.try_recv().is_err(), "satisfied by one checkpoint");

        let (native_tx, mut native_rx) = mpsc::channel(4);
        let (native_disconnect, _native_disconnect_rx) = mpsc::channel(1);
        wr.lock().await.add_subscriber(Subscriber {
            device_public_key: native.device_public_key,
            role: native.role,
            tx: native_tx,
            disconnect: native_disconnect,
        });
        // The browser claims another admitted device's author identity.
        let forged = crate::AttributedPresence {
            author: native.device_public_key,
            body: b"fixture presence".to_vec(),
        };
        handle_presence(
            &browser,
            &session,
            &[id],
            Envelope::Presence {
                workspace_id: id,
                payload: forged.encode().unwrap(),
            },
        )
        .await
        .unwrap();
        let Envelope::Presence { payload, .. } = native_rx.try_recv().unwrap() else {
            panic!("expected forwarded presence");
        };
        let observed = crate::AttributedPresence::decode(&payload).unwrap();
        assert_eq!(observed.author, browser.device_public_key);
        assert_eq!(observed.body, forged.body);
    }
}

#[cfg(test)]
mod retention_diagnostic_tests {
    use super::*;

    fn retained(
        workspace: u64,
        author: u8,
        channel: u8,
        checkpoint: bool,
        body_bytes: usize,
    ) -> EnvelopeV2 {
        let payload = if checkpoint {
            crate::coedit::CoeditPayload::Checkpoint {
                channel: [channel; 32],
                body: vec![5; body_bytes],
            }
        } else {
            crate::coedit::CoeditPayload::RetainedDelta {
                channel: [channel; 32],
                body: vec![5; body_bytes],
            }
        };
        EnvelopeV2::State(Envelope::Delta {
            workspace_id: workspace,
            author_device_public_key: [author; 32],
            authored_at_ns: 1,
            delta: payload.encode().unwrap(),
            signature: [0; 64],
        })
    }

    #[test]
    fn buffered_prefix_reports_bounded_payload_free_shape() {
        let frames = (0..25)
            .map(|channel| retained(7, 1, channel, true, 3))
            .collect::<Vec<_>>();
        let shape = buffered_retention_prefix(frames.iter()).unwrap();
        assert_eq!(shape.frames, 25);
        assert_eq!(shape.checkpoints, 25);
        assert_eq!(shape.deltas, 0);
        assert_eq!(shape.distinct_channels, 25);
        assert_eq!(shape.stop_reason, "end");
        assert!(!shape.frame_cap_reached);
        assert!(!shape.byte_cap_reached);
        assert!(shape.input_bytes > 25 * 256);
    }

    #[test]
    fn buffered_prefix_stops_at_identity_kind_and_resource_bounds() {
        for (barrier, expected) in [
            (retained(8, 1, 2, true, 3), "workspace_change"),
            (retained(7, 2, 2, true, 3), "author_change"),
            (EnvelopeV2::State(Envelope::Ping), "non_delta"),
            (
                EnvelopeV2::State(Envelope::Delta {
                    workspace_id: 7,
                    author_device_public_key: [1; 32],
                    authored_at_ns: 1,
                    delta: vec![9],
                    signature: [0; 64],
                }),
                "non_retained",
            ),
        ] {
            let frames = [retained(7, 1, 1, false, 3), barrier];
            let shape = buffered_retention_prefix(frames.iter()).unwrap();
            assert_eq!(shape.frames, 1);
            assert_eq!(shape.checkpoints, 0);
            assert_eq!(shape.deltas, 1);
            assert_eq!(shape.stop_reason, expected);
        }

        for (first, expected) in [
            (EnvelopeV2::State(Envelope::Ping), "non_delta"),
            (
                EnvelopeV2::State(Envelope::Delta {
                    workspace_id: 7,
                    author_device_public_key: [1; 32],
                    authored_at_ns: 1,
                    delta: vec![9],
                    signature: [0; 64],
                }),
                "non_retained",
            ),
        ] {
            let frames = [first];
            let shape = buffered_retention_prefix(frames.iter()).unwrap();
            assert_eq!(shape.frames, 0);
            assert_eq!(shape.stop_reason, expected);
        }

        let frames = [
            retained(7, 1, 1, true, 3),
            retained(7, 1, 2, true, 3),
            retained(7, 1, 3, true, 3),
        ];
        let frame_limited =
            buffered_retention_prefix_with_limits(frames.iter(), 2, usize::MAX).unwrap();
        assert_eq!(frame_limited.frames, 2);
        assert!(frame_limited.frame_cap_reached);
        assert_eq!(frame_limited.stop_reason, "frame_cap");

        let full_backlog = (0..RETENTION_PREFIX_FRAME_CAP)
            .map(|channel| retained(7, 1, u8::try_from(channel).unwrap(), true, 3))
            .collect::<Vec<_>>();
        let exact_cap = buffered_retention_prefix(full_backlog.iter()).unwrap();
        assert_eq!(exact_cap.frames, RETENTION_PREFIX_FRAME_CAP);
        assert!(exact_cap.frame_cap_reached);
        assert_eq!(exact_cap.stop_reason, "frame_cap");

        let first_bytes = inbound_bytes(&frames[0]);
        let byte_limited =
            buffered_retention_prefix_with_limits(frames.iter(), 32, first_bytes).unwrap();
        assert_eq!(byte_limited.frames, 1);
        assert!(byte_limited.byte_cap_reached);
        assert_eq!(byte_limited.stop_reason, "byte_cap");

        let first_too_large =
            buffered_retention_prefix_with_limits(frames.iter(), 32, first_bytes - 1).unwrap();
        assert_eq!(first_too_large.frames, 0);
        assert!(first_too_large.byte_cap_reached);
        assert_eq!(first_too_large.stop_reason, "byte_cap");
    }

    #[test]
    fn eligible_checkpoint_prefix_requires_a_coherent_distinct_checkpoint_run() {
        assert_eq!(eligible_checkpoint_prefix(&Envelope::Ping, [].iter()), 1);
        let EnvelopeV2::State(current) = retained(7, 1, 1, true, 3) else {
            unreachable!()
        };
        let suffix = [
            retained(7, 1, 2, true, 3),
            retained(7, 1, 2, true, 3),
            retained(7, 1, 3, false, 3),
            retained(7, 1, 4, true, 3),
        ];
        let suffix_before = suffix
            .iter()
            .map(|frame| tilo_delivery_protocol::transport_v2::encode(frame).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(eligible_checkpoint_prefix(&current, suffix.iter()), 2);
        let suffix_after = suffix
            .iter()
            .map(|frame| tilo_delivery_protocol::transport_v2::encode(frame).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(suffix_before, suffix_after);
    }

    #[test]
    fn checkpoint_batch_mode_requires_an_exact_completed_projection_shape() {
        use crate::store::shell_selection::{
            BatchProjectionReason, BatchSelectionOutcome, IncomingDisposition, Publication,
        };
        let represented = BatchSelectionOutcome {
            supplemental_recovery: None,
            publication: Publication::Suppress,
            dispositions: vec![IncomingDisposition::Covered; 2],
            reason: BatchProjectionReason::Represented,
        };
        assert_eq!(
            checkpoint_mode_for(Some(&represented), 2),
            CheckpointBatchMode::Represented
        );
        let all_original = BatchSelectionOutcome {
            supplemental_recovery: None,
            publication: Publication::Original,
            dispositions: vec![IncomingDisposition::Original; 2],
            reason: BatchProjectionReason::FinalBasisOmission,
        };
        assert_eq!(
            checkpoint_mode_for(Some(&all_original), 2),
            CheckpointBatchMode::AllOriginal
        );
        for rejected in [
            BatchSelectionOutcome {
                supplemental_recovery: None,
                publication: Publication::Original,
                dispositions: vec![IncomingDisposition::Original],
                reason: BatchProjectionReason::FinalBasisOmission,
            },
            BatchSelectionOutcome {
                supplemental_recovery: None,
                publication: Publication::Original,
                dispositions: vec![IncomingDisposition::Covered, IncomingDisposition::Original],
                reason: BatchProjectionReason::FinalBasisOmission,
            },
            BatchSelectionOutcome {
                supplemental_recovery: None,
                publication: Publication::Activate(std::path::PathBuf::from("selected")),
                dispositions: vec![IncomingDisposition::Original; 2],
                reason: BatchProjectionReason::FinalBasisOmission,
            },
            BatchSelectionOutcome {
                supplemental_recovery: None,
                publication: Publication::Original,
                dispositions: vec![IncomingDisposition::Original; 2],
                reason: BatchProjectionReason::UnresolvedTailDependencies,
            },
        ] {
            assert_eq!(
                checkpoint_mode_for(Some(&rejected), 2),
                CheckpointBatchMode::Serial
            );
        }
        assert_eq!(checkpoint_mode_for(None, 2), CheckpointBatchMode::Serial);
    }

    #[test]
    fn eligible_checkpoint_prefix_stops_at_every_identity_kind_and_malformed_barrier() {
        let EnvelopeV2::State(current) = retained(7, 1, 1, true, 3) else {
            unreachable!()
        };
        for barrier in [
            retained(7, 1, 9, false, 3),
            retained(8, 1, 2, true, 3),
            retained(7, 2, 2, true, 3),
            EnvelopeV2::State(Envelope::Ping),
            EnvelopeV2::State(Envelope::Delta {
                workspace_id: 7,
                author_device_public_key: [1; 32],
                authored_at_ns: 1,
                delta: vec![9],
                signature: [0; 64],
            }),
        ] {
            assert_eq!(
                eligible_checkpoint_prefix(&current, [retained(7, 1, 2, true, 3), barrier].iter()),
                2
            );
        }
    }

    #[test]
    fn eligible_checkpoint_prefix_includes_current_and_enforces_frame_and_byte_caps() {
        let EnvelopeV2::State(current) = retained(7, 1, 1, true, 3) else {
            unreachable!()
        };
        let full = (2..=34)
            .map(|channel| retained(7, 1, channel, true, 3))
            .collect::<Vec<_>>();
        assert_eq!(eligible_checkpoint_prefix(&current, full.iter()), 32);

        let EnvelopeV2::State(retained_delta) = retained(7, 1, 2, false, 3) else {
            unreachable!()
        };
        assert_eq!(eligible_checkpoint_prefix(&retained_delta, [].iter()), 1);
        let malformed = Envelope::Delta {
            workspace_id: 7,
            author_device_public_key: [1; 32],
            authored_at_ns: 1,
            delta: vec![9],
            signature: [0; 64],
        };
        assert_eq!(eligible_checkpoint_prefix(&malformed, [].iter()), 1);

        let EnvelopeV2::State(large) = retained(7, 1, 2, true, RETENTION_PREFIX_BYTE_CAP) else {
            unreachable!()
        };
        assert_eq!(eligible_checkpoint_prefix(&large, [].iter()), 1);
        assert_eq!(eligible_checkpoint_prefix(&current, [].iter()), 1);
        assert_eq!(
            eligible_checkpoint_prefix(&current, [EnvelopeV2::State(large)].iter()),
            1
        );
    }

    #[test]
    fn completed_publication_labels_are_fixed_and_payload_free() {
        use crate::store::shell_selection::Publication;

        assert_eq!(
            retention_publication_kind(&Publication::Suppress),
            "suppress"
        );
        assert_eq!(
            retention_publication_kind(&Publication::Original),
            "original"
        );
        assert_eq!(
            retention_publication_kind(&Publication::Activate(std::path::PathBuf::from("ignored"))),
            "activate"
        );
    }
}

#[cfg(test)]
mod state_tests;

#[cfg(test)]
#[path = "peer_state_delivery_tests.rs"]
mod state_delivery_tests;
