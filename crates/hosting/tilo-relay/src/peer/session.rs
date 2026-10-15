//! The connection's session loop: [`run_peer`] and its variants, the
//! reader/writer select in `run_peer_stream`, lag resumption and the
//! inbound-frame helpers it uses.

#[cfg(test)]
use super::observe_reader_backlog;
use super::{
    AccessAudit, Arc, AsyncWriteExt, AuthenticatedPeer, Envelope, EnvelopeV2, HashMap, Lag,
    PEER_OUTBOUND_CAPACITY, PeerContext, PeerError, RETENTION_PREFIX_BYTE_CAP,
    RETENTION_PREFIX_FRAME_CAP, ReplayWrite, STATE_RUN, SessionCoding, SessionCtx, SessionReader,
    StateDelivery, audit_subscriber_access, authorize_state_frame, buffered_retention_prefix,
    delivery_room, eligible_checkpoint_prefix, encode_charged, finish_replay_write,
    handle_delta_prefix, handle_envelope, handle_private, mpsc, resume_record, send_private_reply,
    trace_native_publication, write_presence, write_private, write_state_deliveries,
};

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
        crate::timing::window("state.lag_resume", crate::timing::start());
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
    /// author.
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
pub(super) async fn run_peer_stream<R, W>(
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
        member_limits,
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
        member_limits: member_limits.as_deref(),
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
                    // What else is queued for the same room goes with it.
                    let room = delivery_room(&state);
                    let mut run = vec![state];
                    while run.len() < STATE_RUN {
                        match state_rx.try_recv() {
                            Ok(next) if room.is_some() && delivery_room(&next) == room => {
                                run.push(next);
                            }
                            Ok(next) => {
                                deferred_state = Some(next);
                                break;
                            }
                            Err(_) => break,
                        }
                    }
                    write_state_deliveries(
                        run,
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

pub(super) fn private_may_overtake<'a>(
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

pub(super) fn inbound_bytes(frame: &EnvelopeV2) -> usize {
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
