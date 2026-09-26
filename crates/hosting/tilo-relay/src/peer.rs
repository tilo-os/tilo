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

use tilo_clock::unix_now_ns;
use tokio::io::AsyncWriteExt;
use tokio::sync::{Mutex as AsyncMutex, mpsc};

use crate::auth::AuthenticatedPeer;
use crate::frame::FrameReader;
use crate::store::StoreError;
use crate::transport::Envelope;
use crate::workspace::{OutboundFrame, SharedWorkspace, Subscriber, encode_frame};

/// Maximum live fan-out backlog per connected peer, in frames. Frames are
/// shared with every other recipient (see [`OutboundFrame`]), so the backlog
/// costs a pointer per entry rather than a payload copy: the bound exists to
/// notice a peer that stopped reading, not to cap memory per subscriber. A
/// full backlog disconnects the peer; on reconnect it replays the retained
/// store and its pending queue. Sized for a busy public room, where a burst
/// of messages and reactions from hundreds of members must not disconnect a
/// member on an ordinary connection.
const PEER_OUTBOUND_CAPACITY: usize = 1024;

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
    /// Metrics — cheap counters.
    pub metrics: Arc<crate::metrics::Metrics>,
    /// This relay's own device key: the address remote-action frames use to
    /// reach the app-serving plane.
    pub own_device_public_key: [u8; 32],
    /// This relay's device signing key, which signs delegated admissions into
    /// the public rooms it hosts ([`crate::public_room`]).
    pub own_device_signing_key: ed25519_dalek::SigningKey,
    /// Direct rooms with hosted public rooms' owners, bound on first
    /// subscribe.
    pub direct_rooms: Arc<crate::public_room::DirectRoomHosting>,
    /// Answers remote-action frames addressed to this relay. `None` unless
    /// the host supplied one and advertises `app_serving`.
    pub app_serving: Option<Arc<dyn crate::app_serving::AppServingPlane>>,
    /// Live subscribers one workspace admits (see
    /// [`crate::config::RelayLimits::max_subscribers_per_workspace`]).
    pub max_subscribers_per_workspace: usize,
}

/// Read-mostly session state [`handle_envelope`] and its per-arm
/// helpers read on every inbound message for one connection. Built
/// once per [`run_peer`] call from a [`PeerContext`] plus the
/// session's own outbound / disconnect channels.
struct SessionCtx<'a> {
    workspaces: &'a WorkspaceRegistry,
    autobind: Option<&'a WorkspaceAutobind>,
    outbound: &'a mpsc::Sender<OutboundFrame>,
    max_subscribers_per_workspace: usize,
    own_user_public_key: &'a [u8; 32],
    metrics: &'a crate::metrics::Metrics,
    trust_policy: &'a Arc<std::sync::RwLock<crate::policy::TrustPolicy>>,
    disconnect: &'a mpsc::Sender<()>,
    own_device_public_key: &'a [u8; 32],
    own_device_signing_key: &'a ed25519_dalek::SigningKey,
    direct_rooms: &'a crate::public_room::DirectRoomHosting,
    app_serving: Option<&'a Arc<dyn crate::app_serving::AppServingPlane>>,
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
        trust_policy,
        own_user_public_key,
        metrics,
        own_device_public_key,
        own_device_signing_key,
        direct_rooms,
        app_serving,
        max_subscribers_per_workspace,
    } = ctx;

    // Outbound channel — every workspace runtime this peer is
    // subscribed to writes into it; we drain to the wire.
    let (tx, mut rx) = mpsc::channel::<OutboundFrame>(PEER_OUTBOUND_CAPACITY);
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
        max_subscribers_per_workspace,
        own_user_public_key: &own_user_public_key,
        metrics: &metrics,
        trust_policy: &trust_policy,
        disconnect: &disconnect_tx,
        own_device_public_key: &own_device_public_key,
        own_device_signing_key: &own_device_signing_key,
        direct_rooms: &direct_rooms,
        app_serving: app_serving.as_ref(),
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
                        Some(frame) => write.write_all(&frame).await?,
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
        let mut workspace = workspace.lock().await;
        workspace.refresh_membership()?;
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
        let bytes = crate::frame::encode(&envelope)?;
        write.write_all(&bytes).await?;
    }
    if !replay_cleanup.is_empty() {
        tilo_rt::io::run(move || {
            replay_cleanup
                .into_iter()
                .try_for_each(std::fs::remove_file)
        })
        .await?;
    }
    if let Some(direct_reply) = direct_reply {
        let bytes = crate::frame::encode(&direct_reply)?;
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
        Envelope::ReplayChannel {
            workspace_id,
            channel,
        } => {
            replay.extend(handle_replay_channel(session, subscribed, workspace_id, channel).await?);
            Ok(None)
        }
        Envelope::Redeem {
            workspace_id,
            invite_id,
        } => Ok(Some(
            handle_redeem(peer, session, workspace_id, invite_id).await,
        )),
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
        // Envelopes the relay only *sends*, never receives; ignore
        // silently so a peer bug doesn't wedge the connection.
        Envelope::Ready { .. }
        | Envelope::NotAuthorised { .. }
        | Envelope::Pong
        | Envelope::SyncResponse { .. }
        | Envelope::RemoteActionUndeliverable { .. }
        | Envelope::Admitted { .. }
        | Envelope::ChannelReplay { .. } => Ok(None),
    }
}

/// Handle `Redeem`: admit this connection's authenticated device into a
/// public room this relay was delegated admission for, when it presents a
/// registered standing invitation, and publish the signed admission into the
/// room. Every refusal is a `NotAuthorised` naming why; nothing is admitted.
async fn handle_redeem(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    workspace_id: u64,
    invite_id: [u8; 32],
) -> Envelope {
    let refuse = |reason: String| Envelope::NotAuthorised {
        workspace_id,
        reason,
    };
    // A federating relay speaks for other devices; only a device may redeem
    // for itself.
    if peer.role != crate::auth::PeerRole::Device {
        return refuse("only a device may redeem an invitation for itself".into());
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return refuse("workspace not bound on this relay".into());
    };
    let wr = ws.lock_owned().await;
    let member_user = peer.user_public_key;
    let member_device = peer.device_public_key;
    let relay_sk = session.own_device_signing_key.clone();
    let relay_device = *session.own_device_public_key;
    let outcome = tokio::task::spawn_blocking(move || {
        let mut wr = wr;
        (|| {
            let hosting = crate::public_room::PublicRoomHosting::load(wr.store.root())?
                .ok_or(crate::public_room::PublicRoomError::NotHosted)?;
            // Already admitted: its admission is retained in the room, so a
            // reconnecting member costs no re-signing and no fan-out.
            if wr.store.load_membership()?.contains(&member_device) {
                return Ok(());
            }
            let admission = hosting.admit(
                invite_id,
                member_user,
                member_device,
                &relay_sk,
                std::time::SystemTime::now(),
            )?;
            let (envelope, raw) =
                crate::public_room::admission_envelope(workspace_id, relay_device, admission)
                    .map_err(|error| {
                        crate::public_room::PublicRoomError::Store(StoreError::Io(
                            std::io::Error::other(error),
                        ))
                    })?;
            // Published before the device is recorded as a member, so a
            // crash in between leaves a device that simply redeems again.
            wr.fanout_delta(
                &relay_device,
                crate::auth::PeerRole::Device,
                &envelope,
                &raw,
                unix_now_ns(),
            )
            .map_err(crate::public_room::PublicRoomError::Store)?;
            wr.admit_device(member_device)?;
            Ok::<(), crate::public_room::PublicRoomError>(())
        })()
    })
    .await;
    match outcome {
        Ok(Ok(())) => {
            tracing::info!(
                workspace_id,
                user = %hex::encode(member_user),
                device = %hex::encode(member_device),
                "admitted a device into a hosted public room"
            );
            Envelope::Admitted { workspace_id }
        }
        Ok(Err(error)) => {
            tracing::info!(workspace_id, %error, "refused a standing-invitation redemption");
            refuse(error.to_string())
        }
        Err(_) => refuse("redemption failed".into()),
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
    let mut ws =
        get_or_autobind_workspace(session.workspaces, session.autobind, workspace_id).await?;
    if ws.is_none()
        && peer.role == crate::auth::PeerRole::Device
        && let Some(members) = session
            .direct_rooms
            .members(workspace_id, peer.user_public_key)
    {
        ws = Some(
            bind_direct_room(
                session.workspaces,
                session.direct_rooms,
                workspace_id,
                members,
            )
            .await?,
        );
    }
    let Some(ws) = ws else {
        return Ok(Some(Envelope::NotAuthorised {
            workspace_id,
            reason: "workspace not bound on this relay".into(),
        }));
    };
    let admission = Admission {
        device: peer.device_public_key,
        user: peer.user_public_key,
        federating: peer.role == crate::auth::PeerRole::PeerRelay
            && peer.user_public_key == *session.own_user_public_key,
        max_subscribers: session.max_subscribers_per_workspace,
    };
    // Admission may persist membership and the replay scans the retained
    // and pending directories: blocking I/O, so it runs on the blocking
    // pool. The workspace lock moves with it, so no delta can be fanned out
    // between the scan and the subscriber registration below.
    let wr = ws.lock_owned().await;
    let (mut wr, prepared) = tokio::task::spawn_blocking(move || {
        let mut wr = wr;
        let prepared = prepare_subscribe(&mut wr, admission, workspace_id);
        (wr, prepared)
    })
    .await
    .map_err(|_| PeerError::Io(std::io::Error::other("subscribe replay scan panicked")))?;
    let (retained_count, pending_count) = match prepared? {
        SubscribeOutcome::Rejected(reason) => {
            return Ok(Some(Envelope::NotAuthorised {
                workspace_id,
                reason: reason.into(),
            }));
        }
        SubscribeOutcome::Admitted(prepared) => {
            replay.extend(prepared.replay);
            replay_cleanup.extend(prepared.cleanup);
            (prepared.retained_count, prepared.pending_count)
        }
    };
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

/// Who is subscribing, copied out so admission can run off the async
/// workers.
#[derive(Clone, Copy)]
struct Admission {
    device: [u8; 32],
    user: [u8; 32],
    /// A peer relay of the relay's own user, federating rather than
    /// subscribing as a device.
    federating: bool,
    /// Live subscribers the workspace admits; a device already subscribed
    /// may always resubscribe.
    max_subscribers: usize,
}

/// The replay backlog gathered for an admitted subscriber.
struct PreparedReplay {
    replay: Vec<Envelope>,
    cleanup: Vec<std::path::PathBuf>,
    retained_count: u32,
    pending_count: u32,
}

enum SubscribeOutcome {
    Admitted(PreparedReplay),
    Rejected(&'static str),
}

/// Refresh membership, admit the peer, and load its replay backlog. Blocking
/// I/O: run it on the blocking pool with the workspace lock held.
fn prepare_subscribe(
    wr: &mut crate::workspace::WorkspaceRuntime,
    admission: Admission,
    workspace_id: u64,
) -> Result<SubscribeOutcome, PeerError> {
    if !wr.has_subscriber(&admission.device) && wr.subscribers.len() >= admission.max_subscribers {
        return Ok(SubscribeOutcome::Rejected(
            "workspace subscriber limit reached",
        ));
    }
    wr.refresh_membership()?;
    // Membership gates DEVICE subscribers. Peer-relay subscribers are
    // already gated by cross-user auth (they must chain to the same
    // user root, same handshake as any device) plus the
    // `can_act_as_host` capability — they don't need to appear in the
    // workspace membership list to legitimately federate.
    let is_federated = admission.federating && wr.known_peer_relays.contains(&admission.device);
    if !wr.membership.contains(&admission.device)
        && !wr.admitted_users.contains(&admission.user)
        && !is_federated
    {
        if wr.admit_authenticated {
            wr.admit_device(admission.device)?;
        } else {
            return Ok(SubscribeOutcome::Rejected(
                "device not in workspace membership",
            ));
        }
    }
    let mut replay = Vec::new();
    let mut cleanup = Vec::new();
    let (retained_count, retained) = load_retained_replay(wr, workspace_id, &mut replay)?;
    let pending_count = load_pending_replay(
        wr,
        &admission.device,
        workspace_id,
        &retained,
        &mut replay,
        &mut cleanup,
    )?;
    Ok(SubscribeOutcome::Admitted(PreparedReplay {
        replay,
        cleanup,
        retained_count,
        pending_count,
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
        match crate::frame::decode(payload) {
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
    device: &[u8; 32],
    workspace_id: u64,
    retained: &std::collections::HashSet<Vec<u8>>,
    replay: &mut Vec<Envelope>,
    replay_cleanup: &mut Vec<std::path::PathBuf>,
) -> Result<u32, PeerError> {
    let pending = wr.store.load_pending(device)?;
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
        match crate::frame::decode(payload) {
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

/// Most bytes of retained frames one [`Envelope::ChannelReplay`] carries, well
/// under the wire frame limit; a larger channel is answered in several.
const CHANNEL_REPLAY_BUDGET: usize = 16 * 1024 * 1024;

/// Handle `ReplayChannel`: the channel's retained envelopes, checkpoint
/// first, for a subscriber of the workspace only. Read-only.
async fn handle_replay_channel(
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    workspace_id: u64,
    channel: [u8; 32],
) -> Result<Vec<Envelope>, PeerError> {
    if !subscribed.contains(&workspace_id) {
        return Err(PeerError::Protocol(
            "channel replay requested before subscribing to workspace".into(),
        ));
    }
    let Some(ws) = session.workspaces.lock().await.get(&workspace_id).cloned() else {
        return Ok(Vec::new());
    };
    let wr = ws.lock_owned().await;
    let frames = tokio::task::spawn_blocking(move || wr.store.load_retained_channel(&channel))
        .await
        .map_err(|_| PeerError::Io(std::io::Error::other("channel replay read panicked")))??;
    let mut replies = Vec::new();
    let mut batch = Vec::new();
    let mut batch_bytes = 0usize;
    for frame in frames {
        if !batch.is_empty() && batch_bytes.saturating_add(frame.len()) > CHANNEL_REPLAY_BUDGET {
            replies.push(Envelope::ChannelReplay {
                workspace_id,
                channel,
                frames: std::mem::take(&mut batch),
            });
            batch_bytes = 0;
        }
        batch_bytes = batch_bytes.saturating_add(frame.len());
        batch.push(serde_bytes::ByteBuf::from(frame));
    }
    replies.push(Envelope::ChannelReplay {
        workspace_id,
        channel,
        frames: batch,
    });
    Ok(replies)
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
    let mut wr = ws.lock_owned().await;
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
    if let Some(crate::coedit::CoeditPayload::Checkpoint {
        coverage: crate::coedit::Coverage::Entries(entries),
        ..
    }) = crate::coedit::CoeditPayload::decode(delta)
        && entries.len() > crate::coedit::MAX_COVERED_ENTRIES
    {
        return Err(PeerError::Protocol(
            "checkpoint names more covered entries than the limit".into(),
        ));
    }
    let seq = unix_now_ns();
    let raw_bytes = delta.clone();
    let author = *author_device_public_key;
    let role = peer.role;
    let delta_len = raw_bytes.len();
    // The fan-out persists pending queues, the retained tail and the pinned
    // snapshot (each fsynced) and merges the delta into the pinned document:
    // blocking I/O and CPU work, so it runs on the blocking pool. The
    // workspace lock moves with it, keeping deltas in arrival order.
    let fanout =
        tokio::task::spawn_blocking(move || wr.fanout_delta(&author, role, &env, &raw_bytes, seq))
            .await
            .map_err(|_| PeerError::Io(std::io::Error::other("delta fan-out panicked")))??;
    session.metrics.inc_deltas_received();
    session
        .metrics
        .add_deltas_fanned_out(fanout.delivered as u64);
    session
        .metrics
        .add_bytes_queued((delta_len as u64).saturating_mul(fanout.queued as u64));
    Ok(None)
}

/// Handle an inbound `Presence`: fan it out to every other subscriber.
/// Ephemeral — never persisted.
///
/// Presence has no author slot of its own: the author rides inside the
/// payload ([`crate::coedit::AttributedPresence`]), and receivers attribute
/// the message to it. So a device may only publish presence attributed to
/// itself, exactly as with `Delta`; anything else — a forged author, or an
/// unattributed payload a receiver cannot check — ends the session. In a
/// public room anyone may join, so without this any member could speak as
/// any other endpoint. A peer relay forwards its own user's devices, as it
/// does for deltas.
async fn handle_presence(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    env: Envelope,
) -> Result<Option<Envelope>, PeerError> {
    let Envelope::Presence {
        workspace_id,
        payload,
    } = &env
    else {
        return Err(PeerError::Protocol(
            "handle_presence called with a non-Presence envelope".into(),
        ));
    };
    if peer.role == crate::auth::PeerRole::Device
        && crate::coedit::AttributedPresence::decode(payload)
            .is_none_or(|presence| presence.author != peer.device_public_key)
    {
        return Err(PeerError::Protocol(
            "presence author does not match authenticated device".into(),
        ));
    }
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
/// need not wait out a timeout.
async fn handle_remote_action(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    subscribed: &[u64],
    route: RemoteActionRoute,
    frame: Vec<u8>,
) -> Option<Envelope> {
    let RemoteActionRoute {
        workspace_id,
        version,
        to,
    } = route;
    let undeliverable = |reason: &str| {
        Some(Envelope::RemoteActionUndeliverable {
            workspace_id,
            to_device_public_key: to,
            reason: reason.to_string(),
        })
    };
    if version != crate::transport::REMOTE_ACTION_ENVELOPE_VERSION {
        return undeliverable("unsupported remote action envelope version");
    }
    if !subscribed.contains(&workspace_id) {
        return undeliverable("sender is not subscribed to the workspace");
    }
    if frame.len() > tilo_actions::remote::MAX_REMOTE_ACTION_FRAME_BYTES {
        return undeliverable("remote action frame exceeds the limit");
    }
    if to == *session.own_device_public_key {
        let Some(plane) = session.app_serving else {
            return undeliverable("this host does not serve apps");
        };
        return serve_locally(peer, session, plane.as_ref(), workspace_id, &frame)
            .unwrap_or_else(undeliverable);
    }
    let ws = {
        let map = session.workspaces.lock().await;
        map.get(&workspace_id).cloned()
    };
    let stamped = Envelope::RemoteAction {
        workspace_id,
        version,
        to_device_public_key: to,
        from_device_public_key: peer.device_public_key,
        from_user_public_key: peer.user_public_key,
        frame,
    };
    let delivered = match ws {
        Some(ws) => ws.lock().await.route_directed(&to, &stamped),
        None => false,
    };
    if delivered {
        None
    } else {
        undeliverable("addressed device is not connected to the workspace")
    }
}

/// Answer a frame addressed to this relay through its app-serving plane,
/// with the caller the connection authenticated. The answer is written to
/// the connection whenever the plane has it, so a request that needs an
/// application's help never stalls this peer's other traffic. `Err` when
/// the frame cannot be used.
fn serve_locally(
    peer: &AuthenticatedPeer,
    session: &SessionCtx<'_>,
    plane: &dyn crate::app_serving::AppServingPlane,
    workspace_id: u64,
    frame: &[u8],
) -> Result<Option<Envelope>, &'static str> {
    use tilo_actions::remote::{Caller, RemoteActionFrame};
    let request = RemoteActionFrame::decode(frame).map_err(|_| "malformed remote action frame")?;
    let caller = Caller::authenticated(peer.user_public_key, peer.device_public_key);
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
        let Ok(frame) = encode_frame(&Envelope::RemoteAction {
            workspace_id,
            version: crate::transport::REMOTE_ACTION_ENVELOPE_VERSION,
            to_device_public_key,
            from_device_public_key,
            from_user_public_key,
            frame,
        }) else {
            return;
        };
        let _ = outbound.try_send(frame);
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
/// Bind the direct room `workspace_id` between the two `members`, admitting
/// exactly those two people. Idempotent under the registry lock.
async fn bind_direct_room(
    workspaces: &WorkspaceRegistry,
    direct_rooms: &crate::public_room::DirectRoomHosting,
    workspace_id: u64,
    members: [[u8; 32]; 2],
) -> Result<SharedWorkspace, StoreError> {
    let mut map = workspaces.lock().await;
    if let Some(workspace) = map.get(&workspace_id) {
        return Ok(workspace.clone());
    }
    if !autobind_has_capacity(map.len()) {
        return Err(StoreError::Io(std::io::Error::other(
            "direct room limit reached",
        )));
    }
    let mut runtime = crate::workspace::WorkspaceRuntime::open(
        &direct_rooms.state_root,
        workspace_id,
        false,
        crate::workspace::Membership::default(),
    )?;
    runtime.admitted_users.extend(members);
    let workspace = Arc::new(AsyncMutex::new(runtime));
    map.insert(workspace_id, workspace.clone());
    drop(map);
    tracing::info!(
        workspace_id,
        "bound a direct room with a hosted room's owner"
    );
    Ok(workspace)
}

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
    drop(map);
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
