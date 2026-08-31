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
use crate::store::StoreError;
use crate::transport::{Envelope, FrameReader};
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
    disconnect: &'a mpsc::Sender<()>,
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
    mut write: W,
    ctx: PeerContext,
) -> Result<(), PeerError>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let PeerContext {
        workspaces,
        autobind,
        revocations,
        own_user_public_key,
        metrics,
    } = ctx;

    // Outbound channel — every workspace runtime this peer is
    // subscribed to writes into it; we drain to the wire.
    let (tx, mut rx) = mpsc::channel::<Envelope>(PEER_OUTBOUND_CAPACITY);
    let (disconnect_tx, mut disconnect_rx) = mpsc::channel::<()>(1);
    let mut reader = FrameReader::new(read);
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
        disconnect: &disconnect_tx,
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
                            let bytes = crate::transport::encode(&env)?;
                            write.write_all(&bytes).await?;
                        }
                        None => break,
                    }
                }
                frame = reader.read_next() => {
                    match frame {
                        Ok(Some(env)) => {
                            handle_inbound_frame(env, &peer, &session, &mut subscribed, &mut write).await?;
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
            ws.lock().await.remove_subscriber(&peer.device_public_key);
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
        let bytes = crate::transport::encode(&envelope)?;
        write.write_all(&bytes).await?;
    }
    for path in replay_cleanup {
        std::fs::remove_file(path)?;
    }
    if let Some(direct_reply) = direct_reply {
        let bytes = crate::transport::encode(&direct_reply)?;
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
    if !wr.membership.contains(&peer.device_public_key) && !is_federated {
        if wr.admit_authenticated {
            wr.admit_device(peer.device_public_key)?;
        } else {
            return Ok(Some(Envelope::NotAuthorised {
                workspace_id,
                reason: "device not in workspace membership".into(),
            }));
        }
    }
    let retained_count = load_retained_replay(&wr, workspace_id, replay)?;
    let pending_count = load_pending_replay(&wr, peer, workspace_id, replay, replay_cleanup)?;
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
) -> Result<u32, PeerError> {
    let retained = wr.store.load_retained()?;
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
    Ok(retained_count)
}

/// Load pending deltas before advertising Ready, but retain their
/// files until the connection task has written the complete replay.
/// A write failure therefore leaves the queue intact for reconnect.
fn load_pending_replay(
    wr: &crate::workspace::WorkspaceRuntime,
    peer: &AuthenticatedPeer,
    workspace_id: u64,
    replay: &mut Vec<Envelope>,
    replay_cleanup: &mut Vec<std::path::PathBuf>,
) -> Result<u32, PeerError> {
    let pending = wr.store.load_pending(&peer.device_public_key)?;
    let mut pending_count = 0u32;
    for entry in pending {
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
        ws.lock().await.remove_subscriber(&peer.device_public_key);
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
    if !wr.membership.contains(&peer.device_public_key) && !is_federated && !wr.admit_authenticated
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
async fn get_or_autobind_workspace(
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
