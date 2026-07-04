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

/// Sequence-number source for enqueued deltas — a monotonic clock
/// so filenames sort in delivery order across restarts.
pub fn now_ns() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Run one peer connection until the wire closes.
///
/// `read` / `write` are the halves of the bidirectional stream
/// **after** the auth handshake completed.
pub async fn run_peer<R, W>(
    peer: AuthenticatedPeer,
    read: R,
    mut write: W,
    workspaces: WorkspaceRegistry,
) -> Result<(), PeerError>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
    W: tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    // Outbound channel — every workspace runtime this peer is
    // subscribed to writes into it; we drain to the wire.
    let (tx, mut rx) = mpsc::unbounded_channel::<Envelope>();
    let mut reader = FrameReader::new(read);
    // Track our subscriptions so we can unregister on disconnect.
    let mut subscribed: Vec<u64> = Vec::new();

    loop {
        tokio::select! {
            biased;
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
                        if let Some(reply) = handle_envelope(
                            &peer,
                            env,
                            &workspaces,
                            &tx,
                            &mut subscribed,
                        ).await? {
                            let bytes = crate::transport::encode(&reply)?;
                            write.write_all(&bytes).await?;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => return Err(PeerError::Frame(e)),
                }
            }
        }
    }

    // Clean up subscriptions on disconnect.
    let wsmap = workspaces.lock().await;
    for id in subscribed {
        if let Some(ws) = wsmap.get(&id) {
            ws.lock().await.remove_subscriber(&peer.device_public_key);
        }
    }
    Ok(())
}

async fn handle_envelope(
    peer: &AuthenticatedPeer,
    env: Envelope,
    workspaces: &WorkspaceRegistry,
    outbound: &mpsc::UnboundedSender<Envelope>,
    subscribed: &mut Vec<u64>,
) -> Result<Option<Envelope>, PeerError> {
    match env {
        Envelope::Subscribe { workspace_id } => {
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
            if !wr.membership.contains(&peer.device_public_key)
                && !wr.membership.devices.is_empty()
            {
                return Ok(Some(Envelope::NotAuthorised {
                    workspace_id,
                    reason: "device not in workspace membership".into(),
                }));
            }
            // Drain any pending deltas before advertising Ready.
            let pending = wr
                .store
                .drain_pending(&peer.device_public_key)
                .unwrap_or_default();
            let pending_count = pending.len() as u32;
            for raw in pending {
                // Each pending entry is a full pre-encoded envelope
                // — send it verbatim.
                if outbound
                    .send(match crate::transport::decode(&raw[4..]) {
                        Ok(e) => e,
                        Err(_) => continue,
                    })
                    .is_err()
                {
                    // Outbound closed → connection dying; drop.
                    break;
                }
            }
            wr.add_subscriber(Subscriber {
                device_public_key: peer.device_public_key,
                tx: outbound.clone(),
            });
            if !subscribed.contains(&workspace_id) {
                subscribed.push(workspace_id);
            }
            Ok(Some(Envelope::Ready {
                workspace_id,
                pending_deltas: pending_count,
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
                if author_device_public_key != &peer.device_public_key {
                    return Err(PeerError::Protocol(
                        "author does not match authenticated device".into(),
                    ));
                }
                let seq = now_ns();
                let raw_bytes = delta.clone();
                let _ = wr.fanout_delta(&peer.device_public_key, env.clone(), &raw_bytes, seq)?;
            }
            Ok(None)
        }
        env @ Envelope::Presence { .. } => {
            let workspace_id = if let Envelope::Presence { workspace_id, .. } = &env {
                *workspace_id
            } else {
                unreachable!()
            };
            if let Some(ws) = workspaces.lock().await.get(&workspace_id).cloned() {
                ws.lock().await.fanout_presence(&peer.device_public_key, env);
            }
            Ok(None)
        }
        Envelope::Ping => Ok(Some(Envelope::Pong)),
        // Envelopes the relay only *sends*, never receives; ignore
        // silently so a peer bug doesn't wedge the connection.
        Envelope::Ready { .. } | Envelope::NotAuthorised { .. } | Envelope::Pong => Ok(None),
    }
}
