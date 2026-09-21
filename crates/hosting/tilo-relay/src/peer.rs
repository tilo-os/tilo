//! Per-connection peer task — reads envelopes from the wire,
//! dispatches to the workspace runtime, drains the outbound queue
//! back to the wire.
//!
//! The connection carries **one** bidirectional stream. Reads and
//! writes run concurrently: a `tokio::select!` between the wire
//! reader and the outbound mpsc receiver. The receiver stops when
//! the connection closes; the reader stops on EOF / decode error;
//! either termination cancels the other via drop.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use crate::auth::AuthenticatedPeer;
use crate::peer_actions::{ActionMessage, DirectedAction, UnavailableReason};
use crate::persistence::{ReplayAdvance, ReplayHandle, ReplayTicket, WrittenBatch};
use crate::store::StoreError;
use crate::store::replay::Step;
use crate::transport::Envelope;
use crate::transport_v2::{ActionCapability, EnvelopeV2, SessionReader};
use crate::workspace::{SharedWorkspace, Subscriber};

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
    Absent,
    Armed,
    Fired,
}

#[cfg(feature = "test-support")]
pub fn arm_checkpoint_prefix_post_first_promotion(root: &std::path::Path) -> std::io::Result<()> {
    let armed = root.join(CHECKPOINT_PREFIX_FAULT_ARMED);
    let fired = root.join(CHECKPOINT_PREFIX_FAULT_FIRED);
    match std::fs::remove_file(fired) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(armed)?
        .sync_all()
}

#[cfg(feature = "test-support")]
pub fn checkpoint_prefix_post_first_promotion_state(
    root: &std::path::Path,
) -> CheckpointPrefixPromotionFaultState {
    if root.join(CHECKPOINT_PREFIX_FAULT_FIRED).is_file() {
        CheckpointPrefixPromotionFaultState::Fired
    } else if root.join(CHECKPOINT_PREFIX_FAULT_ARMED).is_file() {
        CheckpointPrefixPromotionFaultState::Armed
    } else {
        CheckpointPrefixPromotionFaultState::Absent
    }
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
    Frame(#[from] crate::transport::FrameError),
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

/// Wall-clock timestamp for protocol and diagnostics. Ordered state owners
/// allocate durable monotonic receipt sequences separately.
pub fn now_ns() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        // `u64` nanoseconds since the epoch doesn't overflow until the
        // year 2554; clamp rather than panic on the (unreachable in
        // practice) far side of that.
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// Relay-wide state a peer session needs for its whole lifetime — the
/// workspace registry, autobind policy, live revocation list, own
/// user chain, and metrics. Bundled into one struct so [`run_peer`]'s
/// argument count stays reasonable; see [`SessionCtx`] for the subset
/// [`handle_envelope`] reads on every message.
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
    /// Metrics — cheap counters.
    pub metrics: Arc<crate::metrics::Metrics>,
}

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
    metrics: &'a Arc<crate::metrics::Metrics>,
    trust_policy: &'a Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    disconnect: &'a mpsc::Sender<()>,
    v2: bool,
    private_tx: &'a mpsc::Sender<PrivateOutbound>,
    revocations: &'a Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
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
    run_peer_version(peer, connection, read, write, ctx, false).await
}

pub(crate) async fn run_peer_version<R, W>(
    peer: AuthenticatedPeer,
    connection: iroh::endpoint::Connection,
    read: R,
    write: W,
    ctx: PeerContext,
    v2: bool,
) -> Result<(), PeerError>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    run_peer_stream(peer, read, write, ctx, v2, connection.closed()).await
}

// Shared stream driver also exercised with a deterministic in-memory wire.
async fn run_peer_stream<R, W>(
    peer: AuthenticatedPeer,
    read: R,
    mut write: W,
    ctx: PeerContext,
    v2: bool,
    closed: impl std::future::Future,
) -> Result<(), PeerError>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    tokio::pin!(closed);
    let PeerContext {
        workspaces,
        autobind,
        revocations,
        trust_policy,
        own_user_public_key,
        metrics,
    } = ctx;

    // Outbound channel — every workspace runtime this peer is
    // subscribed to writes into it; we drain to the wire.
    let (tx, mut rx) = mpsc::channel::<Envelope>(PEER_OUTBOUND_CAPACITY);
    let (state_tx, mut state_rx) = mpsc::channel::<StateDelivery>(PEER_OUTBOUND_CAPACITY);
    let (disconnect_tx, mut disconnect_rx) = mpsc::channel::<()>(1);
    let mut reader = SessionReader::new(read, v2);
    let (private_tx, mut private_rx) = mpsc::channel(PEER_OUTBOUND_CAPACITY);
    // Track our subscriptions so we can unregister on disconnect.
    let mut subscribed: Vec<u64> = Vec::new();
    let mut access_audit = tokio::time::interval(std::time::Duration::from_secs(1));
    access_audit.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    access_audit.tick().await;

    let connection_live = ConnectionLease(Arc::new(std::sync::atomic::AtomicBool::new(true)));
    let session = SessionCtx {
        workspaces: &workspaces,
        autobind: autobind.as_ref(),
        outbound: &tx,
        state_outbound: Some(&state_tx),
        connection_live: Some(&connection_live.0),
        own_user_public_key: &own_user_public_key,
        metrics: &metrics,
        trust_policy: &trust_policy,
        disconnect: &disconnect_tx,
        v2,
        private_tx: &private_tx,
        revocations: &revocations,
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
        loop {
            let (frame, inbound_budget) = if let Some((frame, budget)) = backlog.pop_front() {
                (frame, Some(budget))
            } else {
                tokio::select! {
                    _ = access_audit.tick() => { audit_subscriber_access(&revocations, &peer, &session, &subscribed).await?; continue; }
                    frame = reader.read_next() => match frame? { Some(frame) => (frame, None), None => return Ok::<(), PeerError>(()) }
                }
            };
            match frame {
                EnvelopeV2::State(env) => {
                    let prefix_len =
                        eligible_checkpoint_prefix(&env, backlog.iter().map(|(frame, _)| frame));
                    let mut prefix = vec![env];
                    let mut prefix_budgets = vec![inbound_budget];
                    for _ in 1..prefix_len {
                        let Some((EnvelopeV2::State(next), budget)) = backlog.pop_front() else {
                            unreachable!("eligible checkpoint prefix changed while draining")
                        };
                        prefix.push(next);
                        prefix_budgets.push(Some(budget));
                    }
                    let env = prefix.remove(0);
                    let replaying = matches!(env, Envelope::Subscribe { .. });
                    let blocked_room = match &env {
                        Envelope::Subscribe { workspace_id }
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
                                let mut envelopes = Vec::with_capacity(prefix.len() + 1);
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
                                frame = reader.read_next(), if backlog.len() < 32 => {
                                    let Some(frame) = frame? else { return Ok(()); };
                                    let can_route = private_may_overtake(&frame, blocked_room, backlog.iter().map(|(frame, _)| frame));
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
                                            private_tx.try_send(PrivateOutbound::Reply(reply)).map_err(|_| PeerError::Protocol("private reply backpressure".into()))?;
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
                                    result.map_err(|_| PeerError::Protocol("reply backpressure".into()))?;
                                    break;
                                }
                                frame = reader.read_next(), if backlog.len() < 32 => {
                                    let Some(frame) = frame? else { return Ok(()); };
                                    let can_route = private_may_overtake(&frame, None, backlog.iter().map(|(frame, _)| frame));
                                    if can_route {
                                        if let Some(reply) = handle_private(frame, &peer, &session).await? {
                                            private_tx.try_send(PrivateOutbound::Reply(reply)).map_err(|_| PeerError::Protocol("private reply backpressure".into()))?;
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
                        private_tx
                            .try_send(PrivateOutbound::Reply(reply))
                            .map_err(|_| {
                                PeerError::Protocol("private reply backpressure".into())
                            })?;
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
                finish_replay_write(
                    batch,
                    &peer,
                    &session,
                    &pending_replays,
                    &mut private_rx,
                    &mut write,
                )
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
                    write
                        .write_all(&crate::transport_v2::encode_state(&frame, v2)?)
                        .await?;
                    private_burst = 0;
                    raw_turn = false;
                    continue;
                }
                if let Some(state) = deferred_state.take() {
                    write_state_delivery(state, &peer, &session, &mut write).await?;
                    private_burst = 0;
                    raw_turn = true;
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
        Err(PeerError::Superseded) => Ok(()),
        Err(error) => {
            metrics.inc_subscriber_dropped();
            tracing::warn!(
                device = %hex::encode(&device[..4]),
                %error,
                "peer session ended with error"
            );
            Err(error)
        }
        Ok(()) => Ok(()),
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
            Envelope::Subscribe { workspace_id } | Envelope::Unsubscribe { workspace_id },
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
    mut queued: impl Iterator<Item = &'a EnvelopeV2>,
) -> bool {
    let EnvelopeV2::Directed(action) = frame else {
        return false;
    };
    let Ok(rooms) = crate::room_workspace_ids(&action.room.to_string()) else {
        return false;
    };
    !active_control.is_some_and(|room| rooms.contains(&room))
        && queued.all(|frame| !control_room(frame).is_some_and(|room| rooms.contains(&room)))
}

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
        total_bytes += frame_bytes;
        total += 1;
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
                shape.stop_reason = if expected.0 != current.0 {
                    "workspace_change"
                } else {
                    "author_change"
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
        shape.frames += 1;
        shape.input_bytes += bytes;
        channels.insert(channel);
        match kind {
            crate::coedit::RetentionKind::Checkpoint => shape.checkpoints += 1,
            crate::coedit::RetentionKind::Delta => shape.deltas += 1,
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
        crate::store::shell_selection::Publication::Original => "original",
    }
}

struct ReplayWrite {
    ticket: Option<(u64, ReplayTicket, ReplayHandle, Arc<ReplayCatchup>)>,
    reply: Option<Envelope>,
    replaying: bool,
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
                .write_all(&crate::transport_v2::encode(&reply)?)
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
    let ws = session
        .workspaces
        .lock()
        .await
        .get(&id)
        .cloned()
        .ok_or_else(|| PeerError::Protocol("workspace disappeared".into()))?;
    let wr = crate::workspace_io::fresh_membership(&ws).await?;
    if !session_peer_authorized(peer, session)
        || !state_member(&wr, peer)
        || ((subscribed || wr.subscribers.contains_key(&peer.device_public_key))
            && !current_session(&wr, peer, session.outbound))
    {
        return Err(PeerError::Protocol(
            "state recipient authorization changed".into(),
        ));
    }
    Ok(())
}
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
                            .write_all(&crate::transport_v2::encode_state(frame, session.v2)?)
                            .await?;
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
                                authority.as_ref().expect("replay cleanup"),
                                &cleanup_handle,
                                |store| {
                                    authorize_replay_now(
                                        store,
                                        &checking_workspace,
                                        &checking_source,
                                        authority.as_ref().expect("replay cleanup"),
                                    )
                                },
                            )
                        },
                        |result| async { result },
                    );
                    let _ = replay_wait(cleanup, peer, session, private_rx, write).await?;
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
                                match state.capture_pending_replay(&capture)? {
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
                        if let Some(catchup_handle) =
                            replay_wait(boundary, peer, session, private_rx, write).await?
                        {
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
                    crate::store::test_barrier::pause(&root, "replay-before-ready")
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
            .write_all(&crate::transport_v2::encode_state(&reply, session.v2)?)
            .await?;
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
        .unwrap()
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
    let replay = session.replay.as_ref().unwrap();
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
fn authorize_replay_now(
    store: &crate::store::WorkspaceStore,
    workspace: &SharedWorkspace,
    source: &StateSource,
    authority: &crate::persistence::ReplayAuthority,
) -> Result<(), StoreError> {
    let members = store.load_membership()?;
    let runtime = workspace.blocking_lock();
    let member = (members.contains(&source.peer.device_public_key)
        && runtime
            .membership_ceiling
            .as_ref()
            .is_none_or(|ceiling| ceiling.contains(&source.peer.device_public_key)))
        || runtime
            .admitted_users
            .contains(&source.peer.user_public_key)
        || (source.peer.role == crate::auth::PeerRole::PeerRelay
            && runtime
                .known_peer_relays
                .contains(&source.peer.device_public_key));
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
    let mut private_open = session.v2;
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

/// Periodic membership + revocation re-check for every workspace this
/// peer is subscribed to. Reloads membership from disk each tick —
/// the only way a removal made while a device is connected takes
/// effect promptly — and ends the session if the device's own cert
/// was revoked or it fell out of membership.
async fn audit_subscriber_access(
    revocations: &Arc<std::sync::RwLock<tilo_user_identity::RevocationList>>,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
) -> Result<(), PeerError> {
    if revocations
        .read()
        .is_ok_and(|list| list.is_revoked(&peer.device_public_key))
    {
        return Err(PeerError::Protocol(
            "authenticated device was revoked".into(),
        ));
    }
    if !session
        .trust_policy
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .admits(&peer.cert, session.own_user_public_key)
    {
        return Err(PeerError::Protocol(
            "collaborator was removed from signed workspace authority".into(),
        ));
    }
    for workspace_id in subscribed {
        let Some(workspace) = session.workspaces.lock().await.get(workspace_id).cloned() else {
            continue;
        };
        let workspace = crate::workspace_io::fresh_membership(&workspace).await?;
        if !session_peer_authorized(peer, session)
            || !current_session(&workspace, peer, session.outbound)
        {
            return Err(PeerError::Protocol(
                "subscriber authorization changed".into(),
            ));
        }
        let is_federated = peer.role == crate::auth::PeerRole::PeerRelay
            && peer.user_public_key == *session.own_user_public_key
            && workspace
                .known_peer_relays
                .contains(&peer.device_public_key);
        if !workspace.admit_authenticated
            && !workspace.admitted_users.contains(&peer.user_public_key)
            && !workspace.membership.contains(&peer.device_public_key)
            && !is_federated
        {
            return Err(PeerError::Protocol(
                "device was removed from workspace membership".into(),
            ));
        }
    }
    Ok(())
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
    let replaying = matches!(env, Envelope::Subscribe { .. });
    let mut ticket = None;
    let reply = handle_envelope(peer, env, session, subscribed, &mut ticket).await?;
    let pending = std::sync::atomic::AtomicUsize::new(usize::from(replaying));
    let (_tx, mut private_rx) = mpsc::channel(1);
    finish_replay_write(
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
    )
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
            handle_subscribe(peer, session, subscribed, replay, workspace_id).await
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
        // Envelopes the relay only *sends*, never receives; ignore
        // silently so a peer bug doesn't wedge the connection.
        Envelope::Ready { .. }
        | Envelope::NotAuthorised { .. }
        | Envelope::Pong
        | Envelope::SyncResponse { .. } => Ok(None),
    }
}

/// Handle `Subscribe`: gate on membership (or admit, for a
/// same-owner autobind), replay retained baselines + this device's
/// pending queue, then register the live subscriber.
async fn handle_subscribe(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &mut Vec<u64>,
    replay: &mut Option<(u64, ReplayTicket, ReplayHandle, Arc<ReplayCatchup>)>,
    workspace_id: u64,
) -> Result<Option<Envelope>, PeerError> {
    let ws = get_or_autobind_workspace(session.workspaces, session.autobind, workspace_id).await?;
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
    // Membership gates DEVICE subscribers. Peer-relay subscribers are
    // already gated by cross-user auth (they must chain to the same
    // user root, same handshake as any device) plus the
    // `can_act_as_host` capability — they don't need to appear in the
    // workspace membership list to legitimately federate.
    let is_federated = peer.role == crate::auth::PeerRole::PeerRelay
        && peer.user_public_key == *session.own_user_public_key
        && wr.known_peer_relays.contains(&peer.device_public_key);
    if !wr.membership.contains(&peer.device_public_key)
        && !wr.admitted_users.contains(&peer.user_public_key)
        && !is_federated
    {
        if wr.admit_authenticated {
            let revision = wr.membership_revision;
            drop(wr);
            crate::workspace_io::admit_member(&ws, peer.device_public_key, revision).await?;
            wr = crate::workspace_io::fresh_membership(&ws).await?;
            if !session_peer_authorized(peer, session)
                || !current_member(&wr, peer)
                || (subscribed.contains(&workspace_id)
                    && !current_session(&wr, peer, session.outbound))
            {
                return Err(PeerError::Protocol(
                    "subscriber authorization changed".into(),
                ));
            }
        } else {
            return Ok(Some(Envelope::NotAuthorised {
                workspace_id,
                reason: "device not in workspace membership".into(),
            }));
        }
    }
    let expected = wr
        .subscribers
        .get(&peer.device_public_key)
        .map(|sub| sub.tx.clone());
    drop(wr);
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
    enum SubscribeReplayCompletion {
        Ready(ReplayHandle),
        Superseded,
    }

    let rx = owner
        .submit_replay_resumable_wait(
            true,
            1,
            async { Ok(()) },
            move |state, _| match state.capture_replay(&capture)? {
                Step::More => Ok(Step::More),
                Step::Done(handle) => Ok(Step::Done(SubscribeReplayCompletion::Ready(handle))),
            },
            move |result| async move {
                let SubscribeReplayCompletion::Ready(handle) = result? else {
                    unreachable!("only replay completion can classify a subscriber as superseded")
                };
                let mut wr = crate::workspace_io::fresh_membership(&completion_workspace).await?;
                let current = wr.subscribers.get(&source.peer.device_public_key);
                let unchanged = match (expected.as_ref(), current) {
                    (Some(expected), Some(current)) => expected.same_channel(&current.tx),
                    (None, None) => true,
                    _ => false,
                };
                if !source.alive() || !source.cert(&source.peer) || !state_member(&wr, &source.peer)
                {
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
                wr.state_sessions.insert(
                    source.peer.device_public_key,
                    StateSession {
                        live,
                        peer: source.peer,
                        tx: state_tx,
                        replay: Some(session_catchup),
                    },
                );
                drop(wr);
                Ok(SubscribeReplayCompletion::Ready(handle))
            },
        )
        .await?;
    let handle = match crate::persistence::receipt(rx).await? {
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
    federation: Option<Arc<std::sync::atomic::AtomicBool>>,
    session_live: Option<Arc<std::sync::atomic::AtomicBool>>,
}
impl StateSource {
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
            federation: None,
            session_live: session.connection_live.cloned(),
        }
    }
    fn cert(&self, peer: &AuthenticatedPeer) -> bool {
        peer.cert
            .expires_at_ns
            .is_none_or(|expiry| expiry > now_ns())
            && self
                .revocations
                .read()
                .is_ok_and(|list| !list.is_revoked(&peer.device_public_key))
            && self
                .policy
                .read()
                .is_ok_and(|policy| policy.admits(&peer.cert, &self.own_user))
    }
    pub(crate) fn authorized(&self, runtime: &crate::workspace::WorkspaceRuntime) -> bool {
        self.alive()
            && self.cert(&self.peer)
            && state_member(runtime, &self.peer)
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
fn state_member(runtime: &crate::workspace::WorkspaceRuntime, peer: &AuthenticatedPeer) -> bool {
    current_member(runtime, peer)
        || (peer.role == crate::auth::PeerRole::PeerRelay
            && runtime.known_peer_relays.contains(&peer.device_public_key))
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
            postcard::take_from_bytes::<qos_peer_state::NeighbourMsg>(&body)
        && trailing.is_empty()
        && matches!(
            &msg,
            qos_peer_state::NeighbourMsg::PersonalContinuityCheckpoint {
                user_public_key, ..
            } if *user_public_key == source.peer.user_public_key
        )
        && qos_peer_state::transport::retention(&msg, false)
            == Some((channel, qos_peer_state::transport::Retention::Checkpoint))
    {
        return None;
    }
    Some(channel)
}

impl std::ops::Deref for LiveEnvelope {
    type Target = Envelope;
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

struct StateCommit {
    continuity: Option<crate::store::continuity::Receipt>,
    envelope: Arc<Envelope>,
    pending: HashMap<[u8; 32], std::path::PathBuf>,
    persisted_bytes: u64,
    sessions: Vec<([u8; 32], mpsc::Sender<Envelope>)>,
    logical_offline_bytes: u64,
    publish: bool,
    activation: Option<std::path::PathBuf>,
    activation_error: Option<crate::store::StoreError>,
    replay_targets: Vec<ReplayPendingTarget>,
    accepted_deltas: u64,
}

struct PreparedRecipients {
    sessions: Vec<([u8; 32], mpsc::Sender<Envelope>)>,
    replay_targets: Vec<ReplayPendingTarget>,
    logical_offline_bytes: u64,
}

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
        generation: &std::path::Path,
        targets: Vec<ReplayPendingTarget>,
    ) -> Result<Self, crate::store::StoreError> {
        Ok(Self {
            continuity: None,
            publication: ReplayPendingPublication::Selected(
                crate::store::replay::PublicationCursor::open(generation)?,
            ),
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
                                batch.front().expect("one activation frame"),
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
            let frame = self.frame.as_mut().unwrap();
            let Some(target) = self.targets.get(frame.next_target) else {
                self.frame = None;
                continue;
            };
            frame.next_target += 1;
            if !target.active() {
                continue;
            }
            let sequence = match frame.sequence {
                Some(sequence) => sequence,
                None => {
                    let sequence = match &self.continuity {
                        Some(receipt) => receipt.sequence,
                        None => state.next_sequence()?,
                    };
                    frame.sequence = Some(sequence);
                    sequence
                }
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
    delta: Vec<u8>,
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
        let delta = delta.clone();
        let Some(crate::coedit::CoeditPayload::Checkpoint { channel, .. }) =
            crate::coedit::CoeditPayload::decode(&delta)
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
        let stage = if state.store.checkpoint_lineage_exists(&channel, &raw)? {
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
            delta,
            raw,
            channel,
            incoming,
            stage,
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
        let Some(stage) = input.stage.as_mut() else {
            batch.all_projectable = false;
            batch.stage_index += 1;
            return Ok(Step::More);
        };
        let result = stage.step(Default::default());
        if !input.cleanup_registered
            && let Some(paths) = stage.cleanup_paths()
        {
            state.retention_candidates.push(paths);
            input.cleanup_registered = true;
        }
        match result? {
            Step::More => return Ok(Step::More),
            Step::Done(eligible) => {
                batch.all_projectable &= eligible;
                batch.stage_index += 1;
                return Ok(Step::More);
            }
        }
    }
    if batch.selector.is_none() && batch.all_projectable {
        let overrides = batch
            .inputs
            .iter()
            .map(|input| {
                (
                    input.stage.as_ref().unwrap().lineage_name().to_os_string(),
                    input
                        .stage
                        .as_ref()
                        .unwrap()
                        .projected_generation()
                        .to_path_buf(),
                )
            })
            .collect();
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
    match selector.step_projection(Default::default())? {
        Step::More => Ok(Step::More),
        Step::Done(projected) => {
            batch.projected = Some(projected);
            Ok(Step::Done(()))
        }
    }
}

fn union_sessions<'a>(
    prepared: impl IntoIterator<Item = &'a PreparedRecipients>,
) -> (
    Vec<([u8; 32], mpsc::Sender<Envelope>)>,
    Vec<ReplayPendingTarget>,
) {
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

fn commit_log_and_materialize(
    state: &mut crate::persistence::State,
    input: &CheckpointBatchInput,
) -> Result<u64, crate::store::StoreError> {
    let sequence = input
        .sequence
        .ok_or_else(|| crate::persistence::error("checkpoint sequence was not reserved"))?;
    let Envelope::Delta {
        author_device_public_key,
        ..
    } = &*input.envelope
    else {
        return Err(crate::persistence::error(
            "checkpoint input was not a delta",
        ));
    };
    let _ = state.store.append_log(&crate::store::LogEntry {
        workspace_id: state.store.workspace_id(),
        received_at_ns: sequence,
        author_device_hex: hex::encode(author_device_public_key),
        authored_at_ns: sequence,
        delta_len: input.delta.len() as u64,
    });
    state.materialize(&input.delta);
    Ok(input.stage.as_ref().map_or(0, |stage| stage.written()))
}

fn state_commit_for(
    input: &mut CheckpointBatchInput,
    publication: crate::store::shell_selection::Publication,
) -> Result<StateCommit, crate::store::StoreError> {
    let prepared = input
        .prepared
        .take()
        .ok_or_else(|| crate::persistence::error("checkpoint recipients were not prepared"))?;
    let (publish, activation) = match publication {
        crate::store::shell_selection::Publication::Suppress => (false, None),
        crate::store::shell_selection::Publication::Activate(path) => (true, Some(path)),
        crate::store::shell_selection::Publication::Original => (true, None),
    };
    Ok(StateCommit {
        continuity: input.continuity.clone(),
        envelope: input.envelope.clone(),
        pending: HashMap::new(),
        persisted_bytes: input.stage.as_ref().map_or(0, |stage| stage.written()),
        sessions: prepared.sessions,
        logical_offline_bytes: prepared.logical_offline_bytes,
        publish,
        activation,
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
static CHECKPOINT_BATCH_MODE_OBSERVERS: std::sync::OnceLock<
    std::sync::Mutex<HashMap<u64, Arc<std::sync::Mutex<Vec<CheckpointBatchMode>>>>>,
> = std::sync::OnceLock::new();

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
        entry.0 += 1;
        entry.1 += self.started.elapsed().as_micros();
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
            |selector| selector.diagnostic_phase(),
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
    Commit(StateCommit),
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
                activating: true,
                sessions,
                replay_targets,
            }
        });
        Self {
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

    fn step(
        &mut self,
        state: &mut crate::persistence::State,
        batch: &mut CheckpointBatch,
    ) -> Result<ProjectedCheckpointStep, crate::store::StoreError> {
        if !self.selected_committed {
            match batch
                .selector
                .as_mut()
                .expect("projected checkpoint selector")
                .step(Default::default())?
            {
                Step::More => return Ok(ProjectedCheckpointStep::More),
                Step::Done(crate::store::shell_selection::SelectionOutcome::BatchPublished {
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
                    if publication != expected.publication || dispositions != expected.dispositions
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
                .unwrap()
                .finish_gc(Default::default())?
            {
                Step::More => return Ok(ProjectedCheckpointStep::More),
                Step::Done(()) => {
                    commit_log_and_materialize(state, input)?;
                    self.gc_index += 1;
                    return Ok(ProjectedCheckpointStep::More);
                }
            }
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
                let commit = state_commit_for(
                    &mut batch.inputs[self.publication_index],
                    crate::store::shell_selection::Publication::Original,
                )?;
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
                        self.commit.as_mut().unwrap().persisted_bytes = self
                            .commit
                            .as_ref()
                            .unwrap()
                            .persisted_bytes
                            .saturating_add(written);
                        self.replay = None;
                    }
                }
            }
            self.publication_index += 1;
            return Ok(ProjectedCheckpointStep::Commit(self.commit.take().unwrap()));
        }
        if self.emission_done {
            return Ok(ProjectedCheckpointStep::Done);
        }
        if self.commit.is_none() {
            let prepared = batch
                .inputs
                .iter_mut()
                .map(|input| input.prepared.take().expect("prepared checkpoint input"))
                .collect::<Vec<_>>();
            let (sessions, replay_targets) = union_sessions(&prepared);
            let (publish, activation) = match self
                .publication
                .take()
                .expect("represented checkpoint publication")
            {
                crate::store::shell_selection::Publication::Suppress => (false, None),
                crate::store::shell_selection::Publication::Activate(path) => (true, Some(path)),
                crate::store::shell_selection::Publication::Original => {
                    return Err(crate::persistence::error(
                        "represented checkpoint batch returned original publication",
                    ));
                }
            };
            let commit = StateCommit {
                continuity: None,
                envelope: batch.inputs.last().unwrap().envelope.clone(),
                pending: HashMap::new(),
                persisted_bytes: batch
                    .inputs
                    .iter()
                    .map(|input| input.stage.as_ref().unwrap().written())
                    .sum(),
                sessions,
                logical_offline_bytes: prepared.iter().map(|item| item.logical_offline_bytes).sum(),
                publish,
                activation,
                activation_error: None,
                replay_targets,
                accepted_deltas: batch.inputs.len() as u64,
            };
            self.commit = Some(commit);
            let commit = self.commit.as_ref().unwrap();
            self.replay = if let Some(path) = &commit.activation {
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
                    let commit = self.commit.as_mut().unwrap();
                    commit.persisted_bytes = commit.persisted_bytes.saturating_add(written);
                    self.replay = None;
                }
            }
        }
        self.emission_done = true;
        Ok(ProjectedCheckpointStep::Commit(self.commit.take().unwrap()))
    }

    fn error_fence(&self, batch: &CheckpointBatch) -> Option<CheckpointCommitFence> {
        if self.mode == CheckpointBatchMode::AllOriginal
            && (self.selected_committed
                || batch
                    .selector
                    .as_ref()
                    .is_some_and(|selector| selector.current_switch_attempted()))
        {
            return self.full_fence.clone();
        }
        if let Some(commit) = &self.commit {
            return Some(CheckpointCommitFence::capture(commit));
        }
        let activating = batch
            .selector
            .as_ref()
            .is_some_and(|selector| selector.activation_on_error().is_some());
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
    let input = &mut batch.inputs[index];
    input.stage.as_mut().unwrap().promote()
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
        let (retain, selection) = if batch.inputs[index].stage.is_some() {
            promote_checkpoint_input(state, batch, index)?;
            let selector = crate::store::shell_selection::Selection::new(
                &state.store,
                batch.root.join(format!("serial-{index:02}")),
                Some(batch.inputs[index].incoming),
            )?;
            state.retention_candidates.push(selector.cleanup_paths());
            (None, Some(selector))
        } else {
            (
                Some(
                    state
                        .store
                        .start_retain_in(
                            &batch.inputs[index].channel,
                            None,
                            batch.inputs[index].raw.clone(),
                            batch.root.join(format!("serial-retain-{index:02}")),
                        )?
                        .with_continuity(batch.inputs[index].continuity.clone()),
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

    fn step(
        &mut self,
        state: &mut crate::persistence::State,
        batch: &mut CheckpointBatch,
    ) -> Result<Step<StateCommit>, crate::store::StoreError> {
        if let Some(retain) = &mut self.retain {
            match retain.step(Default::default())? {
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
            match selection.step(Default::default())? {
                Step::More => return Ok(Step::More),
                Step::Done(crate::store::shell_selection::SelectionOutcome::Published(
                    publication,
                )) => {
                    if matches!(
                        publication,
                        crate::store::shell_selection::Publication::Activate(_)
                    ) {
                        let prepared = batch.inputs[self.index]
                            .prepared
                            .as_ref()
                            .expect("prepared checkpoint input");
                        self.post_cut_fence = Some(CheckpointCommitFence {
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
            let input = &mut batch.inputs[self.index];
            if let Some(stage) = input.stage.as_mut() {
                if matches!(stage.finish_gc(Default::default())?, Step::More) {
                    return Ok(Step::More);
                }
            }
            commit_log_and_materialize(state, input)?;
            let mut commit = state_commit_for(
                input,
                self.publication
                    .take()
                    .expect("serial checkpoint publication"),
            )?;
            commit.persisted_bytes = commit.persisted_bytes.max(self.retained_written);
            self.commit = Some(commit);
            let commit = self.commit.as_ref().unwrap();
            self.replay = if let Some(path) = &commit.activation {
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
                    self.commit.as_mut().unwrap().persisted_bytes = self
                        .commit
                        .as_ref()
                        .unwrap()
                        .persisted_bytes
                        .saturating_add(written);
                    self.replay = None;
                }
            }
        }
        Ok(Step::Done(self.commit.take().unwrap()))
    }

    fn error_fence(&self, batch: &CheckpointBatch) -> Option<CheckpointCommitFence> {
        if let Some(commit) = &self.commit {
            return Some(CheckpointCommitFence::capture(commit));
        }
        if let Some(fence) = &self.post_cut_fence {
            return Some(fence.clone());
        }
        let activating = self
            .selection
            .as_ref()
            .is_some_and(|selection| selection.activation_on_error().is_some())
            || self
                .retain
                .as_ref()
                .is_some_and(|retain| retain.activation_on_error().is_some());
        if !activating {
            return None;
        }
        let prepared = batch.inputs[self.index].prepared.as_ref()?;
        Some(CheckpointCommitFence {
            activating: true,
            sessions: prepared.sessions.clone(),
            replay_targets: prepared.replay_targets.clone(),
        })
    }
}

async fn publish_checkpoint_commit(
    workspace: &SharedWorkspace,
    source: &StateSource,
    workers: Arc<tokio::sync::Semaphore>,
    metrics: &crate::metrics::Metrics,
    mut commit: StateCommit,
) -> Result<(), crate::store::StoreError> {
    if !commit.publish {
        return Ok(());
    }
    for _ in 0..commit.accepted_deltas {
        metrics.inc_deltas_received();
    }
    let activating = commit.activation.is_some();
    let mut cursor = if let Some(path) = commit.activation.take() {
        Some(
            crate::persistence::blocking(workers.clone(), move || {
                crate::store::replay::PublicationCursor::open(&path)
            })
            .await??,
        )
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
            let Some(sub) = runtime.subscribers.get(recipient) else {
                continue;
            };
            if !sub.tx.same_channel(original) {
                continue;
            }
            let Some(target) = runtime.state_sessions.get(recipient) else {
                continue;
            };
            if !state_member(&runtime, &target.peer) || !source.cert(&target.peer) {
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
                    .unwrap()
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
    activating: bool,
    sessions: Vec<([u8; 32], mpsc::Sender<Envelope>)>,
    replay_targets: Vec<ReplayPendingTarget>,
}

impl CheckpointCommitFence {
    fn capture(commit: &StateCommit) -> Self {
        Self {
            activating: commit.activation.is_some(),
            sessions: commit.sessions.clone(),
            replay_targets: commit.replay_targets.clone(),
        }
    }

    async fn apply(self, workspace: &SharedWorkspace) {
        let mut runtime = workspace.lock().await;
        if self.activating {
            for (recipient, original) in &self.sessions {
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

async fn enqueue_checkpoint_prefix(
    workspace: &SharedWorkspace,
    envelopes: Vec<Envelope>,
    source: StateSource,
    role: crate::auth::PeerRole,
    metrics: Arc<crate::metrics::Metrics>,
    inbound_budget: Vec<Option<Arc<tokio::sync::OwnedSemaphorePermit>>>,
) -> Result<(), PeerError> {
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
                    let (next, result) = crate::persistence::blocking(workers.clone(), move || {
                        let mut state = staging_state
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner);
                        let result = step_checkpoint_batch(&mut state, &mut batch);
                        (batch, result)
                    })
                    .await?;
                    batch = next;
                    if matches!(result?, Step::Done(())) {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                let projection_elapsed_ms =
                    projection_started.map_or(0, |started| started.elapsed().as_millis());
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
                    let input = &batch.inputs[index];
                    let Envelope::Delta {
                        author_device_public_key,
                        ..
                    } = &*input.envelope
                    else {
                        unreachable!()
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
                            input.delta.len(),
                        )
                        .await?
                    };
                    batch.inputs[index].prepared = Some(prepared);
                    if mode != CheckpointBatchMode::Serial {
                        let operation_state = state.clone();
                        let (next, result) =
                            crate::persistence::blocking(workers.clone(), move || {
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
                            })
                            .await?;
                        batch = next;
                        result?;
                    } else {
                        let operation_state = state.clone();
                        let (next, result) =
                            crate::persistence::blocking(workers.clone(), move || {
                                let mut state = operation_state
                                    .lock()
                                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                                let result =
                                    SerialCheckpointCommit::new(&mut state, &mut batch, index);
                                (batch, result)
                            })
                            .await?;
                        batch = next;
                        let mut work = result?;
                        loop {
                            let operation_state = state.clone();
                            let (next_batch, next_work, result) =
                                crate::persistence::blocking(workers.clone(), move || {
                                    let mut state = operation_state
                                        .lock()
                                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                                    let result = work.step(&mut state, &mut batch);
                                    (batch, work, result)
                                })
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
                    }
                }
                if mode != CheckpointBatchMode::Serial {
                    let mut work = ProjectedCheckpointCommit::new(&batch, mode);
                    loop {
                        // Every AllOriginal worker quantum runs after all N
                        // promotions. A worker acquisition/join error cannot
                        // prove whether the selected CURRENT switched, so the
                        // immutable exact-session fence is conservative here.
                        let worker_fence = work.full_fence.clone();
                        let operation_state = state.clone();
                        let blocking = crate::persistence::blocking(workers.clone(), move || {
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
                        })
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
}
async fn enqueue_state_inner(
    workspace: &SharedWorkspace,
    envelope: Envelope,
    source: Option<StateSource>,
    role: crate::auth::PeerRole,
    metrics: Arc<crate::metrics::Metrics>,
    sync: bool,
) -> Result<(), PeerError> {
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
    let runtime = crate::workspace_io::fresh_membership(workspace).await?;
    if source
        .as_ref()
        .is_some_and(|source| !source.authorized(&runtime))
    {
        return Err(PeerError::Protocol(
            "state source authorization changed".into(),
        ));
    }
    drop(runtime);
    let owner = crate::persistence::handle(workspace).await;
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
    owner.submit_resumable_wait(
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
                replay_targets,
                logical_offline_bytes,
            ))
        },
        move |state, prepared| {
            if commit.is_none() {
                let (recipients, sessions, replay_targets, logical_offline_bytes) =
                    prepared.take().expect("ordered recipients");
                let envelope = envelope.take().expect("admitted envelope");
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
                        logical_offline_bytes,
                        publish,
                        activation: None,
                        activation_error: None,
                        replay_targets,
                        accepted_deltas: 1,
                    }));
                }
                let seq = state.next_sequence()?;
                sequence = Some(seq);
                let continuity = continuity_evidence.clone().map(|evidence| evidence.accept(seq));
                let raw = crate::transport::encode(&envelope)
                    .map_err(|e| crate::persistence::error(e.to_string()))?;
                let retention =
                    crate::coedit::CoeditPayload::decode(delta).map(|payload| match payload {
                        crate::coedit::CoeditPayload::Checkpoint { channel, .. } => (channel, None),
                        crate::coedit::CoeditPayload::RetainedDelta { channel, .. } => {
                            (channel, Some(seq))
                        }
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
                    logical_offline_bytes,
                    publish,
                    activation: None,
                    activation_error: None,
                    replay_targets,
                    accepted_deltas: 1,
                });
                return Ok(Step::More);
            }
            let current = commit.as_mut().unwrap();
            if let Some(build) = retained_build.as_mut() {
                let result = match build.step(Default::default()) {
                    Ok(result) => result,
                    Err(error) => {
                        let Some(activation) = build.activation_on_error() else { return Err(error); };
                        // Keep the captured sessions and uncertain/committed cut
                        // across the worker error so completion fences them.
                        current.activation = Some(activation);
                        current.activation_error = Some(error);
                        return Ok(Step::Done(commit.take().unwrap()));
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
                            crate::store::shell_selection::Publication::Activate(path) => {
                                current.activation = Some(path.clone());
                                if !current.replay_targets.is_empty() {
                                    match ReplayPendingWork::selected(
                                        &path,
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
                        .enqueue_pending(&recipient, sequence.unwrap(), raw)?;
                    current.pending.insert(recipient, path);
                    persist_metrics.add_bytes_queued(raw.len() as u64);
                    current.persisted_bytes =
                        current.persisted_bytes.saturating_add(raw.len() as u64);
                }
                if pending_work.is_some() {
                    return Ok(Step::More);
                }
            }
            let seq = sequence.unwrap();
            let Envelope::Delta { delta, .. } = &*current.envelope else {
                unreachable!()
            };
            let _ = state.store.append_log(&crate::store::LogEntry {
                workspace_id: state.store.workspace_id(),
                received_at_ns: seq,
                author_device_hex: hex::encode(author),
                authored_at_ns: seq,
                delta_len: delta.len() as u64,
            });
            state.materialize(delta);
            Ok(Step::Done(commit.take().unwrap()))
        },
        move |result| async move {
            let result = async {
                let mut commit = result?;
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
                let activating = commit.activation.is_some();
                let replay_targets = commit.replay_targets.clone();
                let mut delivered = 0u64;
                let publication_result: Result<(), crate::store::StoreError> = async {
                if let Some(error) = commit.activation_error.take() { return Err(error); }
                let mut cursor = if let Some(path) = commit.activation.take() {
                    Some(crate::persistence::blocking(workers.clone(), move || crate::store::replay::PublicationCursor::open(&path)).await??)
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
                    for (recipient, original) in &commit.sessions {
                        let Some(sub) = runtime.subscribers.get(recipient) else { continue };
                        if !sub.tx.same_channel(original) { continue; }
                        let Some(target) = runtime.state_sessions.get(recipient) else { continue };
                        if !state_member(&runtime, &target.peer) || source.as_ref().is_some_and(|s| !s.cert(&target.peer)) { continue; }
                        let replay_phase = target.replay.as_ref().map(|replay| {
                            replay.phase.load(std::sync::atomic::Ordering::SeqCst)
                        });
                        if replay_phase == Some(ReplayCatchup::ACTIVE) {
                            // This commit is already durable. The replay owner
                            // will take one final coherent cut before opening
                            // this exact session to post-Ready live delivery.
                            target.replay.as_ref().unwrap()
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
                                }
                            }
                            Err(mpsc::error::TrySendError::Closed(_)) => {
                                let _ = sub.disconnect.try_send(());
                                runtime.remove_subscriber_session(recipient, original);
                            }
                        }
                    }
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
    )
    .await?;
    Ok(())
}

async fn write_state_delivery<W: tokio::io::AsyncWrite + Unpin>(
    delivery: StateDelivery,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
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
    if !session_peer_authorized(peer, session)
        || !state_member(&runtime, peer)
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
    write
        .write_all(&crate::transport_v2::encode_state(
            &delivery.envelope,
            session.v2,
        )?)
        .await?;
    if let Some(path) = &delivery.cleanup {
        let path = path.clone();
        // Enqueue is never receipt. Exact cleanup is admitted only after the
        // complete frame write, and is ordered against commits/replay.
        let owner = crate::persistence::handle(&workspace).await;
        let runtime = crate::workspace_io::fresh_membership(&workspace).await?;
        if session_peer_authorized(peer, session)
            && state_member(&runtime, peer)
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
    let source = StateSource::new(peer, session);
    enqueue_state(&ws, env, Some(source), peer.role, session.metrics.clone()).await?;

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
    let source = StateSource::new(peer, session);
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

/// Handle an inbound `Presence`: fan it out to every other subscriber.
/// Ephemeral — no author-forgery check, no persistence.
async fn handle_presence(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    env: Envelope,
) -> Result<Option<Envelope>, PeerError> {
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
    let ws = {
        let map = session.workspaces.lock().await;
        map.get(&workspace_id).cloned()
    };
    if let Some(ws) = ws {
        let mut wr = ws.lock().await;
        // A device sends presence only to say hello, to ask for a baseline
        // (`SyncReq`) or after rejecting a checkpoint — each means it needs the
        // next live checkpoint (Stage 9 gate in `fanout_delta`).
        if peer.role == crate::auth::PeerRole::Device {
            wr.request_checkpoint(&peer.device_public_key);
        }
        wr.fanout_presence(&peer.device_public_key, peer.role, &env);
    }
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
    if !session_peer_authorized(peer, session) || !state_member(&wr, peer) {
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            reason: "device not in workspace membership".into(),
        }));
    }
    let expected = wr
        .subscribers
        .get(&peer.device_public_key)
        .map(|sub| sub.tx.clone());
    drop(wr);
    let owner = crate::persistence::handle(&ws).await;
    let source = StateSource::new(peer, session);
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
                    && source.cert(&source.peer)
                    && state_member(&wr, &source.peer)
                    && same
                {
                    if source
                        .tx
                        .try_send(Envelope::SyncResponse {
                            workspace_id,
                            delta_since: changes.clone(),
                        })
                        .is_err()
                    {
                        let _ = source.disconnect.try_send(());
                    }
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
        true,
        MAX_AUTOBOUND_WORKSPACES,
    )
    .await
    .map(Some)
}

#[cfg(test)]
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
                let unlocked = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    observer_registry.lock(),
                )
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
    source: AuthenticatedPeer,
    source_tx: mpsc::Sender<Envelope>,
    reply: mpsc::Sender<PrivateOutbound>,
    // Capture the originating session, never re-resolve by device key: that
    // key may already identify a newer replacement connection at delivery.
    source_disconnect: mpsc::Sender<()>,
}

fn current_cert(peer: &AuthenticatedPeer, session: &SessionCtx<'_>) -> bool {
    peer.role == crate::auth::PeerRole::Device && session_peer_authorized(peer, session)
}

fn session_peer_authorized(peer: &AuthenticatedPeer, session: &SessionCtx<'_>) -> bool {
    peer.cert
        .expires_at_ns
        .is_none_or(|expiry| expiry > now_ns())
        && session
            .revocations
            .read()
            .is_ok_and(|list| !list.is_revoked(&peer.device_public_key))
        && session
            .trust_policy
            .read()
            .is_ok_and(|policy| policy.admits(&peer.cert, session.own_user_public_key))
}

fn current_member(wr: &crate::workspace::WorkspaceRuntime, peer: &AuthenticatedPeer) -> bool {
    // Private authority is explicit and current. The broad auto-bind switch
    // allows a Subscribe to enrol an owner device, never bypasses this check.
    wr.membership.contains(&peer.device_public_key)
        || wr.admitted_users.contains(&peer.user_public_key)
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
// subscription on one of its two frozen shards; never infer membership from a
// numeric alias, and never choose between two simultaneously bound namespaces.
async fn private_workspace_binding(
    room: uuid::Uuid,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<(u64, SharedWorkspace)>, PeerError> {
    let ids = crate::room_workspace_ids(&room.to_string())
        .map_err(|_| PeerError::Protocol("invalid room".into()))?;
    let candidates = {
        let workspaces = session.workspaces.lock().await;
        ids.into_iter()
            .filter_map(|id| workspaces.get(&id).cloned().map(|ws| (id, ws)))
            .collect::<Vec<_>>()
    };
    let mut selected = None;
    for (id, workspace) in candidates {
        let runtime = workspace.lock().await;
        if runtime.store.workspace_id() != id || !current_session(&runtime, peer, session.outbound)
        {
            continue;
        }
        if selected.is_some() {
            return Err(PeerError::Protocol(
                "ambiguous room namespace binding".into(),
            ));
        }
        drop(runtime);
        selected = Some((id, workspace));
    }
    Ok(selected)
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
    if !current_cert(peer, session)
        || !current_member(&runtime, peer)
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
        EnvelopeV2::EnableActions { room } => {
            if room.is_nil() || !current_cert(peer, session) {
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
            if !current_cert(peer, session) {
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
            if host.is_nil() || !current_cert(peer, session) {
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
            timestamp_ns = now_ns()
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

    fn success(&mut self) -> Result<(), UnavailableReason> {
        self.phase("delivery_authorization_success");
        self.finished = true;
        Ok(())
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
    use UnavailableReason as U;
    private_phase(action, "route_enter");
    if action.source != peer.device_public_key || !current_cert(peer, session) {
        private_phase(action, "route_rejected_source");
        return Ok(Some(U::NotAuthorised));
    }
    if !action.validate() {
        private_phase(action, "route_rejected_message");
        return Ok(Some(U::InvalidRequest));
    }
    if crate::transport_v2::encode(&EnvelopeV2::Directed(action.clone())).is_err() {
        private_phase(action, "route_rejected_encoding");
        return Ok(Some(U::InvalidRequest));
    }
    let Some(ws) = private_workspace(action.room, peer, session).await? else {
        return Ok(Some(U::NotAuthorised));
    };
    #[cfg(test)]
    {
        let root = ws.lock().await.store.root().to_owned();
        crate::store::test_barrier::pause(&root, "private-authorization");
    }
    let wr = crate::workspace_io::fresh_membership(&ws).await?;
    private_phase(action, "route_lock_acquired");
    if !current_cert(peer, session)
        || !current_member(&wr, peer)
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
        return Ok(Some(U::Offline));
    };
    let Some(target) = wr.action_targets.get(&action.target) else {
        return Ok(Some(U::UnsupportedPeer));
    };
    if !current_cert(&target.peer, session)
        || !current_member(&wr, &target.peer)
        || target.room != action.room
        || !target.state_tx.same_channel(&sub.tx)
    {
        return Ok(Some(U::NotAuthorised));
    }
    // Legacy unbound invokes never acquire authority. Extended invocations
    // require the exact attached host identity advertised by the recipient.
    if matches!(action.message, ActionMessage::Invoke { .. })
        || matches!(&action.message, ActionMessage::InvokeBound { host, .. } if target.broker_host != Some(*host))
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
    private_phase(action, "route_authorized");
    let delivery = PrivateDelivery {
        action: action.clone(),
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
                EnvelopeV2::CertifiedDelivered {
                    action: delivery.action.clone(),
                    source_certificate: serde_json::to_vec(&delivery.source.cert)
                        .map_err(|_| PeerError::Protocol("certificate encoding failed".into()))?,
                }
            } else {
                EnvelopeV2::Delivered {
                    action: delivery.action.clone(),
                    source_user: delivery.source.user_public_key,
                }
            };
            let bytes = match crate::transport_v2::encode(&frame) {
                Ok(bytes) => bytes,
                Err(_) => {
                    private_phase(&delivery.action, "delivery_encoding_rejected");
                    let _ = delivery.source_disconnect.try_send(());
                    return Ok(());
                }
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
    if !current_cert(&delivery.source, session) {
        return trace.reject("delivery_rejected_source_cert_initial", U::NotAuthorised);
    }
    if !current_cert(recipient, session) {
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
    let wr = match crate::workspace_io::fresh_membership_for_delivery(
        &ws,
        workspace_id,
        delivery.action.request_id,
    )
    .await
    {
        Ok(wr) => wr,
        Err(_) => return trace.reject("delivery_rejected_membership_error", U::NotAuthorised),
    };
    if !current_cert(&delivery.source, session) {
        return trace.reject("delivery_rejected_source_cert_current", U::NotAuthorised);
    }
    if !current_cert(recipient, session) {
        return trace.reject("delivery_rejected_recipient_cert_current", U::NotAuthorised);
    }
    if !current_member(&wr, &delivery.source) {
        return trace.reject("delivery_rejected_source_member", U::NotAuthorised);
    }
    if !current_member(&wr, recipient) {
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
    drop(wr);
    trace.success()
}

#[cfg(test)]
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
            now_ns(),
        )
        .unwrap();
        let proof = crate::auth::build_hello_ack2(crate::auth::AuthDomain::Tilo, &key, &[1; 32]);
        crate::auth::verify_hello_ack2(
            crate::auth::AuthDomain::Tilo,
            &proof,
            &peer.device_public_key,
            &[1; 32],
        )
        .unwrap();
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
                qos_peer_state::retained_proof::RetentionAuthority::new(
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
            tx: governor_tx.clone(),
            disconnect: governor_disconnect,
            revocations: revocations.clone(),
            policy: Arc::new(std::sync::RwLock::new(
                crate::policy::TrustPolicy::AllowMatchingUser,
            )),
            own_user: user.public_key,
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
            revocations: revocations.clone(),
            policy: source.policy.clone(),
            own_user: user.public_key,
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
            shell_selection::{self, Selection},
            shell_selection_tests::{producer_frame, producer_metadata},
        };
        let meta = producer_metadata();
        let authority = qos_peer_state::retained_proof::RetentionAuthority::new(
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

    #[tokio::test]
    async fn thirty_four_route_prefix_uses_one_all_original_selection_then_converges() {
        use crate::store::{
            bounded::{Step, WorkLimit},
            shell_selection::{self, SelectedRead, Selection},
            shell_selection_tests::replay_frames,
        };
        use automerge::transaction::Transactable;

        fn frame(
            authority: &qos_peer_state::retained_proof::RetentionAuthority,
            message: &qos_peer_state::NeighbourMsg,
        ) -> Vec<u8> {
            let channel = qos_peer_state::transport::retention(message, true)
                .unwrap()
                .0;
            let body = postcard::to_allocvec(message).unwrap();
            let source = match message {
                qos_peer_state::NeighbourMsg::AppDocument { envelope, .. } => {
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
            device_label: "34 route production prefix".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut cert, &root_key).unwrap();
        let (observer, _observer_key) = certified_device(
            &root_key,
            root_key.verifying_key().to_bytes(),
            "34 route observer",
        );
        let (fenced_observer, _fenced_observer_key) = certified_device(
            &root_key,
            root_key.verifying_key().to_bytes(),
            "34 route fenced observer",
        );
        let now = web_time::SystemTime::UNIX_EPOCH + web_time::Duration::from_secs(4000);
        let room = "dddddddd-dddd-4ddd-8ddd-dddddddddddd";
        let mut publisher = qos_peer_state::StatePeer::new(
            room,
            cert.clone(),
            vec![],
            tilo_user_identity::RevocationList::new(),
            now,
        )
        .unwrap();
        let documents = (0..34)
            .map(|index| {
                let mut document = automerge::AutoCommit::new();
                document
                    .put(automerge::ROOT, "index", index as u64)
                    .unwrap();
                qos_peer_state::SeedDocument {
                    app_id: "threshold.app".into(),
                    record_id: format!("record-{index:02}"),
                    kind: "test".into(),
                    bytes: document.save(),
                }
            })
            .collect();
        publisher
            .install_seed(
                qos_peer_state::SeedState {
                    room_changes: qos_peer_state::seed_room(
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
            qos_peer_state::retained_proof::RetentionAuthority::new(room, cert.clone()).unwrap();
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
        assert_eq!(baseline.len(), 35);
        assert_eq!(next.len(), 35);
        assert_eq!(third.len(), 35);

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
            tokio::time::timeout(std::time::Duration::from_secs(2), observer_state_rx.recv())
                .await
                .unwrap()
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
                .take(32)
                .map(|raw| crate::transport::decode(&raw[4..]).unwrap())
                .collect(),
            source.clone(),
            crate::auth::PeerRole::Device,
            Arc::new(crate::metrics::Metrics::default()),
            vec![None; 32],
        )
        .await
        .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), reached)
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
        let delivered_before_failure =
            tokio::time::timeout(std::time::Duration::from_secs(2), observer_state_rx.recv())
                .await
                .expect("first exact Original was not visible before the injected failure")
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
                },
            );
        }
        drop(release);
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            observer_disconnected.recv(),
        )
        .await
        .expect("post-CURRENT failure did not fence the captured observer")
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
                },
            );
        }
        enqueue_checkpoint_prefix(
            &workspace,
            next.iter()
                .skip(1)
                .take(32)
                .map(|raw| crate::transport::decode(&raw[4..]).unwrap())
                .collect(),
            source.clone(),
            crate::auth::PeerRole::Device,
            Arc::new(crate::metrics::Metrics::default()),
            vec![None; 32],
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
        for (index, expected) in next.iter().skip(1).take(32).enumerate() {
            let observed =
                tokio::time::timeout(std::time::Duration::from_secs(2), reopened_state_rx.recv())
                    .await
                    .unwrap()
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
        for raw in baseline.iter().chain(next.iter().take(33)) {
            assert!(
                retained_after_prefix.contains(raw),
                "selected CURRENT omitted old basis or an exact Original prefix frame"
            );
        }

        enqueue_checkpoint_prefix(
            &workspace,
            next.iter()
                .skip(33)
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
        for (chunk, budgets) in [
            (third.iter().take(1).collect::<Vec<_>>(), vec![None]),
            (
                third.iter().skip(1).take(32).collect::<Vec<_>>(),
                vec![None; 32],
            ),
            (third.iter().skip(33).collect::<Vec<_>>(), vec![None; 2]),
        ] {
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
        use crate::store::shell_selection_tests::{producer_frame, producer_metadata};
        let meta = producer_metadata();
        let authority = qos_peer_state::retained_proof::RetentionAuthority::new(
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
            shell_selection::{self, Selection},
            shell_selection_tests::{producer_frame, producer_metadata},
        };
        let meta = producer_metadata();
        let authority = qos_peer_state::retained_proof::RetentionAuthority::new(
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
                metrics,
            },
            true,
            std::future::pending::<()>(),
        ));
        let (client_read, mut client_write) = tokio::io::split(client);
        let mut client_reader = SessionReader::new(client_read, true);
        client_write
            .write_all(
                &crate::transport_v2::encode_state(&Envelope::Subscribe { workspace_id: id }, true)
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
        let envelopes = meta["baseline"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| {
                let raw = producer_frame(&meta, row, 2);
                crate::transport::decode(&raw[4..]).unwrap()
            })
            .collect::<Vec<_>>();
        client_write
            .write_all(&crate::transport_v2::encode_state(&blocker, true).unwrap())
            .await
            .unwrap();
        for envelope in &envelopes {
            client_write
                .write_all(&crate::transport_v2::encode_state(envelope, true).unwrap())
                .await
                .unwrap();
        }
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while observed.load(std::sync::atomic::Ordering::SeqCst) < envelopes.len() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("reader did not retain the authenticated suffix");
        release_tx.send(()).unwrap();
        crate::persistence::receipt(held).await.unwrap();
        let progressed = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while selection_count.load(std::sync::atomic::Ordering::SeqCst) < 1 {
                tokio::task::yield_now().await;
            }
        })
        .await;
        if progressed.is_err() {
            let finished = task.is_finished();
            if finished {
                panic!(
                    "reader suffix failed after {} selections: {:?}",
                    selection_count.load(std::sync::atomic::Ordering::SeqCst),
                    task.await
                );
            }
            panic!(
                "reader suffix stalled after {} selections",
                selection_count.load(std::sync::atomic::Ordering::SeqCst)
            );
        }
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
        stop_tracking_reader_backlog(id);
        assert_eq!(shell_selection::selection_constructions(&store_root), 1);
        drain.abort();
        let _ = drain.await;
        task.abort();
        let _ = task.await;
    }

    async fn checkpoint_prefix_activation_failure_fences_original_sessions_and_retry(
        operation: &'static str,
        force_serial_finish_gc: bool,
    ) {
        use crate::store::{
            bounded::{Step, WorkLimit},
            shell_selection::Selection,
            shell_selection_tests::{producer_frame, producer_metadata},
        };
        let meta = producer_metadata();
        let authority = qos_peer_state::retained_proof::RetentionAuthority::new(
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
            } = &real_envelopes[real_envelopes.len() - 1]
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
        tokio::time::timeout(std::time::Duration::from_secs(10), reached)
            .await
            .expect("checkpoint prefix never reached its post-CURRENT fault")
            .unwrap();

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
                },
            );
        }
        drop(release);
        let owner = crate::persistence::handle(&workspace).await;
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();
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
        crate::persistence::receipt(owner.read(1, |_| Ok(())).unwrap())
            .await
            .unwrap();

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
        checkpoint_prefix_activation_failure_fences_original_sessions_and_retry(
            "shell-after-current",
            false,
        )
        .await;
    }

    #[tokio::test]
    async fn checkpoint_prefix_cursor_open_failure_fences_only_original_sessions() {
        checkpoint_prefix_activation_failure_fences_original_sessions_and_retry(
            "shell-publication-open",
            false,
        )
        .await;
    }

    #[tokio::test]
    async fn checkpoint_prefix_serial_gc_failure_fences_only_original_sessions() {
        checkpoint_prefix_activation_failure_fences_original_sessions_and_retry(
            "checkpoint-stage-finish-gc",
            true,
        )
        .await;
    }

    async fn activation_failure_fences_original_sessions_and_retry(operation: &'static str) {
        use crate::store::shell_selection_tests::{
            admit_frames, producer_fixture, producer_frame, producer_metadata, replay_frames,
        };
        let meta = producer_metadata();
        let authority = qos_peer_state::retained_proof::RetentionAuthority::new(
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
        tokio::time::timeout(std::time::Duration::from_secs(10), reached)
            .await
            .expect("selected activation never reached its fault barrier")
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
        use crate::store::shell_selection_tests::{
            admit_frames, producer_fixture, producer_frame, producer_metadata,
        };
        let meta = producer_metadata();
        let authority = qos_peer_state::retained_proof::RetentionAuthority::new(
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
        paused_state_private_roundtrip(true, false, false, false).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn paused_state_persistence_allows_same_connection_private_roundtrip() {
        paused_state_private_roundtrip(false, false, false, false).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn paused_replay_cleanup_with_full_reply_queue_keeps_private_reader_and_writer_live() {
        paused_state_private_roundtrip(false, true, false, false).await;
    }

    #[tokio::test]
    async fn full_control_backlog_backpressures_and_recovers_after_held_writer() {
        paused_state_private_roundtrip(false, true, true, false).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn overtaking_private_keeps_held_state_authorization_future_pollable() {
        paused_state_private_roundtrip(false, false, false, true).await;
    }

    async fn paused_state_private_roundtrip(
        check_lock: bool,
        held_cleanup: bool,
        full_backlog: bool,
        held_authorization: bool,
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
                metrics: Arc::new(crate::metrics::Metrics::default()),
            };
            tasks.push(tokio::spawn(run_peer_stream(
                peer.clone(),
                read,
                write,
                ctx,
                true,
                std::future::pending::<()>(),
            )));
            let (read, mut write) = tokio::io::split(client);
            let mut reader = SessionReader::new(read, true);
            write
                .write_all(
                    &crate::transport_v2::encode_state(
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
                    &crate::transport_v2::encode(&EnvelopeV2::EnableActions { room }).unwrap(),
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
                &crate::transport_v2::encode_state(&Envelope::Subscribe { workspace_id: id }, true)
                    .unwrap(),
            )
            .await
            .unwrap();
            assert_eq!(
                ar.read_next().await.unwrap(),
                Some(EnvelopeV2::State(pending))
            );
        } else {
            aw.write_all(&crate::transport_v2::encode_state(&envelope, true).unwrap())
                .await
                .unwrap();
        }
        reached.await.unwrap();
        if held_cleanup {
            for _ in 0..if full_backlog { 64 } else { 12 } {
                aw.write_all(&crate::transport_v2::encode_state(&Envelope::Ping, true).unwrap())
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
                &crate::transport_v2::encode(&EnvelopeV2::Directed(request.clone())).unwrap(),
            )
            .await
            .unwrap();
            let entered =
                tokio::time::timeout(std::time::Duration::from_secs(2), private_reached).await;
            drop(release);
            drop(private_release);
            let progress = tokio::time::timeout(std::time::Duration::from_secs(2), async {
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
                        other => panic!("expected state/private progress: {other:?}"),
                    }
                }
            })
            .await;
            for task in tasks {
                task.abort();
                let _ = task.await;
            }
            assert!(
                matches!(entered, Ok(Ok(()))),
                "private handler never entered lookahead"
            );
            assert!(
                progress.is_ok(),
                "overtaking private handler stopped polling the state future holding its authorization lane"
            );
            return;
        }
        let progress = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            if check_lock {
                drop(workspace.lock().await);
            }
            let encoded_request =
                crate::transport_v2::encode(&EnvelopeV2::Directed(request.clone())).unwrap();
            if !check_lock {
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
                    &crate::transport_v2::encode(&EnvelopeV2::Directed(probe.clone())).unwrap(),
                )
                .await
                .unwrap();
                let Some(EnvelopeV2::Delivered { action, .. }) = ar.read_next().await.unwrap()
                else {
                    panic!("expected private frame during partial inbound header")
                };
                assert_eq!(action, probe);
                aw.write_all(&encoded_request[2..]).await.unwrap();
            } else {
                aw.write_all(&encoded_request).await.unwrap();
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
                &crate::transport_v2::encode(&EnvelopeV2::Directed(response.clone())).unwrap(),
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
                    metrics: Arc::new(crate::metrics::Metrics::default()),
                },
                true,
                std::future::pending::<()>(),
            )));
            let (read, mut write) = tokio::io::split(client);
            let mut reader = SessionReader::new(read, true);
            for (position, room) in rooms.iter().enumerate() {
                write
                    .write_all(
                        &crate::transport_v2::encode_state(
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
                            &crate::transport_v2::encode(&EnvelopeV2::EnableActions {
                                room: *room,
                            })
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
                        &crate::transport_v2::encode(&EnvelopeV2::RegisterBroker {
                            room: rooms[0],
                            host,
                        })
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
        let (mut br, _bw) = wires.remove(0);
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
            &crate::transport_v2::encode_state(
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
                aw.write_all(&crate::transport_v2::encode_state(&Envelope::Ping, true).unwrap())
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
            aw.write_all(&crate::transport_v2::encode(&frame).unwrap())
                .await
                .unwrap();
        }
        let progress =
            tokio::time::timeout(std::time::Duration::from_secs(2), br.read_next()).await;
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
        let delivered = tokio::time::timeout(std::time::Duration::from_secs(2), br.read_next())
            .await
            .unwrap()
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
    async fn retained_replay_failure_and_legacy_duplicate_retirement_are_recoverable() {
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
        let opaque = b"unknown legacy bytes";
        runtime
            .store
            .enqueue_pending(&peer.device_public_key, 3, opaque)
            .unwrap();
        // Matching undecodable retained bytes do NOT constitute replay coverage.
        std::fs::write(
            runtime.store.root().join("retained/checkpoints/opaque"),
            opaque,
        )
        .unwrap();
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
            metrics: &metrics,
            trust_policy: &trust,
            disconnect: &disconnect,
            v2: false,
            private_tx: &private,
            revocations: &revocations,
        };
        let (mut broken, reader) = tokio::io::duplex(1);
        drop(reader);
        assert!(
            handle_inbound_frame(
                Envelope::Subscribe { workspace_id: 7 },
                &peer,
                &session,
                &mut Vec::new(),
                &mut broken
            )
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
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
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
        .await
        .expect("old owner did not release cancelled replay");
        // Emulate a crash during resumable legacy cleanup: write the complete
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
                .capture_replay_step(&mut capture, Default::default())
                .unwrap()
            {
                break snapshot;
            }
        };
        while matches!(
            store
                .prepare_replay_step(&mut snapshot, Default::default())
                .unwrap(),
            Step::More
        ) {}
        let budget = crate::store::replay::ReplayBudget::new(1024 * 1024);
        loop {
            let frames = store
                .read_replay_batch(&mut snapshot, Default::default(), &budget)
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
        handle_inbound_frame(
            Envelope::Subscribe { workspace_id: 7 },
            &peer,
            &session,
            &mut Vec::new(),
            &mut wire,
        )
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
            "unknown legacy entry was incorrectly retired"
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
            metrics: &metrics,
            trust_policy: &trust,
            disconnect: &disconnect,
            v2: true,
            private_tx: &ap,
            revocations: &revocations,
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
                let lock =
                    tokio::time::timeout(std::time::Duration::from_secs(2), observed.lock()).await;
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
            metrics: &metrics,
            trust_policy: &trust,
            disconnect: &disconnect,
            v2: true,
            private_tx: &ap,
            revocations: &revocations,
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
                })
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
            metrics: &metrics,
            trust_policy: &trust,
            disconnect: &source_disconnect,
            v2: true,
            private_tx: &source_private,
            revocations: &revocations,
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
                app_id: "dev.qos.test".into(),
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
        bounded_private_queue_for_room_namespace(false).await;
    }

    #[tokio::test]
    async fn legacy_room_binding_preserves_private_delivery_and_authorization_rechecks() {
        bounded_private_queue_for_room_namespace(true).await;
    }

    async fn bounded_private_queue_for_room_namespace(legacy: bool) {
        let (root, user) = UserIdentity::generate("alice", 1);
        let (a, a_key) = certified_device(&root, user.public_key, "a");
        let (b, _) = certified_device(&root, user.public_key, "b");
        let room = uuid::Uuid::from_u128(7);
        let id = if legacy {
            crate::legacy_room_workspace_id(&room.to_string())
        } else {
            crate::room_workspace_id(&room.to_string())
        }
        .unwrap();
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
            metrics: &metrics,
            trust_policy: &trust,
            disconnect: &disconnect,
            v2: true,
            private_tx: &ap,
            revocations: &revocations,
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
                std::iter::empty(),
            ),
            "private traffic must not overtake either namespace's state control"
        );
        let other = crate::room_workspace_ids(&room.to_string())
            .unwrap()
            .into_iter()
            .find(|candidate| *candidate != id)
            .unwrap();
        let other_root = tempfile::tempdir().unwrap();
        let mut other_runtime = crate::workspace::WorkspaceRuntime::open(
            other_root.path(),
            other,
            true,
            crate::workspace::Membership::default(),
        )
        .unwrap();
        other_runtime.admit_device(a.device_public_key).unwrap();
        other_runtime.add_subscriber(Subscriber {
            device_public_key: a.device_public_key,
            role: a.role,
            tx: atx.clone(),
            disconnect: disconnect.clone(),
        });
        registry
            .lock()
            .await
            .insert(other, Arc::new(AsyncMutex::new(other_runtime)));
        assert!(
            private_workspace(room, &a, &session).await.is_err(),
            "two admitted bindings must never select a namespace implicitly"
        );
        registry.lock().await.remove(&other);
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
        expiring.source.cert.expires_at_ns = Some(now_ns().saturating_add(100_000_000));
        sign_device_cert(&mut expiring.source.cert, &root).unwrap();
        expiring.source = crate::auth::accept_hello(
            &crate::auth::Hello {
                cert: expiring.source.cert.clone(),
                client_nonce: [9; 32],
            },
            &user.public_key,
            &crate::policy::TrustPolicy::AllowMatchingUser,
            &RevocationList::new(),
            now_ns(),
        )
        .unwrap();
        crate::auth::verify_hello_ack2(
            crate::auth::AuthDomain::Tilo,
            &crate::auth::build_hello_ack2(crate::auth::AuthDomain::Tilo, &a_key, &[8; 32]),
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
            now_ns(),
        )
        .unwrap();
        crate::auth::verify_hello_ack2(
            crate::auth::AuthDomain::Tilo,
            &crate::auth::build_hello_ack2(crate::auth::AuthDomain::Tilo, &a_key, &[6; 32]),
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
        // Wiring for the Stage 9 gate: after a device has been served one live
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
            metrics: &metrics,
            trust_policy: &trust,
            disconnect: &bdis,
            v2: false,
            private_tx: &ptx,
            revocations: &revocations,
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
            let frames = vec![retained(7, 1, 1, false, 3), barrier];
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

        let frames = vec![
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
            .map(|channel| retained(7, 1, channel as u8, true, 3))
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
        let current = match retained(7, 1, 1, true, 3) {
            EnvelopeV2::State(current) => current,
            _ => unreachable!(),
        };
        let suffix = vec![
            retained(7, 1, 2, true, 3),
            retained(7, 1, 2, true, 3),
            retained(7, 1, 3, false, 3),
            retained(7, 1, 4, true, 3),
        ];
        let suffix_before = suffix
            .iter()
            .map(|frame| crate::transport_v2::encode(frame).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(eligible_checkpoint_prefix(&current, suffix.iter()), 2);
        let suffix_after = suffix
            .iter()
            .map(|frame| crate::transport_v2::encode(frame).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(suffix_before, suffix_after);
    }

    #[test]
    fn checkpoint_batch_mode_requires_an_exact_completed_projection_shape() {
        use crate::store::shell_selection::{
            BatchProjectionReason, BatchSelectionOutcome, IncomingDisposition, Publication,
        };
        let represented = BatchSelectionOutcome {
            publication: Publication::Suppress,
            dispositions: vec![IncomingDisposition::Covered; 2],
            reason: BatchProjectionReason::Represented,
        };
        assert_eq!(
            checkpoint_mode_for(Some(&represented), 2),
            CheckpointBatchMode::Represented
        );
        let all_original = BatchSelectionOutcome {
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
                publication: Publication::Original,
                dispositions: vec![IncomingDisposition::Original],
                reason: BatchProjectionReason::FinalBasisOmission,
            },
            BatchSelectionOutcome {
                publication: Publication::Original,
                dispositions: vec![IncomingDisposition::Covered, IncomingDisposition::Original],
                reason: BatchProjectionReason::FinalBasisOmission,
            },
            BatchSelectionOutcome {
                publication: Publication::Activate(std::path::PathBuf::from("selected")),
                dispositions: vec![IncomingDisposition::Original; 2],
                reason: BatchProjectionReason::FinalBasisOmission,
            },
            BatchSelectionOutcome {
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
        let current = match retained(7, 1, 1, true, 3) {
            EnvelopeV2::State(current) => current,
            _ => unreachable!(),
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
        let current = match retained(7, 1, 1, true, 3) {
            EnvelopeV2::State(current) => current,
            _ => unreachable!(),
        };
        let full = (2..=34)
            .map(|channel| retained(7, 1, channel, true, 3))
            .collect::<Vec<_>>();
        assert_eq!(eligible_checkpoint_prefix(&current, full.iter()), 32);

        let retained_delta = match retained(7, 1, 2, false, 3) {
            EnvelopeV2::State(current) => current,
            _ => unreachable!(),
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

        let large = match retained(7, 1, 2, true, RETENTION_PREFIX_BYTE_CAP) {
            EnvelopeV2::State(current) => current,
            _ => unreachable!(),
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
