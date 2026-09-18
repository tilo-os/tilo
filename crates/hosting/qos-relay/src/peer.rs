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
use crate::store::StoreError;
use crate::transport::Envelope;
use crate::transport_v2::{ActionCapability, EnvelopeV2, SessionReader};
use crate::workspace::{SharedWorkspace, Subscriber};

/// Maximum live fan-out backlog per connected peer. Durable deltas move to
/// disk once this fills; ephemeral presence may be dropped with the session.
const PEER_OUTBOUND_CAPACITY: usize = 32;

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
}

/// Handle to the set of workspaces the daemon owns.
pub type WorkspaceRegistry = Arc<AsyncMutex<HashMap<u64, SharedWorkspace>>>;
const MAX_AUTOBOUND_WORKSPACES: usize = 4096;

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

/// Sequence-number source for enqueued deltas — a monotonic clock
/// so filenames sort in delivery order across restarts.
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
    pub revocations: Arc<std::sync::RwLock<qos_user_identity::RevocationList>>,
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
    own_user_public_key: &'a [u8; 32],
    metrics: &'a crate::metrics::Metrics,
    trust_policy: &'a Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    disconnect: &'a mpsc::Sender<()>,
    v2: bool,
    private_tx: &'a mpsc::Sender<PrivateOutbound>,
    revocations: &'a Arc<std::sync::RwLock<qos_user_identity::RevocationList>>,
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
    mut write: W,
    ctx: PeerContext,
    v2: bool,
) -> Result<(), PeerError>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
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
    let (disconnect_tx, mut disconnect_rx) = mpsc::channel::<()>(1);
    let mut reader = SessionReader::new(read, v2);
    let (private_tx, mut private_rx) = mpsc::channel(PEER_OUTBOUND_CAPACITY);
    // Track our subscriptions so we can unregister on disconnect.
    let mut subscribed: Vec<u64> = Vec::new();
    let mut access_audit = tokio::time::interval(std::time::Duration::from_secs(1));
    access_audit.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    access_audit.tick().await;

    let session = SessionCtx {
        workspaces: &workspaces,
        autobind: autobind.as_ref(),
        outbound: &tx,
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
    let outcome: Result<(), PeerError> = async {
        loop {
            tokio::select! {
                biased;
                _ = connection.closed() => break,
                _ = access_audit.tick() => {
                    audit_subscriber_access(&revocations, &peer, &session, &subscribed).await?;
                }
                _ = disconnect_rx.recv() => {
                    return Err(PeerError::Protocol("subscriber exceeded outbound backpressure limit".into()));
                }
                outbound = rx.recv() => {
                    match outbound {
                        Some(env) => {
                            let bytes = crate::transport_v2::encode_state(&env, v2)?;
                            write.write_all(&bytes).await?;
                        }
                        None => break,
                    }
                }
                private = private_rx.recv(), if v2 => {
                    if let Some(private) = private {
                        match private {
                            PrivateOutbound::Reply(reply) => write.write_all(&crate::transport_v2::encode(&reply)?).await?,
                            PrivateOutbound::Delivery(delivery) => {
                                write_private_delivery(&delivery, &peer, &session, &mut write).await?;
                            }
                        }
                    }
                }
                frame = reader.read_next() => {
                    match frame {
                        Ok(Some(env)) => {
                            match env {
                                EnvelopeV2::State(env) => handle_inbound_frame(env, &peer, &session, &mut subscribed, &mut write).await?,
                                frame => {
                                    if let Some(reply) = handle_private(frame, &peer, &session).await? {
                                        write.write_all(&crate::transport_v2::encode(&reply)?).await?;
                                    }
                                }
                            }
                        }
                        Ok(None) => break,
                        Err(e) => return Err(PeerError::Frame(e)),
                    }
                }
            }
        }
        Ok(())
    }
    .await;

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

/// Periodic membership + revocation re-check for every workspace this
/// peer is subscribed to. Reloads membership from disk each tick —
/// the only way a removal made while a device is connected takes
/// effect promptly — and ends the session if the device's own cert
/// was revoked or it fell out of membership.
async fn audit_subscriber_access(
    revocations: &Arc<std::sync::RwLock<qos_user_identity::RevocationList>>,
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
        let mut workspace = workspace.lock().await;
        let devices = workspace.store.load_membership()?;
        workspace.membership.set(devices);
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
    let mut replay = Vec::new();
    let mut replay_cleanup = Vec::new();
    // Named apart from `replay` on purpose — "reply" vs "replay" reads as a
    // typo at a glance even though they're deliberately distinct concepts.
    let direct_reply = handle_envelope(
        peer,
        env,
        session,
        subscribed,
        &mut replay,
        &mut replay_cleanup,
    )
    .await?;
    for envelope in replay {
        let bytes = crate::transport_v2::encode_state(&envelope, session.v2)?;
        write.write_all(&bytes).await?;
    }
    for path in replay_cleanup {
        std::fs::remove_file(path)?;
    }
    if let Some(direct_reply) = direct_reply {
        let bytes = crate::transport_v2::encode_state(&direct_reply, session.v2)?;
        write.write_all(&bytes).await?;
    }
    Ok(())
}

async fn handle_envelope(
    peer: &AuthenticatedPeer,
    env: Envelope,
    session: &SessionCtx<'_>,
    subscribed: &mut Vec<u64>,
    replay: &mut Vec<Envelope>,
    replay_cleanup: &mut Vec<std::path::PathBuf>,
) -> Result<Option<Envelope>, PeerError> {
    match env {
        Envelope::Subscribe { workspace_id } => {
            handle_subscribe(
                peer,
                session,
                subscribed,
                replay,
                replay_cleanup,
                workspace_id,
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
    replay: &mut Vec<Envelope>,
    replay_cleanup: &mut Vec<std::path::PathBuf>,
    workspace_id: u64,
) -> Result<Option<Envelope>, PeerError> {
    let ws = get_or_autobind_workspace(session.workspaces, session.autobind, workspace_id).await?;
    let Some(ws) = ws else {
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            reason: "workspace not bound on this relay".into(),
        }));
    };
    let mut wr = ws.lock().await;
    let devices = wr.store.load_membership()?;
    wr.membership.set(devices);
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
            wr.admit_device(peer.device_public_key)?;
        } else {
            return Ok(Some(Envelope::NotAuthorised {
                workspace_id,
                reason: "device not in workspace membership".into(),
            }));
        }
    }
    let (retained_count, retained) = load_retained_replay(&wr, workspace_id, replay)?;
    let pending_count =
        load_pending_replay(&wr, peer, workspace_id, &retained, replay, replay_cleanup)?;
    wr.add_subscriber(Subscriber {
        device_public_key: peer.device_public_key,
        role: peer.role,
        tx: session.outbound.clone(),
        disconnect: session.disconnect.clone(),
    });
    drop(wr);
    if !subscribed.contains(&workspace_id) {
        subscribed.push(workspace_id);
    }
    Ok(Some(Envelope::Ready {
        workspace_id,
        pending_deltas: pending_count.saturating_add(retained_count),
    }))
}

/// Replay retained document baselines first. Unlike a per-device
/// pending queue these exist before a newly invited device has ever
/// connected, which is what makes an offline owner + new guest work.
fn load_retained_replay(
    wr: &crate::workspace::WorkspaceRuntime,
    workspace_id: u64,
    replay: &mut Vec<Envelope>,
) -> Result<(u32, std::collections::HashSet<Vec<u8>>), PeerError> {
    let retained = wr.store.load_retained()?;
    let retained_bytes = retained.iter().cloned().collect();
    let mut retained_count = 0u32;
    for raw in retained {
        let Some(payload) = raw.get(4..) else {
            tracing::warn!(
                workspace_id,
                "retained envelope is shorter than its frame prefix"
            );
            continue;
        };
        match crate::transport::decode(payload) {
            Ok(envelope) => {
                replay.push(envelope);
                retained_count = retained_count.saturating_add(1);
            }
            Err(error) => {
                tracing::warn!(workspace_id, %error, "retained envelope could not be decoded");
            }
        }
    }
    Ok((retained_count, retained_bytes))
}

/// Load pending deltas before advertising Ready, but retain their
/// files until the connection task has written the complete replay.
/// A write failure therefore leaves the queue intact for reconnect.
fn load_pending_replay(
    wr: &crate::workspace::WorkspaceRuntime,
    peer: &AuthenticatedPeer,
    workspace_id: u64,
    retained: &std::collections::HashSet<Vec<u8>>,
    replay: &mut Vec<Envelope>,
    replay_cleanup: &mut Vec<std::path::PathBuf>,
) -> Result<u32, PeerError> {
    let pending = wr.store.load_pending(&peer.device_public_key)?;
    let mut pending_count = 0u32;
    for entry in pending {
        // A retained document message may also have been queued while this
        // already-enrolled device was offline. Replay it once through the
        // retained baseline and remove the duplicate pending entry only after
        // that replay has been written successfully.
        if retained.contains(&entry.bytes) {
            replay_cleanup.push(entry.path);
            continue;
        }
        // Each pending entry is a full pre-encoded envelope — send it
        // verbatim.
        let Some(payload) = entry.bytes.get(4..) else {
            tracing::warn!(
                workspace_id,
                path = %entry.path.display(),
                "pending envelope is shorter than its frame prefix"
            );
            continue;
        };
        match crate::transport::decode(payload) {
            Ok(envelope) => {
                replay.push(envelope);
                replay_cleanup.push(entry.path);
                pending_count = pending_count.saturating_add(1);
            }
            Err(error) => {
                tracing::warn!(
                    workspace_id,
                    path = %entry.path.display(),
                    %error,
                    "pending envelope could not be decoded"
                );
            }
        }
    }
    Ok(pending_count)
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
async fn handle_delta(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    env: Envelope,
) -> Result<Option<Envelope>, PeerError> {
    let Envelope::Delta {
        workspace_id,
        author_device_public_key,
        delta,
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
    let mut wr = ws.lock().await;
    // Author check — the peer that sent the delta is the author; we
    // don't allow one peer to forge a delta as if it came from
    // another device. A plain device can only publish deltas signed
    // as itself; a peer relay legitimately forwards deltas authored
    // by any device it hosts, so the author field is not required to
    // match its own cert.
    if peer.role == crate::auth::PeerRole::Device
        && author_device_public_key != &peer.device_public_key
    {
        return Err(PeerError::Protocol(
            "author does not match authenticated device".into(),
        ));
    }
    let seq = now_ns();
    let raw_bytes = delta.clone();
    let offline_devices = wr
        .membership
        .devices
        .iter()
        .filter(|device| {
            *device != author_device_public_key && !wr.subscribers.contains_key(*device)
        })
        .count();
    let offline_relays = if peer.role == crate::auth::PeerRole::Device {
        wr.known_peer_relays
            .iter()
            .filter(|device| {
                *device != author_device_public_key && !wr.subscribers.contains_key(*device)
            })
            .count()
    } else {
        0
    };
    let delivered = wr.fanout_delta(author_device_public_key, peer.role, &env, &raw_bytes, seq)?;
    drop(wr);
    session.metrics.inc_deltas_received();
    session.metrics.add_deltas_fanned_out(delivered as u64);
    session.metrics.add_bytes_queued(
        (raw_bytes.len() as u64)
            .saturating_mul(offline_devices.saturating_add(offline_relays) as u64),
    );
    Ok(None)
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
        ws.lock()
            .await
            .fanout_presence(&peer.device_public_key, peer.role, &env);
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
    let mut wr = ws.lock().await;
    let is_federated = peer.role == crate::auth::PeerRole::PeerRelay
        && peer.user_public_key == *session.own_user_public_key
        && wr.known_peer_relays.contains(&peer.device_public_key);
    if !wr.membership.contains(&peer.device_public_key)
        && !wr.admitted_users.contains(&peer.user_public_key)
        && !is_federated
        && !wr.admit_authenticated
    {
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            reason: "device not in workspace membership".into(),
        }));
    }
    let delta_since = wr.changes_since(&have_heads);
    drop(wr);
    Ok(Some(Envelope::SyncResponse {
        workspace_id,
        delta_since,
    }))
}

#[allow(
    clippy::significant_drop_tightening,
    reason = "the registry lock is deliberately held across the synchronous \
              WorkspaceRuntime::open below — see the comment there — so two \
              devices racing to autobind the same workspace can't create two \
              independent runtimes"
)]
pub(crate) async fn get_or_autobind_workspace(
    workspaces: &WorkspaceRegistry,
    autobind: Option<&WorkspaceAutobind>,
    workspace_id: u64,
) -> Result<Option<SharedWorkspace>, StoreError> {
    let mut map = workspaces.lock().await;
    if let Some(workspace) = map.get(&workspace_id) {
        return Ok(Some(workspace.clone()));
    }
    let Some(autobind) = autobind else {
        return Ok(None);
    };
    if !autobind_has_capacity(map.len()) {
        return Err(StoreError::Io(std::io::Error::other(
            "home peer workspace limit reached",
        )));
    }

    // Holding the registry lock across the synchronous open makes first
    // subscribe atomic: two devices racing to introduce a room cannot create
    // two independent runtimes or lose the first admitted membership.
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        &autobind.state_root,
        workspace_id,
        true,
        crate::workspace::Membership::default(),
    )?;
    runtime.admit_authenticated = true;
    let workspace = Arc::new(AsyncMutex::new(runtime));
    map.insert(workspace_id, workspace.clone());
    tracing::info!(workspace_id, "auto-bound same-owner workspace");
    Ok(Some(workspace))
}

#[cfg(test)]
mod autobind_tests {
    use super::*;

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
    peer.role == crate::auth::PeerRole::Device
        && peer
            .cert
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
            let id = crate::room_workspace_id(&room.to_string())
                .map_err(|_| PeerError::Protocol("invalid room".into()))?;
            let Some(ws) = session.workspaces.lock().await.get(&id).cloned() else {
                return Err(PeerError::Protocol("private room not subscribed".into()));
            };
            let mut wr = ws.lock().await;
            let members = wr.store.load_membership()?;
            wr.membership.set(members);
            if !current_member(&wr, peer) || !current_session(&wr, peer, session.outbound) {
                return Err(PeerError::Protocol("private room not subscribed".into()));
            }
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
            Ok(Some(EnvelopeV2::ActionsReady {
                room,
                capability: ActionCapability::TransportOnly,
            }))
        }
        EnvelopeV2::EnableBrokerClient { room } => {
            if !current_cert(peer, session) {
                return Err(PeerError::Protocol("broker protocol not authorised".into()));
            }
            let id = crate::room_workspace_id(&room.to_string())
                .map_err(|_| PeerError::Protocol("invalid room".into()))?;
            let ws = session
                .workspaces
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or_else(|| PeerError::Protocol("room not subscribed".into()))?;
            let mut wr = ws.lock().await;
            let members = wr.store.load_membership()?;
            wr.membership.set(members);
            if !current_member(&wr, peer) || !current_session(&wr, peer, session.outbound) {
                return Err(PeerError::Protocol("broker protocol not authorised".into()));
            }
            let target = wr
                .action_targets
                .get_mut(&peer.device_public_key)
                .filter(|t| t.room == room && t.state_tx.same_channel(session.outbound))
                .ok_or_else(|| PeerError::Protocol("private lane not enabled".into()))?;
            target.broker_protocol = true;
            Ok(None)
        }
        EnvelopeV2::RegisterBroker { room, host } => {
            if host.is_nil() || !current_cert(peer, session) {
                return Err(PeerError::Protocol("broker not authorised".into()));
            }
            let id = crate::room_workspace_id(&room.to_string())
                .map_err(|_| PeerError::Protocol("invalid room".into()))?;
            let ws = session
                .workspaces
                .lock()
                .await
                .get(&id)
                .cloned()
                .ok_or_else(|| PeerError::Protocol("room not subscribed".into()))?;
            let mut wr = ws.lock().await;
            let members = wr.store.load_membership()?;
            wr.membership.set(members);
            if !current_member(&wr, peer) || !current_session(&wr, peer, session.outbound) {
                return Err(PeerError::Protocol("broker not authorised".into()));
            }
            let target = wr
                .action_targets
                .get_mut(&peer.device_public_key)
                .filter(|t| t.room == room && t.state_tx.same_channel(session.outbound))
                .ok_or_else(|| PeerError::Protocol("private lane not enabled".into()))?;
            target.broker_host = Some(host);
            target.broker_protocol = true;
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

async fn route_private(
    action: &DirectedAction,
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
) -> Result<Option<UnavailableReason>, PeerError> {
    use UnavailableReason as U;
    if action.source != peer.device_public_key || !current_cert(peer, session) {
        return Ok(Some(U::NotAuthorised));
    }
    if !action.validate()
        || crate::transport_v2::encode(&EnvelopeV2::Directed(action.clone())).is_err()
    {
        return Ok(Some(U::InvalidRequest));
    }
    let id = crate::room_workspace_id(&action.room.to_string())
        .map_err(|_| PeerError::Protocol("invalid room".into()))?;
    let Some(ws) = session.workspaces.lock().await.get(&id).cloned() else {
        return Ok(Some(U::NotAuthorised));
    };
    let mut wr = ws.lock().await;
    let members = wr.store.load_membership()?;
    wr.membership.set(members);
    if !current_member(&wr, peer)
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
            | ActionMessage::BrokerCatalogue { .. }
            | ActionMessage::Observation { .. }
            | ActionMessage::Rejected { .. }
    );
    if (target.broker_host.is_some() && !source_extended)
        || (extension && (!source_extended || !target.broker_protocol))
    {
        return Ok(Some(U::UnsupportedPeer));
    }
    let delivery = PrivateDelivery {
        action: action.clone(),
        source: peer.clone(),
        source_tx: session.outbound.clone(),
        reply: session.private_tx.clone(),
        source_disconnect: session.disconnect.clone(),
        certified: target.broker_host.is_some(),
    };
    Ok(target
        .tx
        .try_send(PrivateOutbound::Delivery(delivery))
        .err()
        .map(|error| match error {
            mpsc::error::TrySendError::Full(_) => U::Backpressure,
            mpsc::error::TrySendError::Closed(_) => U::Offline,
        }))
}

async fn write_private_delivery<W: tokio::io::AsyncWrite + Unpin>(
    delivery: &PrivateDelivery,
    recipient: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    write: &mut W,
) -> Result<(), PeerError> {
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
                    let _ = delivery.source_disconnect.try_send(());
                    return Ok(());
                }
            };
            write.write_all(&bytes).await?
        }
        Err(reason) => {
            if delivery
                .reply
                .try_send(PrivateOutbound::Reply(EnvelopeV2::unavailable(
                    &delivery.action,
                    reason,
                )))
                .is_err()
            {
                // A bounded failure queue cannot silently erase the outcome.
                // Its originating session must observe transport loss instead.
                let _ = delivery.source_disconnect.try_send(());
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
    if !current_cert(&delivery.source, session) || !current_cert(recipient, session) {
        return Err(U::NotAuthorised);
    }
    let id = crate::room_workspace_id(&delivery.action.room.to_string())
        .map_err(|_| U::InvalidRequest)?;
    let ws = session
        .workspaces
        .lock()
        .await
        .get(&id)
        .cloned()
        .ok_or(U::NotAuthorised)?;
    let mut wr = ws.lock().await;
    let members = wr.store.load_membership().map_err(|_| U::NotAuthorised)?;
    wr.membership.set(members);
    if !current_member(&wr, &delivery.source)
        || !current_member(&wr, recipient)
        || delivery.action.target != recipient.device_public_key
        || !current_session(&wr, &delivery.source, &delivery.source_tx)
        || !wr
            .action_targets
            .get(&delivery.source.device_public_key)
            .is_some_and(|target| {
                target.room == delivery.action.room
                    && target.state_tx.same_channel(&delivery.source_tx)
            })
        || !current_session(&wr, recipient, session.outbound)
        || !wr
            .action_targets
            .get(&recipient.device_public_key)
            .is_some_and(|target| {
                target.room == delivery.action.room
                    && target.state_tx.same_channel(session.outbound)
            })
    {
        return Err(U::NotAuthorised);
    }
    Ok(())
}

#[cfg(test)]
mod private_transport_tests {
    use super::*;
    use qos_user_identity::{
        DeviceCapabilities, DeviceCert, DeviceIdentity, RevocationList, UserIdentity,
        sign_device_cert,
    };

    fn certified_device(
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
        let proof = crate::auth::build_hello_ack2(&key, &[1; 32]);
        crate::auth::verify_hello_ack2(&proof, &peer.device_public_key, &[1; 32]).unwrap();
        (peer, key)
    }

    #[tokio::test]
    async fn bounded_private_queue_and_delivery_recheck_never_retain_traffic() {
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
        let metrics = crate::metrics::Metrics::default();
        let session = SessionCtx {
            workspaces: &registry,
            autobind: None,
            outbound: &atx,
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
            message: ActionMessage::CatalogueRefresh,
        };
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
        let mut entry = qos_user_identity::RevocationEntry {
            user_public_key: user.public_key,
            revoked_device_public_key: a.device_public_key,
            revoked_at_ns: 1,
            reason: qos_user_identity::RevocationReason::Lost,
            signature: [0; 64],
        };
        qos_user_identity::sign_revocation_entry(&mut entry, &root).unwrap();
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
            now_ns(),
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
}
