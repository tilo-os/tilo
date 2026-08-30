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
        .map_or(0, |d| d.as_nanos() as u64)
}

/// Run one peer connection until the wire closes.
///
/// `read` / `write` are the halves of the bidirectional stream
/// **after** the auth handshake completed.
pub async fn run_peer<R, W>(
    peer: AuthenticatedPeer,
    connection: iroh::endpoint::Connection,
    read: R,
    mut write: W,
    workspaces: WorkspaceRegistry,
    autobind: Option<WorkspaceAutobind>,
    revocations: Arc<std::sync::RwLock<qos_user_identity::RevocationList>>,
    own_user_public_key: [u8; 32],
    metrics: Arc<crate::metrics::Metrics>,
) -> Result<(), PeerError>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
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
                    if revocations
                        .read()
                        .is_ok_and(|list| list.is_revoked(&peer.device_public_key))
                    {
                        return Err(PeerError::Protocol("authenticated device was revoked".into()));
                    }
                    for workspace_id in &subscribed {
                        let Some(workspace) = workspaces.lock().await.get(workspace_id).cloned() else {
                            continue;
                        };
                        let mut workspace = workspace.lock().await;
                        let devices = workspace.store.load_membership()?;
                        workspace.membership.set(devices);
                        let is_federated = peer.role == crate::auth::PeerRole::PeerRelay
                            && peer.user_public_key == own_user_public_key
                            && workspace.known_peer_relays.contains(&peer.device_public_key);
                        if !workspace.admit_authenticated
                            && !workspace.membership.contains(&peer.device_public_key)
                            && !is_federated
                        {
                            return Err(PeerError::Protocol(
                                "device was removed from workspace membership".into(),
                            ));
                        }
                    }
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
                            let mut replay = Vec::new();
                            let mut replay_cleanup = Vec::new();
                            if let Some(reply) = handle_envelope(
                                &peer,
                                env,
                                &workspaces,
                                autobind.as_ref(),
                                &tx,
                                &mut subscribed,
                                &own_user_public_key,
                                &metrics,
                                &mut replay,
                                &mut replay_cleanup,
                                &disconnect_tx,
                            ).await? {
                                for envelope in replay.drain(..) {
                                    let bytes = crate::transport::encode(&envelope)?;
                                    write.write_all(&bytes).await?;
                                }
                                for path in replay_cleanup.drain(..) {
                                    std::fs::remove_file(path)?;
                                }
                                let bytes = crate::transport::encode(&reply)?;
                                write.write_all(&bytes).await?;
                            } else {
                                for envelope in replay.drain(..) {
                                    let bytes = crate::transport::encode(&envelope)?;
                                    write.write_all(&bytes).await?;
                                }
                                for path in replay_cleanup.drain(..) {
                                    std::fs::remove_file(path)?;
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
            ws.lock().await.remove_subscriber(&peer.device_public_key);
        }
    }
    drop(wsmap);
    outcome
}

async fn handle_envelope(
    peer: &AuthenticatedPeer,
    env: Envelope,
    workspaces: &WorkspaceRegistry,
    autobind: Option<&WorkspaceAutobind>,
    outbound: &mpsc::Sender<Envelope>,
    subscribed: &mut Vec<u64>,
    own_user_public_key: &[u8; 32],
    metrics: &crate::metrics::Metrics,
    replay: &mut Vec<Envelope>,
    replay_cleanup: &mut Vec<std::path::PathBuf>,
    disconnect: &mpsc::Sender<()>,
) -> Result<Option<Envelope>, PeerError> {
    match env {
        Envelope::Subscribe { workspace_id } => {
            let ws = get_or_autobind_workspace(workspaces, autobind, workspace_id).await?;
            let Some(ws) = ws else {
                return Ok(Some(Envelope::NotAuthorised {
                    workspace_id,
                    reason: "workspace not bound on this relay".into(),
                }));
            };
            let mut wr = ws.lock().await;
            let devices = wr.store.load_membership()?;
            wr.membership.set(devices);
            // Membership gates DEVICE subscribers. Peer-relay
            // subscribers are already gated by cross-user auth (they
            // must chain to the same user root, same handshake as any
            // device) plus the `can_act_as_host` capability — they
            // don't need to appear in the workspace membership list
            // to legitimately federate.
            let is_federated = peer.role == crate::auth::PeerRole::PeerRelay
                && peer.user_public_key == *own_user_public_key
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
            // Replay retained document baselines first. Unlike a per-device
            // pending queue these exist before a newly invited device has ever
            // connected, which is what makes an offline owner + new guest work.
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
            // Load pending deltas before advertising Ready, but retain their
            // files until the connection task has written the complete replay.
            // A write failure therefore leaves the queue intact for reconnect.
            let pending = wr.store.load_pending(&peer.device_public_key)?;
            let mut pending_count = 0u32;
            for entry in pending {
                // Each pending entry is a full pre-encoded envelope
                // — send it verbatim.
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
            wr.add_subscriber(Subscriber {
                device_public_key: peer.device_public_key,
                role: peer.role,
                tx: outbound.clone(),
                disconnect: disconnect.clone(),
            });
            if !subscribed.contains(&workspace_id) {
                subscribed.push(workspace_id);
            }
            Ok(Some(Envelope::Ready {
                workspace_id,
                pending_deltas: pending_count.saturating_add(retained_count),
            }))
        }
        Envelope::Unsubscribe { workspace_id } => {
            if let Some(ws) = workspaces.lock().await.get(&workspace_id).cloned() {
                ws.lock().await.remove_subscriber(&peer.device_public_key);
            }
            subscribed.retain(|id| id != &workspace_id);
            Ok(None)
        }
        env @ Envelope::Delta { .. } => {
            let workspace_id = if let Envelope::Delta { workspace_id, .. } = &env {
                *workspace_id
            } else {
                unreachable!()
            };
            if !subscribed.contains(&workspace_id) {
                return Err(PeerError::Protocol(
                    "delta sent before subscribing to workspace".into(),
                ));
            }
            let ws = match workspaces.lock().await.get(&workspace_id).cloned() {
                Some(w) => w,
                None => {
                    return Ok(Some(Envelope::NotAuthorised {
                        workspace_id,
                        reason: "workspace not bound on this relay".into(),
                    }));
                }
            };
            let mut wr = ws.lock().await;
            // Author check — the peer that sent the delta is the
            // author; we don't allow one peer to forge a delta as
            // if it came from another device.
            if let Envelope::Delta {
                author_device_public_key,
                delta,
                ..
            } = &env
            {
                // A plain device can only publish deltas signed as
                // itself; a peer relay legitimately forwards deltas
                // authored by any device it hosts, so the author
                // field is not required to match its own cert.
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
                            *device != author_device_public_key
                                && !wr.subscribers.contains_key(*device)
                        })
                        .count()
                } else {
                    0
                };
                let delivered = wr.fanout_delta(
                    author_device_public_key,
                    peer.role,
                    env.clone(),
                    &raw_bytes,
                    seq,
                )?;
                metrics.inc_deltas_received();
                metrics.add_deltas_fanned_out(delivered as u64);
                metrics.add_bytes_queued(
                    (raw_bytes.len() as u64)
                        .saturating_mul((offline_devices + offline_relays) as u64),
                );
            }
            Ok(None)
        }
        env @ Envelope::Presence { .. } => {
            let workspace_id = if let Envelope::Presence { workspace_id, .. } = &env {
                *workspace_id
            } else {
                unreachable!()
            };
            if !subscribed.contains(&workspace_id) {
                return Err(PeerError::Protocol(
                    "presence sent before subscribing to workspace".into(),
                ));
            }
            if let Some(ws) = workspaces.lock().await.get(&workspace_id).cloned() {
                ws.lock()
                    .await
                    .fanout_presence(&peer.device_public_key, peer.role, env);
            }
            Ok(None)
        }
        Envelope::PeerRelayHello { hosted_workspaces } => {
            // Peer-relay handshake: silently ignore if the peer's
            // cert is not host-capable — otherwise a plain device
            // could try to smuggle its way into the federation set.
            if peer.role != crate::auth::PeerRole::PeerRelay
                || peer.user_public_key != *own_user_public_key
            {
                return Ok(None);
            }
            // Compute the intersection with what THIS relay hosts and
            // record the dialer's device pubkey as a federated peer
            // relay for each shared workspace. On next `Subscribe`
            // (which the peer relay is about to send for each
            // intersection id), the existing subscriber path enrols
            // it with role `PeerRelay` and the fan-out filter takes
            // care of the rest.
            let mut hosted_here: Vec<u64> = Vec::new();
            let wsmap = workspaces.lock().await;
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
        Envelope::SyncRequest {
            workspace_id,
            have_heads,
        } => {
            // Anti-entropy: reply with exactly the changes the requester
            // is missing, computed against this host's materialized
            // pinned doc. Read-only — does not touch subscriptions or the
            // pending queues, so a peer can reconcile whether or not it is
            // currently subscribed. Membership gates device requesters the
            // same way Subscribe does; peer relays are gated by cross-user
            // auth + host capability.
            let ws = match workspaces.lock().await.get(&workspace_id).cloned() {
                Some(w) => w,
                None => {
                    return Ok(Some(Envelope::NotAuthorised {
                        workspace_id,
                        reason: "workspace not bound on this relay".into(),
                    }));
                }
            };
            let mut wr = ws.lock().await;
            let is_federated = peer.role == crate::auth::PeerRole::PeerRelay
                && peer.user_public_key == *own_user_public_key
                && wr.known_peer_relays.contains(&peer.device_public_key);
            if !wr.membership.contains(&peer.device_public_key)
                && !is_federated
                && !wr.admit_authenticated
            {
                return Ok(Some(Envelope::NotAuthorised {
                    workspace_id,
                    reason: "device not in workspace membership".into(),
                }));
            }
            let delta_since = wr.changes_since(&have_heads);
            Ok(Some(Envelope::SyncResponse {
                workspace_id,
                delta_since,
            }))
        }
        Envelope::Ping => Ok(Some(Envelope::Pong)),
        // Envelopes the relay only *sends*, never receives; ignore
        // silently so a peer bug doesn't wedge the connection.
        Envelope::Ready { .. }
        | Envelope::NotAuthorised { .. }
        | Envelope::Pong
        | Envelope::SyncResponse { .. } => Ok(None),
    }
}

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
