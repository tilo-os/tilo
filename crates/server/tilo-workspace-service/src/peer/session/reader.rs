//! The session's reader: the state machine that takes inbound frames off the
//! wire, handles state frames one (checkpoint prefix) at a time, answers
//! heartbeats that arrive beside a state frame still in progress, and hands
//! every reply to the writer. A commit holds the reader only while it is
//! admitted: its answer waits for its save beside the frames sent after it.

use crate::peer::{
    AccessAudit, Arc, Envelope, EnvelopeV2, PeerError, RETENTION_PREFIX_BYTE_CAP,
    RETENTION_PREFIX_FRAME_CAP, ReplayWrite, SessionCtx, SessionReader, StateDelivery, Subscribed,
    Subscription, admit_commit, answered_as_replay, audit_subscriber_access,
    buffered_retention_prefix, eligible_checkpoint_prefix, handle_delta_prefix, handle_envelope,
    mpsc, resume_document, subscribe, trace_native_publication,
};
use std::collections::VecDeque;

use super::{SessionLoop, inbound_bytes};
use tilo_server_protocol::WorkspaceId;

/// How often a lagging connection checks whether its queues drained.
const LAG_POLL: std::time::Duration = std::time::Duration::from_millis(20);

/// How many frames the reader reads ahead of a state frame in progress.
const READ_AHEAD: usize = 32;
/// A state frame handled for this long is logged: every state frame
/// behind it, a call's heartbeat among them, waited as long.
const SLOW_STATE_FRAME: std::time::Duration = std::time::Duration::from_secs(2);

/// Most commits of one connection admitted and not yet answered: past it,
/// the reader waits for the oldest to be answered before it admits another.
const PENDING_COMMITS: usize = 64;

/// Most subscriptions to different workspaces one run admits and captures
/// side by side ([`SessionReaderLoop::handle_subscribe_run`]).
const SUBSCRIBE_RUN: usize = 16;

/// The workspace a `Subscribe` or `SubscribeFrom` asks for.
const fn subscription_target(env: &Envelope) -> Option<WorkspaceId> {
    match env {
        Envelope::Subscribe { workspace_id } | Envelope::SubscribeFrom { workspace_id, .. } => {
            Some(*workspace_id)
        }
        _ => None,
    }
}

/// Whether `reply` refuses a device the server does not admit. A connection
/// that holds no workspace holds nothing the refusal leaves open, so the
/// refusal is its last word and the session ends. One that held a workspace
/// before keeps the protocol-violation close for its changed authority.
fn refused_not_admitted(reply: Option<&Envelope>) -> bool {
    matches!(
        reply,
        Some(Envelope::NotAuthorised {
            refusal: tilo_server_protocol::refusal::Refusal::NotAdmitted,
            ..
        })
    )
}

/// The delivery budget an inbound frame holds while it waits.
type Budget = Arc<tokio::sync::OwnedSemaphorePermit>;

/// The next lagging workspace this connection can resume: a `SubscribeFrom` at
/// its current position, handled as if the device had sent it. A current
/// plain subscription catches up through the same authorized full replay.
fn lag_resume(session: &SessionCtx<'_>, subscribed: &[WorkspaceId]) -> Option<Envelope> {
    while let Some(workspace_id) = session.lag.next() {
        if !subscribed.contains(&workspace_id) {
            continue;
        }
        crate::timing::window("state.lag_resume", crate::timing::start());
        if let Some(Envelope::Position { position, .. }) =
            resume_document(session, workspace_id, [])
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
async fn durable_barrier(
    session: &SessionCtx<'_>,
    subscribed: &[WorkspaceId],
) -> Result<(), PeerError> {
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

/// A commit's answer, once its save is durable.
type CommitAnswer<'c> = std::pin::Pin<
    Box<dyn std::future::Future<Output = Result<Option<Envelope>, PeerError>> + Send + 'c>,
>;

/// The commits this connection admitted and has not answered yet. Their
/// saves run beside what the reader handles after them: a read sent behind a
/// bulk write is not held for the write's save. They are answered in the
/// order sent.
struct Commits<'c> {
    /// Admitted, waiting for their saves, oldest first.
    saving: n0_future::FuturesOrdered<CommitAnswer<'c>>,
    /// Answered, waiting until the writer's queue takes them, oldest first.
    answered: VecDeque<Envelope>,
}

impl Commits<'_> {
    fn len(&self) -> usize {
        self.saving.len().saturating_add(self.answered.len())
    }

    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Take the oldest commit's answer once it is ready, or hand the oldest
    /// answer to the writer. Cancel-safe; pending forever with no commit.
    async fn settle(&mut self, replies: &mpsc::Sender<ReplayWrite>) -> Result<(), PeerError> {
        use n0_future::StreamExt as _;
        tokio::select! {
            biased;
            permit = replies.reserve(), if !self.answered.is_empty() => {
                let permit = permit.map_err(|_| PeerError::Protocol("reply writer closed".into()))?;
                permit.send(ReplayWrite {
                    ticket: None,
                    answer: Vec::new(),
                    reply: self.answered.pop_front(),
                    replaying: false,
                });
                Ok(())
            }
            settled = self.saving.next(), if !self.saving.is_empty() => {
                if let Some(reply) = settled.transpose()?.flatten() {
                    self.answered.push_back(reply);
                }
                Ok(())
            }
            else => std::future::pending().await,
        }
    }
}

/// The inbound side of the wire: the framed reader, the frames read ahead
/// of a state frame still in progress, and the commits still being saved.
struct Inbound<'s, 'c, R> {
    reader: &'s mut SessionReader<R>,
    backlog: VecDeque<(EnvelopeV2, Budget)>,
    /// The peer has not finished its half of the stream.
    open: bool,
    /// Where state replies and replays go to the writer.
    replies: &'s mpsc::Sender<ReplayWrite>,
    commits: Commits<'c>,
}

impl<R: tokio::io::AsyncRead + Unpin> Inbound<'_, '_, R> {
    /// Keep a frame that may not overtake, charged to the delivery budget.
    fn defer(&mut self, frame: EnvelopeV2) -> Result<(), PeerError> {
        let bytes = inbound_bytes(&frame);
        self.backlog
            .push_back((frame, crate::persistence::delivery_budget(bytes)?));
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

    /// Keep `frame`, read ahead of the state frame in progress: a
    /// heartbeat is answered at once, a state frame waits in the backlog.
    fn read_ahead(
        &mut self,
        frame: Option<EnvelopeV2>,
        cx: SessionLoop<'_, '_>,
    ) -> Result<(), PeerError> {
        match frame {
            None => self.open = false,
            Some(EnvelopeV2::State(Envelope::Ping)) => cx.heartbeats.owe(),
            Some(frame @ EnvelopeV2::State(_)) => self.defer(frame)?,
            // `SessionReader::read_next` turns these into `State` values
            // before returning.
            Some(
                EnvelopeV2::UnsignedDelta { .. }
                | EnvelopeV2::PresenceBatch { .. }
                | EnvelopeV2::CodedDelta { .. },
            ) => {
                return Err(PeerError::Protocol(
                    "a coded frame escaped its session reader".into(),
                ));
            }
        }
        Ok(())
    }

    /// What the reader does beside an operation in progress: read a frame
    /// ahead, or settle a commit ([`Commits::settle`]). Cancel-safe; pending
    /// forever when there is neither to do.
    async fn step(&mut self, cx: SessionLoop<'_, '_>) -> Result<(), PeerError> {
        let reading = self.open && self.backlog.len() < READ_AHEAD;
        let settling = !self.commits.is_empty();
        tokio::select! {
            biased;
            frame = self.reader.read_next(), if reading => {
                let frame = frame?;
                self.read_ahead(frame, cx)
            }
            settled = self.commits.settle(self.replies), if settling => settled,
            else => std::future::pending().await,
        }
    }

    /// Drive the held state `operation` to its reply, meanwhile reading
    /// ahead and settling commits ([`Self::step`]).
    async fn overtake_while<F, T>(
        &mut self,
        mut operation: std::pin::Pin<Box<F>>,
        cx: SessionLoop<'_, '_>,
    ) -> Result<T, PeerError>
    where
        F: std::future::Future<Output = Result<T, PeerError>>,
    {
        loop {
            tokio::select! {
                biased;
                result = &mut operation => break result,
                stepped = self.step(cx) => stepped?,
            }
        }
    }

    /// Hand `batch` to the writer, reading ahead and settling commits while
    /// the writer's queue is full ([`Self::step`]).
    async fn hand_off_reply(
        &mut self,
        batch: ReplayWrite,
        cx: SessionLoop<'_, '_>,
    ) -> Result<(), PeerError> {
        let replies = self.replies;
        let sending = replies.send(batch);
        tokio::pin!(sending);
        loop {
            tokio::select! {
                biased;
                result = &mut sending => {
                    result.map_err(|_| PeerError::Protocol("reply writer closed".into()))?;
                    break;
                }
                stepped = self.step(cx) => {
                    stepped?;
                    // Give the sole writer a turn even when the
                    // peer supplies an always-ready state flood.
                    tokio::task::yield_now().await;
                }
            }
        }
        Ok(())
    }

    /// Until every commit admitted so far is answered and its answer handed
    /// to the writer, meanwhile reading ahead.
    async fn commits_answered(&mut self, cx: SessionLoop<'_, '_>) -> Result<(), PeerError> {
        while !self.commits.is_empty() {
            self.step(cx).await?;
        }
        Ok(())
    }

    /// Until another commit may be admitted ([`PENDING_COMMITS`]).
    async fn until_another_commit_fits(
        &mut self,
        cx: SessionLoop<'_, '_>,
    ) -> Result<(), PeerError> {
        while self.commits.len() >= PENDING_COMMITS {
            self.step(cx).await?;
        }
        Ok(())
    }
}

/// The reader state machine. It borrows the session's reader, its
/// subscriptions and its access audit for as long as it runs; what it owns
/// (the read-ahead backlog) ends with it.
pub(super) struct SessionReaderLoop<'s, 'c, R> {
    inbound: Inbound<'s, 'c, R>,
    subscribed: &'s mut Vec<WorkspaceId>,
    catching_up: bool,
    access_audit: &'s mut AccessAudit,
    authority: &'s mut tokio::sync::watch::Receiver<u64>,
    /// The queues a lagging workspace waits on to drain before it resumes.
    outbound: &'s mpsc::Sender<Envelope>,
    state_outbound: &'s mpsc::Sender<StateDelivery>,
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

impl<'s, 'c, R: tokio::io::AsyncRead + Unpin> SessionReaderLoop<'s, 'c, R> {
    pub(super) fn new(
        reader: &'s mut SessionReader<R>,
        subscribed: &'s mut Vec<WorkspaceId>,
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
                replies,
                commits: Commits {
                    saving: n0_future::FuturesOrdered::new(),
                    answered: VecDeque::new(),
                },
            },
            subscribed,
            catching_up: false,
            access_audit,
            authority,
            outbound,
            state_outbound,
        }
    }

    /// Read and handle frames until the peer's half of the stream ends and
    /// every owner job it admitted is durable, and every commit answered.
    pub(super) async fn run(mut self, cx: SessionLoop<'c, '_>) -> Result<(), PeerError> {
        loop {
            let (frame, inbound_budget) =
                if let Some((frame, budget)) = self.inbound.backlog.pop_front() {
                    (frame, Some(budget))
                } else if !self.inbound.open {
                    self.inbound.commits_answered(cx).await?;
                    return durable_barrier(cx.session, self.subscribed).await;
                } else if let Some(frame) = self.next_frame(cx).await? {
                    (frame, None)
                } else {
                    continue;
                };
            match frame {
                // A heartbeat is answered by the wire itself, between any
                // two frames, whatever the writer waits for (see `wire`).
                EnvelopeV2::State(Envelope::Ping) => cx.heartbeats.owe(),
                EnvelopeV2::State(env) => {
                    let kind = env.kind();
                    let started = tilo_clock::now();
                    self.handle_state_frame(env, inbound_budget, cx).await?;
                    let held = started.elapsed();
                    if held >= SLOW_STATE_FRAME {
                        tracing::info!(
                            kind,
                            device = %hex::encode(&cx.peer.device_public_key[..4]),
                            held_ms = held.as_millis(),
                            backlog = self.inbound.backlog.len(),
                            "a state frame held its connection"
                        );
                    }
                }
                // `SessionReader::read_next` turns these into `State` values
                // before returning.
                EnvelopeV2::UnsignedDelta { .. }
                | EnvelopeV2::PresenceBatch { .. }
                | EnvelopeV2::CodedDelta { .. } => {
                    return Err(PeerError::Protocol(
                        "a coded frame escaped its session reader".into(),
                    ));
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
        let audit_due = self.access_audit.due();
        let settling = !self.inbound.commits.is_empty();
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
            settled = self.inbound.commits.settle(self.inbound.replies), if settling => {
                settled?;
                Ok(None)
            }
            () = cx.session.lag.wake.notified() => { self.catching_up = true; Ok(None) }
            // A lagging workspace resumes once everything already queued
            // for this connection is written, so its replay starts
            // where the wire left off. A replay still being written
            // counts too: resuming under it would replace the session
            // it is about to answer `Ready` for. A commit still being saved
            // counts too: its answer tells the replica it holds the change, so
            // a resume never replays it to it.
            () = tokio::time::sleep(LAG_POLL), if self.catching_up => {
                let replaying = cx.pending_replays.load(std::sync::atomic::Ordering::SeqCst) > 0;
                if replaying || settling || self.state_outbound.capacity() < self.state_outbound.max_capacity() || self.outbound.capacity() < self.outbound.max_capacity() {
                    return Ok(None);
                }
                let resume = lag_resume(cx.session, self.subscribed);
                self.catching_up = !cx.session.lag.is_empty();
                Ok(resume.map(EnvelopeV2::State))
            }
            () = crate::peer::access_audit::until(audit_due) => {
                let watched = audit_subscriber_access(cx.peer, cx.session, self.subscribed).await?;                self.access_audit.audited(cx.peer, cx.session.agents, self.subscribed, watched);
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
        cx: SessionLoop<'c, '_>,
    ) -> Result<(), PeerError> {
        trace_native_publication("server_received", &env, None);
        // A frame that holds its connection for seconds is named in the stall report by
        // what it was: the kind of frame, with the other work outstanding beside it.
        let _doing = tilo_rt::stall::doing(env.kind());
        if matches!(env, Envelope::Commit { .. }) {
            return self.handle_commit(env, inbound_budget, cx).await;
        }
        if subscription_target(&env).is_some()
            && self
                .inbound
                .backlog
                .front()
                .is_some_and(|(frame, _)| matches!(frame, EnvelopeV2::State(next) if subscription_target(next).is_some()))
        {
            return self.handle_subscribe_run(env, inbound_budget, cx).await;
        }
        let (mut prefix, prefix_budgets) = self.inbound.checkpoint_prefix(env, inbound_budget)?;
        let env = prefix.remove(0);
        let replaying = answered_as_replay(&env);
        if let Envelope::Unsubscribe { .. } = &env {
            // A replay handed to the writer still checks its subscription
            // until its Ready; unsubscribing under it would end the whole
            // connection. The reply to an earlier subscribe (one the device
            // sent, or a lagging workspace's resume) comes first, then this;
            // so does the answer to every commit sent before it.
            self.inbound.commits_answered(cx).await?;
            self.inbound
                .overtake_while(Box::pin(cx.replays_written()), cx)
                .await?;
        }
        if replaying {
            cx.pending_replays
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        let mut ticket = None;
        let mut answer = Vec::new();
        let reply = {
            let subscribed = &mut *self.subscribed;
            let ticket = &mut ticket;
            let answer = &mut answer;
            let operation = Box::pin(async move {
                if prefix.is_empty() {
                    Box::pin(handle_envelope(
                        cx.peer, env, cx.session, subscribed, ticket, answer,
                    ))
                    .await
                } else {
                    let mut envelopes = Vec::with_capacity(prefix.len().saturating_add(1));
                    envelopes.push(env);
                    envelopes.extend(prefix);
                    handle_delta_prefix(cx.peer, cx.session, subscribed, envelopes, prefix_budgets)
                        .await
                }
            });
            self.inbound.overtake_while(operation, cx).await?
        };
        trace_buffered_prefix(&self.inbound.backlog);
        // A session with no workspace here does not stay open for a device
        // the server does not admit: the refusal is written, and the session
        // ends with it.
        let ended = self.subscribed.is_empty() && refused_not_admitted(reply.as_ref());
        if replaying || reply.is_some() || ticket.is_some() {
            self.inbound
                .hand_off_reply(
                    ReplayWrite {
                        ticket,
                        answer,
                        reply,
                        replaying,
                    },
                    cx,
                )
                .await?;
        }
        if ended {
            return Err(PeerError::NotAdmitted);
        }
        Ok(())
    }

    /// Handle `env`, a `Subscribe`, with the subscriptions to other
    /// workspaces read ahead behind it: a device entering many workspaces at
    /// once (a backend working in a workspace per customer) waits for the
    /// slowest admission and capture, not for their sum. Each replay goes to
    /// the writer as soon as its capture is ready; what the device sent after
    /// the run is handled after it, so nothing overtakes a subscription to
    /// its workspace.
    async fn handle_subscribe_run(
        &mut self,
        env: Envelope,
        inbound_budget: Option<Budget>,
        cx: SessionLoop<'_, '_>,
    ) -> Result<(), PeerError> {
        use n0_future::StreamExt as _;
        let mut run = vec![env];
        let mut budgets = vec![inbound_budget];
        while run.len() < SUBSCRIBE_RUN {
            let Some((EnvelopeV2::State(next), _)) = self.inbound.backlog.front() else {
                break;
            };
            let Some(target) = subscription_target(next) else {
                break;
            };
            if run
                .iter()
                .any(|env| subscription_target(env) == Some(target))
            {
                break;
            }
            let Some((EnvelopeV2::State(next), budget)) = self.inbound.backlog.pop_front() else {
                break;
            };
            trace_native_publication("server_received", &next, None);
            run.push(next);
            budgets.push(Some(budget));
        }
        cx.pending_replays
            .fetch_add(run.len(), std::sync::atomic::Ordering::SeqCst);
        let mut entered = Vec::with_capacity(run.len());
        let idle = self.subscribed.is_empty();
        let mut refused_the_session = false;
        let outcome = {
            let subscribed = &*self.subscribed;
            let mut running: n0_future::FuturesUnordered<_> = run
                .into_iter()
                .filter_map(|env| {
                    let (workspace_id, resume) = match env {
                        Envelope::Subscribe { workspace_id } => (workspace_id, Subscription::Whole),
                        Envelope::SubscribeFrom {
                            workspace_id,
                            position,
                        } => (workspace_id, Subscription::From(position)),
                        _ => return None,
                    };
                    Some(async move {
                        subscribe(cx.peer, cx.session, subscribed, workspace_id, resume).await
                    })
                })
                .collect();
            loop {
                let next = Box::pin(async { Ok(running.next().await) });
                let Some(subscribed) = self.inbound.overtake_while(next, cx).await? else {
                    break Ok(());
                };
                let (ticket, reply) = match subscribed {
                    Ok(Subscribed::Replaying(slot)) => {
                        entered.push(slot.0);
                        (Some(slot), None)
                    }
                    Ok(Subscribed::Refused(refusal)) => {
                        refused_the_session |= idle && refused_not_admitted(Some(&refusal));
                        (None, Some(refusal))
                    }
                    Err(error) => {
                        // The session ends; what the rest of the run
                        // entered meanwhile is still left with it.
                        while let Some(rest) = running.next().await {
                            if let Ok(Subscribed::Replaying(slot)) = rest {
                                entered.push(slot.0);
                            }
                        }
                        break Err(error);
                    }
                };
                let batch = ReplayWrite {
                    ticket,
                    answer: Vec::new(),
                    reply,
                    replaying: true,
                };
                if let Err(error) = self.inbound.hand_off_reply(batch, cx).await {
                    break Err(error);
                }
            }
        };
        drop(budgets);
        // Every workspace the run entered is the connection's, even when
        // another of the run ended it: its teardown leaves them all.
        for workspace_id in entered {
            if !self.subscribed.contains(&workspace_id) {
                self.subscribed.push(workspace_id);
            }
        }
        // A batch of subscriptions that only refused a device the server does
        // not admit ends the session with that refusal; one that entered a
        // workspace leaves it.
        match outcome {
            Ok(()) if refused_the_session && self.subscribed.is_empty() => {
                Err(PeerError::NotAdmitted)
            }
            outcome => outcome,
        }
    }

    /// Handle `env`, a `Commit`: admit it, in the order sent, and leave its
    /// answer to settle once it is saved ([`Commits`]), while the reader goes
    /// on with what was sent after it. Commits admitted back to back find
    /// each other queued with their workspace's owner, which makes them
    /// durable under shared barriers.
    async fn handle_commit(
        &mut self,
        env: Envelope,
        inbound_budget: Option<Budget>,
        cx: SessionLoop<'c, '_>,
    ) -> Result<(), PeerError> {
        self.inbound.until_another_commit_fits(cx).await?;
        let admitting = Box::pin(admit_commit(cx.peer, cx.session, self.subscribed, env));
        let admission = self.inbound.overtake_while(admitting, cx).await?;
        let session = cx.session;
        self.inbound.commits.saving.push_back(Box::pin(async move {
            let answer = admission.answer(session).await;
            // The frame counts against the delivery budget until it is
            // answered.
            drop(inbound_budget);
            answer
        }));
        Ok(())
    }
}
