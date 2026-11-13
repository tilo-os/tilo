//! The connection's session loop: [`run_peer`] and its variants, and
//! `run_peer_stream`, which sets a session up, races its [`reader`] and
//! [`writer`] state machines against the carrier closing, and tears it down,
//! plus the inbound-frame helpers the reader uses.

mod reader;
mod wire;
mod writer;

use super::fetch::Grant as FetchGrant;
use super::{
    AccessAudit, Arc, AuthenticatedPeer, Envelope, EnvelopeV2, HashMap, Lag,
    PEER_OUTBOUND_CAPACITY, PeerContext, PeerError, ReplayWrite, SessionCtx, SessionReader,
    StateDelivery, mpsc,
};
use crate::metrics::EgressWrite;
use reader::{ReaderQueues, SessionReaderLoop};
use wire::{Heartbeats, Wire};
use writer::{SessionWriter, WriterQueues};

/// What the reader and the writer of one session both borrow while they
/// run concurrently.
#[derive(Clone, Copy)]
struct SessionLoop<'s, 'a> {
    peer: &'s AuthenticatedPeer,
    session: &'s SessionCtx<'a>,
    /// Subscribe replays the reader has handed to the writer and the writer
    /// has not finished; live frames wait while any is pending.
    pending_replays: &'s std::sync::atomic::AtomicUsize,
    /// Woken each time the writer finishes one of those replays.
    replay_written: &'s tokio::sync::Notify,
    /// Pings read and not yet answered; the wire answers them at once.
    heartbeats: &'s Heartbeats,
}

impl<'s, 'a> SessionLoop<'s, 'a> {
    const fn new(
        peer: &'s AuthenticatedPeer,
        session: &'s SessionCtx<'a>,
        pending_replays: &'s std::sync::atomic::AtomicUsize,
        replay_written: &'s tokio::sync::Notify,
        heartbeats: &'s Heartbeats,
    ) -> Self {
        Self {
            peer,
            session,
            pending_replays,
            replay_written,
            heartbeats,
        }
    }

    /// Until the writer has finished every replay the reader handed it.
    async fn replays_written(self) -> Result<(), PeerError> {
        loop {
            let written = self.replay_written.notified();
            tokio::pin!(written);
            written.as_mut().enable();
            if self
                .pending_replays
                .load(std::sync::atomic::Ordering::SeqCst)
                == 0
            {
                return Ok(());
            }
            written.await;
        }
    }
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
    run_peer_stream(peer, read, write, ctx, connection.closed()).await
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
    run_peer_stream(peer, read, write, ctx, closed).await
}

// Shared stream driver also exercised with a deterministic in-memory wire.
#[expect(
    clippy::too_many_lines,
    reason = "one connection's whole life, in the order it happens: set up, serve until it ends, and the teardown every exit shares"
)]
#[expect(
    clippy::significant_drop_tightening,
    reason = "the session context, the loop context and the writer's queues are borrowed by the concurrent reader and writer futures, and teardown reads the session context again; the lint does not see those uses"
)]
pub(super) async fn run_peer_stream<R, W>(
    peer: AuthenticatedPeer,
    read: R,
    write: W,
    ctx: PeerContext,
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
        own_device_cert,
        metrics,
        own_device_public_key,
        app_serving,
        carried_calls,
        admin,
        fetch,
        agents,
        environments,
        positions,
        compactor,
        member_limits,
    } = ctx;
    let resumes = std::sync::Mutex::new(HashMap::new());
    let agent = std::sync::OnceLock::new();
    let channel_reader = std::sync::atomic::AtomicBool::new(false);
    let lag = Arc::new(Lag::default());
    // Everything this session is sent passes here, so its egress and the
    // relay's are what the stream accepted, not an estimate.
    let wire = Wire::new(EgressWrite::new(write, &metrics, peer.device_public_key));
    let (mut write, beats) = (wire.writer(), Heartbeats::default());

    // Outbound channel: workspaces write here what they fan out to this peer.
    let (tx, mut rx) = mpsc::channel::<Envelope>(PEER_OUTBOUND_CAPACITY);
    let (state_tx, mut state_rx) = mpsc::channel::<StateDelivery>(PEER_OUTBOUND_CAPACITY);
    let (disconnect_tx, mut disconnect_rx) = mpsc::channel::<()>(1);
    let stop = Arc::new(tokio::sync::Notify::new());
    let mut reader = SessionReader::new(read);
    // Track our subscriptions so we can unregister on disconnect.
    let mut subscribed: Vec<u64> = Vec::new();
    let mut authority = crate::authority_epoch::subscribe();
    authority.mark_unchanged();
    // The host's own device, connected in process, is audited only on change.
    let mut access_audit =
        AccessAudit::new(peer.device_public_key == own_device_cert.device_public_key);

    let connection_live = ConnectionLease(Arc::new(std::sync::atomic::AtomicBool::new(true)));
    // Presence is batched per connection, in the order it was posted.
    let presence = Arc::new(crate::presence_mailbox::PresenceMailbox::default());
    let execution = tilo_actions::remote::ExecutionLease::mint();
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
        stop: Some(&stop),
        own_device_public_key: &own_device_public_key,
        app_serving: app_serving.as_ref(),
        revocations: &revocations,
        carried_calls: carried_calls.clone(),
        admin: admin.as_deref(),
        fetch: fetch.as_deref(),
        agents: &agents,
        environments: &environments,
        positions,
        compactor: compactor.as_ref(),
        member_limits: member_limits.as_deref(),
        resumes: &resumes,
        agent: &agent,
        channel_reader: &channel_reader,
        execution: &execution,
        lag: &lag,
        presence: Some(&presence),
    };

    // The person's record says this key of theirs is here now, and when it
    // was last: a device of theirs reading their devices sees it online.
    if let Some(admin) = &admin {
        admin.session_opened(&peer.cert);
    }

    let grant = FetchGrant::open(&session, &peer, &tx);
    // A clean EOF is only one of the ways this ends: a write to a
    // connection the peer has already closed, a frame that will not
    // decode, an envelope the handler rejects. Every one of them has to
    // unregister, so the session runs to a value and the cleanup below
    // is unconditional. Leaving a subscription behind is not cosmetic —
    // fan-out then writes deltas to a peer that is gone instead of
    // queueing them for store-and-forward, and they are lost.
    let (reply_tx, mut reply_rx) = mpsc::channel::<ReplayWrite>(8);
    let pending_replays = std::sync::atomic::AtomicUsize::new(0);
    let replay_written = tokio::sync::Notify::new();
    let outcome = {
        let cx = SessionLoop::new(&peer, &session, &pending_replays, &replay_written, &beats);
        let reading = SessionReaderLoop::new(
            &mut reader,
            &mut subscribed,
            &mut access_audit,
            &mut authority,
            ReaderQueues::new(&tx, &state_tx, &reply_tx),
        )
        .run(cx);
        let queues = WriterQueues::new(&mut rx, &mut state_rx, &mut reply_rx);
        let (end_writer, ending) = tokio::sync::watch::channel(false);
        let writing = SessionWriter::new(queues, &mut write, &presence).run(cx, ending);
        until_ended(
            reading,
            writing,
            wire.answer_heartbeats(&beats),
            end_writer,
            &mut closed,
            &mut disconnect_rx,
            Stopping {
                stop: &stop,
                outbound: &tx,
                // Stopped by the person while the device stays theirs, rather than revoked.
                stopped: &|| {
                    admin
                        .as_deref()
                        .is_some_and(|admin| admin.runs_agent(&peer.device_public_key))
                },
            },
            &metrics,
        )
        .await
    };
    // What an agent's run on this connection started is not followed again.
    execution.close();
    if let Some(admin) = &admin {
        admin.agent_session_ended(&peer.device_public_key, &stop);
        admin.session_ended(&peer.cert);
    }

    connection_live.end();
    grant.end();
    let outcome = finish_peer_outcome(outcome, &metrics, &peer.device_public_key);
    let sent = wire.io(EgressWrite::bytes_sent);
    trace_session_end(&peer, sent, &outcome, &subscribed);
    unregister_subscriber(
        &workspaces,
        &subscribed,
        &peer.device_public_key,
        session.outbound,
    )
    .await;
    // A browser environment's device that was here and is not any more: its
    // environment ends, unless it is back before the host that placed it looks.
    environments.departed(peer.device_public_key);
    outcome
}

/// How a session is stopped: the notice that stops it, where the device is told, and whether the
/// person stopped its run (it is told, and connects no more) rather than its device being revoked.
struct Stopping<'a> {
    stop: &'a tokio::sync::Notify,
    outbound: &'a mpsc::Sender<Envelope>,
    stopped: &'a (dyn Fn() -> bool + Sync),
}

/// How long a stopped agent's session waits for the device to hear that it was stopped.
const STOPPED_WRITE: std::time::Duration = std::time::Duration::from_secs(2);

/// Run a session's `reading` and `writing` until the session ends: the
/// connection closes, the peer overflows its queue, the session is stopped
/// ([`Stopping`]), or either half ends.
/// When the reader ends it (the peer closed, or lost its access), the
/// writer is told through `end_writer` and writes what was queued for the
/// peer then, a refusal among it, before the close; unless the connection
/// is gone first.
async fn until_ended<C: std::future::Future>(
    reading: impl std::future::Future<Output = Result<(), PeerError>>,
    writing: impl std::future::Future<Output = Result<(), PeerError>>,
    heartbeats: impl std::future::Future<Output = Result<(), PeerError>>,
    end_writer: tokio::sync::watch::Sender<bool>,
    closed: &mut std::pin::Pin<&mut C>,
    disconnect: &mut mpsc::Receiver<()>,
    stopping: Stopping<'_>,
    metrics: &crate::metrics::Metrics,
) -> Result<(), PeerError> {
    tokio::pin!(reading, writing, heartbeats);
    tokio::select! {
        _ = closed.as_mut() => Ok(()),
        // The person stopped the agent this session is a run of: the device is told, so it
        // connects no more and its presence ends with it, and the session ends once that is
        // written, or after a moment on a connection that takes nothing.
        () = stopping.stop.notified() => {
            if (stopping.stopped)() {
                let _ = stopping.outbound.try_send(Envelope::AgentRunStopped);
            }
            let _ = end_writer.send(true);
            // Written is not read: closing the connection now would drop the word on the way,
            // so the session waits for the device to close it, as one that heard it does.
            let heard = async {
                let _ = (&mut writing).await;
                closed.as_mut().await;
            };
            let _ = tokio::time::timeout(STOPPED_WRITE, heard).await;
            Ok(())
        }
        _ = disconnect.recv() => {
            metrics.inc_subscriber_dropped_backpressure();
            Err(PeerError::Protocol("subscriber exceeded outbound backpressure limit".into()))
        }
        result = &mut reading => {
            let _ = end_writer.send(true);
            tokio::select! {
                _ = closed.as_mut() => {}
                _ = &mut writing => {}
            }
            result
        }
        result = &mut writing => result,
        result = &mut heartbeats => result,
    }
}

/// Log what the ended session wrote and, with correlated diagnostics on, how
/// it ended.
fn trace_session_end(
    peer: &AuthenticatedPeer,
    bytes_sent: u64,
    outcome: &Result<(), PeerError>,
    subscribed: &[u64],
) {
    tracing::debug!(
        device = %hex::encode(&peer.device_public_key[..4]),
        bytes_sent,
        "peer session egress"
    );
    if crate::persistence::correlated_diagnostics() {
        tracing::info!(phase = "peer_end", device = %hex::encode(&peer.device_public_key[..4]), outcome = ?outcome, workspaces = ?subscribed);
    }
}

/// Remove this session's subscriber from every workspace it subscribed to.
async fn unregister_subscriber(
    workspaces: &super::WorkspaceRegistry,
    subscribed: &[u64],
    device: &[u8; 32],
    outbound: &mpsc::Sender<Envelope>,
) {
    let wsmap = workspaces.lock().await;
    for id in subscribed {
        if let Some(ws) = wsmap.get(id) {
            ws.lock().await.remove_subscriber_session(device, outbound);
        }
    }
    drop(wsmap);
}

pub(super) fn finish_peer_outcome(
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
impl ConnectionLease {
    /// Mark the connection gone: what a workspace would deliver to it stops
    /// now, not at teardown.
    fn end(&self) {
        self.0.store(false, std::sync::atomic::Ordering::SeqCst);
    }
}
impl Drop for ConnectionLease {
    fn drop(&mut self) {
        self.end();
    }
}
pub(super) fn inbound_bytes(frame: &EnvelopeV2) -> usize {
    match frame {
        EnvelopeV2::State(Envelope::Delta { delta, .. }) => delta.len().saturating_add(256),
        EnvelopeV2::State(Envelope::RetiredPresence { payload, .. }) => {
            payload.len().saturating_add(256)
        }
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
