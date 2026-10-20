//! The session's reader: the state machine that takes inbound frames off the
//! wire, handles state frames one (checkpoint prefix) at a time, lets private
//! frames overtake a state frame still in progress, and hands every reply to
//! the writer.

#[cfg(test)]
use crate::peer::observe_reader_backlog;
use crate::peer::{
    AccessAudit, Arc, Envelope, EnvelopeV2, PeerError, RETENTION_PREFIX_BYTE_CAP,
    RETENTION_PREFIX_FRAME_CAP, ReplayWrite, SessionCtx, SessionReader, StateDelivery,
    audit_subscriber_access, buffered_retention_prefix, commit_run_key, eligible_checkpoint_prefix,
    handle_commit_run, handle_delta_prefix, handle_envelope, handle_private, mpsc, resume_record,
    send_private_reply, trace_native_publication,
};
use std::collections::VecDeque;

use super::{SessionLoop, inbound_bytes, private_may_overtake};

/// How often a lagging connection checks whether its queues drained.
const LAG_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// How many frames the reader reads ahead of a state frame in progress.
const READ_AHEAD: usize = 32;

/// Most commits one run queues before the first is answered
/// ([`SessionReaderLoop::handle_commit_run`]).
const COMMIT_RUN: usize = READ_AHEAD;

/// The delivery budget an inbound frame holds while it waits.
type Budget = Arc<tokio::sync::OwnedSemaphorePermit>;

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

/// Keep the authenticated lease live until every owner job admitted before
/// the FIN has passed a durable barrier.
async fn durable_barrier(session: &SessionCtx<'_>, subscribed: &[u64]) -> Result<(), PeerError> {
    for id in subscribed {
        let workspace = session.workspaces.lock().await.get(id).cloned();
        if let Some(workspace) = workspace {
            let owner = crate::persistence::handle(&workspace).await;
            let done = owner
                .submit_prepared_wait(
                    1,
                    async { Ok(()) },
                    // A fence: it retains nothing.
                    |state, ()| {
                        state.retaining_only();
                        Ok(())
                    },
                    |result| async { result },
                )
                .await?;
            crate::persistence::receipt(done).await?;
        }
    }
    Ok(())
}

/// Report the shape of the read-ahead backlog when persistence diagnostics
/// are on.
fn trace_buffered_prefix(backlog: &VecDeque<(EnvelopeV2, Budget)>) {
    if crate::persistence::diagnostics()
        && let Some(shape) = buffered_retention_prefix(backlog.iter().map(|(frame, _budget)| frame))
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
}

/// The inbound side of the wire: the framed reader, and the frames read
/// ahead of a state frame still in progress.
struct Inbound<'s, R> {
    reader: &'s mut SessionReader<R>,
    backlog: VecDeque<(EnvelopeV2, Budget)>,
    /// The peer has not finished its half of the stream.
    open: bool,
}

impl<R: tokio::io::AsyncRead + Unpin> Inbound<'_, R> {
    /// Whether a frame read ahead of the state frame that controls
    /// `active_control` may be served before it.
    fn may_overtake(
        &self,
        frame: &EnvelopeV2,
        active_control: Option<u64>,
        cx: SessionLoop<'_, '_>,
    ) -> bool {
        private_may_overtake(
            frame,
            active_control,
            cx.pending_replays.load(std::sync::atomic::Ordering::SeqCst) == 0,
            self.backlog.iter().map(|(frame, _)| frame),
        )
    }

    /// Keep a frame that may not overtake, charged to the delivery budget.
    fn defer(&mut self, frame: EnvelopeV2) -> Result<(), PeerError> {
        let bytes = inbound_bytes(&frame);
        self.backlog
            .push_back((frame, crate::persistence::delivery_budget(bytes)?));
        #[cfg(test)]
        observe_reader_backlog(&self.backlog.back().unwrap().0, self.backlog.len());
        Ok(())
    }

    /// `env` and the queued deltas that commit with it as one checkpoint
    /// prefix, with their budgets.
    fn checkpoint_prefix(
        &mut self,
        env: Envelope,
        inbound_budget: Option<Budget>,
    ) -> Result<(Vec<Envelope>, Vec<Option<Budget>>), PeerError> {
        let prefix_len =
            eligible_checkpoint_prefix(&env, self.backlog.iter().map(|(frame, _)| frame));
        let mut prefix = vec![env];
        let mut prefix_budgets = vec![inbound_budget];
        for _ in 1..prefix_len {
            let Some((EnvelopeV2::State(next), budget)) = self.backlog.pop_front() else {
                return Err(PeerError::Io(std::io::Error::other(
                    "eligible checkpoint prefix changed while draining",
                )));
            };
            prefix.push(next);
            prefix_budgets.push(Some(budget));
        }
        Ok((prefix, prefix_budgets))
    }

    /// Drive the held state `operation` to its reply, meanwhile reading
    /// ahead: a private frame that may overtake is served, anything else
    /// waits in the backlog.
    async fn overtake_while<F, T>(
        &mut self,
        mut operation: std::pin::Pin<Box<F>>,
        blocked_room: Option<u64>,
        cx: SessionLoop<'_, '_>,
    ) -> Result<T, PeerError>
    where
        F: std::future::Future<Output = Result<T, PeerError>>,
    {
        loop {
            tokio::select! {
                biased;
                result = &mut operation => break result,
                frame = self.reader.read_next(), if self.open && self.backlog.len() < READ_AHEAD => {
                    let Some(frame) = frame? else { self.open = false; continue; };
                    if self.may_overtake(&frame, blocked_room, cx) {
                        if let Some(reply) = serve_private_beside(&mut operation, frame, cx).await? {
                            break Ok(reply);
                        }
                    } else {
                        self.defer(frame)?;
                    }
                }
            }
        }
    }

    /// Hand `batch` to the writer, serving private frames that may overtake
    /// while the writer's queue is full.
    async fn hand_off_reply(
        &mut self,
        replies: &mpsc::Sender<ReplayWrite>,
        batch: ReplayWrite,
        cx: SessionLoop<'_, '_>,
    ) -> Result<(), PeerError> {
        let sending = replies.send(batch);
        tokio::pin!(sending);
        loop {
            tokio::select! {
                biased;
                result = &mut sending => {
                    result.map_err(|_| PeerError::Protocol("reply writer closed".into()))?;
                    break;
                }
                frame = self.reader.read_next(), if self.open && self.backlog.len() < READ_AHEAD => {
                    let Some(frame) = frame? else { self.open = false; continue; };
                    if self.may_overtake(&frame, None, cx) {
                        if let Some(reply) = handle_private(frame, cx.peer, cx.session).await? {
                            send_private_reply(cx.session.private_tx, reply).await?;
                        }
                    } else {
                        self.defer(frame)?;
                    }
                    // Give the sole writer a turn even when the
                    // peer supplies an always-ready control flood.
                    tokio::task::yield_now().await;
                }
            }
        }
        Ok(())
    }
}

/// Serve the private `frame` while still polling the held state
/// `operation`, which may hold the authorization lane the private request
/// needs. Returns the operation's reply if it completed meanwhile.
async fn serve_private_beside<F, T>(
    operation: &mut std::pin::Pin<Box<F>>,
    frame: EnvelopeV2,
    cx: SessionLoop<'_, '_>,
) -> Result<Option<T>, PeerError>
where
    F: std::future::Future<Output = Result<T, PeerError>>,
{
    // The state future may hold the authorization lane
    // needed by this private request. Keep polling it
    // until it completes, retaining its one reply.
    let private = handle_private(frame, cx.peer, cx.session);
    tokio::pin!(private);
    let mut completed = None;
    let reply = loop {
        tokio::select! {
            biased;
            result = &mut *operation, if completed.is_none() => {
                completed = Some(result?);
            }
            result = &mut private => break result?,
        }
    };
    if let Some(reply) = reply {
        // Wait for the writer, still polling the held
        // state future that it may depend on.
        let sending = send_private_reply(cx.session.private_tx, reply);
        tokio::pin!(sending);
        loop {
            tokio::select! {
                biased;
                result = &mut *operation, if completed.is_none() => {
                    completed = Some(result?);
                }
                result = &mut sending => break result?,
            }
        }
    }
    Ok(completed)
}

/// The reader state machine. It borrows the session's reader, its
/// subscriptions and its access audit for as long as it runs; what it owns
/// (the read-ahead backlog) ends with it.
pub(super) struct SessionReaderLoop<'s, R> {
    inbound: Inbound<'s, R>,
    subscribed: &'s mut Vec<u64>,
    catching_up: bool,
    access_audit: &'s mut AccessAudit,
    authority: &'s mut tokio::sync::watch::Receiver<u64>,
    /// The queues a lagging room waits on to drain before it resumes.
    outbound: &'s mpsc::Sender<Envelope>,
    state_outbound: &'s mpsc::Sender<StateDelivery>,
    /// Where state replies and replays go to the writer.
    replies: &'s mpsc::Sender<ReplayWrite>,
}

/// The session's own queues the reader feeds or watches.
pub(super) struct ReaderQueues<'s> {
    outbound: &'s mpsc::Sender<Envelope>,
    state_outbound: &'s mpsc::Sender<StateDelivery>,
    replies: &'s mpsc::Sender<ReplayWrite>,
}

impl<'s> ReaderQueues<'s> {
    pub(super) const fn new(
        outbound: &'s mpsc::Sender<Envelope>,
        state_outbound: &'s mpsc::Sender<StateDelivery>,
        replies: &'s mpsc::Sender<ReplayWrite>,
    ) -> Self {
        Self {
            outbound,
            state_outbound,
            replies,
        }
    }
}

impl<'s, R: tokio::io::AsyncRead + Unpin> SessionReaderLoop<'s, R> {
    pub(super) fn new(
        reader: &'s mut SessionReader<R>,
        subscribed: &'s mut Vec<u64>,
        access_audit: &'s mut AccessAudit,
        authority: &'s mut tokio::sync::watch::Receiver<u64>,
        ReaderQueues {
            outbound,
            state_outbound,
            replies,
        }: ReaderQueues<'s>,
    ) -> Self {
        Self {
            inbound: Inbound {
                reader,
                backlog: VecDeque::new(),
                open: true,
            },
            subscribed,
            catching_up: false,
            access_audit,
            authority,
            outbound,
            state_outbound,
            replies,
        }
    }

    /// Read and handle frames until the peer's half of the stream ends and
    /// every owner job it admitted is durable.
    pub(super) async fn run(mut self, cx: SessionLoop<'_, '_>) -> Result<(), PeerError> {
        loop {
            let (frame, inbound_budget) =
                if let Some((frame, budget)) = self.inbound.backlog.pop_front() {
                    (frame, Some(budget))
                } else if !self.inbound.open {
                    return durable_barrier(cx.session, self.subscribed).await;
                } else if let Some(frame) = self.next_frame(cx).await? {
                    (frame, None)
                } else {
                    continue;
                };
            match frame {
                EnvelopeV2::State(env) => self.handle_state_frame(env, inbound_budget, cx).await?,
                frame => {
                    if let Some(reply) = handle_private(frame, cx.peer, cx.session).await? {
                        send_private_reply(cx.session.private_tx, reply).await?;
                    }
                }
            }
        }
    }

    /// One turn of the idle reader: the next frame to handle, or `None`
    /// after a turn that only moved the reader's own state.
    async fn next_frame(
        &mut self,
        cx: SessionLoop<'_, '_>,
    ) -> Result<Option<EnvelopeV2>, PeerError> {
        // Prefer an already-readable inbound frame over the membership
        // audit. After a live revoke the audit and a refused delta can
        // become ready together; the delta must win so the peer still
        // receives NotAuthorised rather than a bare close.
        self.access_audit.cover(self.subscribed);
        tokio::select! {
            biased;
            frame = self.inbound.reader.read_next() => {
                if let Some(frame) = frame? {
                    Ok(Some(frame))
                } else {
                    self.inbound.open = false;
                    Ok(None)
                }
            },
            () = cx.session.lag.wake.notified() => { self.catching_up = true; Ok(None) }
            // A lagging room resumes once everything already queued
            // for this connection is written, so its replay starts
            // where the wire left off.
            () = tokio::time::sleep(LAG_POLL), if self.catching_up => {
                if self.state_outbound.capacity() < self.state_outbound.max_capacity() || self.outbound.capacity() < self.outbound.max_capacity() {
                    return Ok(None);
                }
                let resume = lag_resume(cx.session, self.subscribed);
                self.catching_up = !cx.session.lag.is_empty();
                Ok(resume.map(EnvelopeV2::State))
            }
            () = tokio::time::sleep_until(self.access_audit.due()) => {
                let watched = audit_subscriber_access(cx.peer, cx.session, self.subscribed).await?;
                self.access_audit.audited(cx.peer, cx.session.agents, self.subscribed, watched);
                Ok(None)
            }
            // The epoch's sender is a process-wide static and never
            // closes, so this cannot spin on an error.
            _ = self.authority.changed() => { self.access_audit.changed(); Ok(None) }
        }
    }

    /// Handle one state frame, with the checkpoint prefix that commits with
    /// it, and hand its reply or replay to the writer.
    async fn handle_state_frame(
        &mut self,
        env: Envelope,
        inbound_budget: Option<Budget>,
        cx: SessionLoop<'_, '_>,
    ) -> Result<(), PeerError> {
        trace_native_publication("server_received", &env, None);
        if matches!(env, Envelope::Commit { .. }) {
            return self.handle_commit_run(env, inbound_budget, cx).await;
        }
        let (mut prefix, prefix_budgets) = self.inbound.checkpoint_prefix(env, inbound_budget)?;
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
        if let Envelope::Unsubscribe { workspace_id } = &env {
            // A replay handed to the writer still checks its subscription
            // until its Ready; unsubscribing under it would end the whole
            // connection. The reply to an earlier subscribe (one the device
            // sent, or a lagging room's resume) comes first, then this.
            self.inbound
                .overtake_while(Box::pin(cx.replays_written()), Some(*workspace_id), cx)
                .await?;
        }
        if replaying {
            cx.pending_replays
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        let mut ticket = None;
        let reply = {
            let subscribed = &mut *self.subscribed;
            let ticket = &mut ticket;
            let operation = Box::pin(async move {
                if prefix.is_empty() {
                    handle_envelope(cx.peer, env, cx.session, subscribed, ticket).await
                } else {
                    let mut envelopes = Vec::with_capacity(prefix.len().saturating_add(1));
                    envelopes.push(env);
                    envelopes.extend(prefix);
                    handle_delta_prefix(cx.peer, cx.session, subscribed, envelopes, prefix_budgets)
                        .await
                }
            });
            self.inbound
                .overtake_while(operation, blocked_room, cx)
                .await?
        };
        trace_buffered_prefix(&self.inbound.backlog);
        if replaying || reply.is_some() || ticket.is_some() {
            self.inbound
                .hand_off_reply(
                    self.replies,
                    ReplayWrite {
                        ticket,
                        reply,
                        replaying,
                    },
                    cx,
                )
                .await?;
        }
        Ok(())
    }

    /// Handle `env`, a `Commit`, with the commits read ahead behind it:
    /// all are queued with their owners before the first is waited for, so
    /// they become durable under shared barriers; their answers go to the
    /// writer in the order sent.
    async fn handle_commit_run(
        &mut self,
        env: Envelope,
        inbound_budget: Option<Budget>,
        cx: SessionLoop<'_, '_>,
    ) -> Result<(), PeerError> {
        let mut keys = vec![commit_run_key(&env)];
        let mut commits = vec![env];
        let mut budgets = vec![inbound_budget];
        while commits.len() < COMMIT_RUN {
            let Some((EnvelopeV2::State(next @ Envelope::Commit { .. }), _)) =
                self.inbound.backlog.front()
            else {
                break;
            };
            let key = commit_run_key(next);
            if keys.contains(&key) {
                break;
            }
            let Some((EnvelopeV2::State(next), budget)) = self.inbound.backlog.pop_front() else {
                break;
            };
            trace_native_publication("server_received", &next, None);
            keys.push(key);
            commits.push(next);
            budgets.push(Some(budget));
        }
        let subscribed = &*self.subscribed;
        let operation = Box::pin(handle_commit_run(cx.peer, cx.session, subscribed, commits));
        let replies = self.inbound.overtake_while(operation, None, cx).await?;
        drop(budgets);
        for reply in replies.into_iter().flatten() {
            self.inbound
                .hand_off_reply(
                    self.replies,
                    ReplayWrite {
                        ticket: None,
                        reply: Some(reply),
                        replaying: false,
                    },
                    cx,
                )
                .await?;
        }
        Ok(())
    }
}
