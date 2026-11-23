//! The connection's session loop: [`run_peer`] and its variants, and
//! `serve_peer_stream`, which sets a session up, races its reader and writer
//! against the carrier closing, and tears it down.
//!
//! The reader takes each frame off the wire and answers it, in the order sent;
//! the writer is the only task that writes to the wire, and writes what the
//! reader and the workspaces queue for the connection, presence once it is due.

mod wire;

use super::fetch::Grant as FetchGrant;
use super::{
    AccessAudit, Arc, AsyncWriteExt, AuthenticatedPeer, Envelope, EnvelopeV2,
    PEER_OUTBOUND_CAPACITY, PeerContext, PeerError, SessionCtx, SessionReader,
    audit_subscriber_access, encode_charged, handle_envelope, mpsc, write_presence,
};
use crate::metrics::EgressWrite;
use tilo_server_protocol::WorkspaceId;
use wire::{Heartbeats, Wire};

/// Whether `reply` refuses a device the server does not admit. A connection
/// that holds no workspace holds nothing the refusal leaves open, so the
/// refusal is its last word and the session ends.
fn refused_not_admitted(reply: Option<&Envelope>) -> bool {
    matches!(
        reply,
        Some(Envelope::NotAuthorised {
            refusal: tilo_server_protocol::refusal::Refusal::NotAdmitted,
            ..
        })
    )
}

/// What the reader of one session works with.
struct Reading<'s, 'a, R> {
    peer: &'s AuthenticatedPeer,
    session: &'s SessionCtx<'a>,
    reader: &'s mut SessionReader<R>,
    subscribed: &'s mut Vec<WorkspaceId>,
    access_audit: &'s mut AccessAudit,
    authority: &'s mut tokio::sync::watch::Receiver<u64>,
    heartbeats: &'s Heartbeats,
}

impl<R: tokio::io::AsyncRead + Unpin> Reading<'_, '_, R> {
    /// Read and answer frames until the peer's half of the stream ends, its
    /// access lapses, or it sends what the host does not take.
    async fn run(self) -> Result<(), PeerError> {
        loop {
            // Prefer an already-readable frame over the membership audit: a
            // refused frame still gets its refusal rather than a bare close.
            let audit_due = self.access_audit.due();
            tokio::select! {
                biased;
                frame = self.reader.read_next() => match frame? {
                    None => return Ok(()),
                    // A heartbeat is answered by the wire itself, between any
                    // two frames, whatever the writer waits for (see `wire`).
                    Some(EnvelopeV2::State(Envelope::Ping)) => self.heartbeats.owe(),
                    Some(EnvelopeV2::State(env)) => {
                        let _doing = tilo_rt::stall::doing(env.kind());
                        let reply =
                            Box::pin(handle_envelope(self.peer, env, self.session, self.subscribed))
                                .await?;
                        let ended =
                            self.subscribed.is_empty() && refused_not_admitted(reply.as_ref());
                        if let Some(reply) = reply {
                            self.session
                                .outbound
                                .send(reply)
                                .await
                                .map_err(|_| PeerError::Protocol("the session's writer ended".into()))?;
                        }
                        if ended {
                            return Err(PeerError::NotAdmitted);
                        }
                    }
                    // `SessionReader::read_next` turns these into `State` values
                    // before returning.
                    Some(_) => {
                        return Err(PeerError::Protocol(
                            "a coded frame escaped its session reader".into(),
                        ));
                    }
                },
                () = crate::peer::access_audit::until(audit_due) => {
                    Box::pin(audit_subscriber_access(self.peer, self.session, self.subscribed)).await?;
                    self.access_audit.audited(self.peer, self.session.agents);
                }
                // The epoch's sender is a process-wide static and never
                // closes, so this cannot spin on an error.
                _ = self.authority.changed() => self.access_audit.changed(),
            }
        }
    }
}

/// Write what is queued for the connection until `ending` says the session
/// ends, then what was queued by then, and close the stream.
async fn write_frames<W: tokio::io::AsyncWrite + Unpin>(
    rx: &mut mpsc::Receiver<Envelope>,
    content: &crate::content_queue::ContentQueue,
    presence: &crate::presence_mailbox::PresenceMailbox,
    write: &mut W,
    (peer, session): (&AuthenticatedPeer, &SessionCtx<'_>),
    mut ending: tokio::sync::watch::Receiver<bool>,
) -> Result<(), PeerError> {
    loop {
        // A slot the last write freed goes to content that waited for it, so
        // the queue behind a busy sender keeps moving without further pieces
        // arriving ([`crate::content_queue`]).
        content.serve_waiting(session.outbound);
        if *ending.borrow_and_update() || ending.has_changed().is_err() {
            while let Ok(frame) = rx.try_recv() {
                write
                    .write_all(&encode_charged(&frame, session, false)?)
                    .await?;
            }
            write.shutdown().await?;
            return Ok(());
        }
        tokio::select! {
            biased;
            frame = rx.recv() => {
                let Some(frame) = frame else {
                    write.shutdown().await?;
                    return Ok(());
                };
                let live = matches!(frame, Envelope::Presence { .. });
                write.write_all(&encode_charged(&frame, session, live)?).await?;
            }
            () = presence.ready() => {
                if presence.next_due().is_some_and(|due| due <= tokio::time::Instant::now()) {
                    write_presence(presence, peer, session, write).await?;
                }
            }
            _ = ending.changed() => {}
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
/// membership while connected, the server does not admit the device
/// ([`PeerError::NotAdmitted`]), the peer violates the protocol (e.g. a
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
    serve_peer_stream(peer, read, write, ctx, connection.closed()).await
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
    serve_peer_stream(peer, read, write, ctx, closed).await
}

// Shared stream driver also exercised with a deterministic in-memory wire.
#[expect(
    clippy::too_many_lines,
    reason = "one connection's whole life, in the order it happens: set up, serve until it ends, and the teardown every exit shares"
)]
pub(super) async fn serve_peer_stream<R, W>(
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
        revocations,
        trust_policy,
        own_user_public_key,
        own_device_cert,
        metrics,
        own_device_public_key,
        app_serving,
        admin,
        fetch,
        agents,
        environments,
    } = ctx;
    let agent = std::sync::OnceLock::new();
    // Everything this session is sent passes here, so its egress and the
    // server's are what the stream accepted, not an estimate.
    let wire = Wire::new(EgressWrite::new(write, &metrics, peer.device_public_key));
    let (mut write, beats) = (wire.writer(), Heartbeats::default());

    // Outbound channel: workspaces write here what they fan out to this peer.
    let (tx, mut rx) = mpsc::channel::<Envelope>(PEER_OUTBOUND_CAPACITY);
    let content = Arc::new(crate::content_queue::ContentQueue::default());
    let (disconnect_tx, mut disconnect_rx) = mpsc::channel::<()>(1);
    let stop = Arc::new(tokio::sync::Notify::new());
    let mut reader = SessionReader::new(read);
    // Track our subscriptions so we can unregister on disconnect.
    let mut subscribed: Vec<WorkspaceId> = Vec::new();
    let mut authority = crate::authority_epoch::subscribe();
    authority.mark_unchanged();
    // The host's own device, connected in process, is audited only on change.
    let mut access_audit = AccessAudit::new(
        peer.device_public_key == own_device_cert.device_public_key,
        &peer,
        &agents,
    );

    // Presence is batched per connection, in the order it was posted.
    let presence = Arc::new(crate::presence_mailbox::PresenceMailbox::default());
    let execution = tilo_actions::remote::ExecutionLease::mint();
    let session = SessionCtx {
        workspaces: &workspaces,
        outbound: &tx,
        content: &content,
        own_user_public_key: &own_user_public_key,
        metrics: &metrics,
        trust_policy: &trust_policy,
        disconnect: &disconnect_tx,
        stop: Some(&stop),
        own_device_public_key: &own_device_public_key,
        app_serving: app_serving.as_ref(),
        revocations: &revocations,
        admin: admin.as_deref(),
        fetch: fetch.as_deref(),
        agents: &agents,
        environments: &environments,
        agent: &agent,
        execution: &execution,
        presence: Some(&presence),
    };

    // The person's record says this key of theirs is here now, and when it
    // was last: a device of theirs reading their devices sees it online.
    if let Some(admin) = &admin {
        admin.session_opened(&peer.cert).await;
    }

    let grant = FetchGrant::open(&session, &peer, &tx);
    // This device is sent its person's workspace list as it changes, so what
    // it lists is never older than the workspaces it may enter: one made, one
    // they joined, one renamed, one they left.
    let personal_watch = admin
        .as_deref()
        .map(|admin| admin.watch_personal(&peer.cert, tx.clone()));
    // A clean EOF is only one of the ways this ends: a write to a
    // connection the peer has already closed, a frame that will not
    // decode, an envelope the handler rejects. Every one of them has to
    // unregister, so the session runs to a value and the cleanup below
    // is unconditional. Leaving a subscription behind is not cosmetic —
    // fan-out then writes deltas to a peer that is gone instead of
    // queueing them for store-and-forward, and they are lost.
    let outcome = {
        let reading = Reading {
            peer: &peer,
            session: &session,
            reader: &mut reader,
            subscribed: &mut subscribed,
            access_audit: &mut access_audit,
            authority: &mut authority,
            heartbeats: &beats,
        }
        .run();
        let (end_writer, ending) = tokio::sync::watch::channel(false);
        let writing = write_frames(
            &mut rx,
            &content,
            &presence,
            &mut write,
            (&peer, &session),
            ending,
        );
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
        admin.session_ended(&peer.cert).await;
    }

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
    if let (Some(admin), Some(watch)) = (admin.as_deref(), personal_watch) {
        admin.unwatch_personal(watch);
    }
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

/// How long a refused session waits for its device to close before the
/// connection closes under the refusal still on its way.
const REFUSAL_READ: std::time::Duration = std::time::Duration::from_millis(500);

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
                let _ = stopping.outbound.try_send(Envelope::AgentSessionStopped);
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
            let written = tokio::select! {
                _ = closed.as_mut() => false,
                _ = &mut writing => true,
            };
            if written && matches!(&result, Err(PeerError::NotAdmitted)) {
                // Written is not read: the connection's close discards what
                // the wire has not taken, so the device gets a moment to
                // close first, as one that heard its refusal does.
                let _ = tokio::time::timeout(REFUSAL_READ, closed.as_mut()).await;
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
    subscribed: &[WorkspaceId],
) {
    tracing::debug!(
        device = %hex::encode(&peer.device_public_key[..4]),
        bytes_sent,
        "peer session egress"
    );
    tracing::debug!(phase = "peer_end", device = %hex::encode(&peer.device_public_key[..4]), outcome = ?outcome, workspaces = ?subscribed);
}

/// Remove this session's subscriber from every workspace it subscribed to.
async fn unregister_subscriber(
    workspaces: &super::WorkspaceRegistry,
    subscribed: &[WorkspaceId],
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
        Ok(()) => Ok(()),
        // The server does not admit the device: its refusal went out and the
        // session ended with it, so nothing here lapsed and nothing is counted.
        Err(PeerError::NotAdmitted) => {
            tracing::info!(
                device = %hex::encode(&device[..4]),
                "peer session refused: not admitted"
            );
            Ok(())
        }
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
