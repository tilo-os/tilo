//! Private commands, reads and events over the Cloud Store disclosure fence.
//! Requests enter the existing certified broker queue with the owner's policy
//! time; the one queue consumer answers from the actual Cloud agent owner and
//! collaboration broker. Replies are disclosed only after a fresh Store check.
use super::*;
use crate::{
    CloudAgentCommands,
    https_owner::private::{CloudPendingPrivate, CloudPrivateService},
};
use qos_action_broker::{
    agent_reads::AgentReadCustody,
    peer_actions::{PeerActionQueue, Request as QueueRequest},
};
use qos_relay::peer_actions::{ActionMessage, DirectedAction};
use qos_session_peer::ingress::ResponseCredits;
use std::{path::PathBuf, sync::Arc};

/// Active agent invocations whose later durable transitions are published to
/// the queue's transition stream (and from there to watching sessions).
const WATCHED: usize = 32;
/// A watch outlives no session: sessions last at most 15 minutes.
const WATCH_TTL: Duration = Duration::from_secs(15 * 60);
/// Fallback observation interval between observed Store commits.
const OBSERVE_TICK: Duration = Duration::from_secs(2);

pub(in crate::https_owner) struct Watched {
    pub(in crate::https_owner) evidence: qos_relay::client::CertifiedAction,
    pub(in crate::https_owner) invocation: qos_actions::InvocationId,
    pub(in crate::https_owner) last: ActionMessage,
    pub(in crate::https_owner) until: Instant,
}
type Observation<'a> = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = std::result::Result<Option<ActionMessage>, String>>
            + Send
            + 'a,
    >,
>;
/// Fresh durable status of the invocation an original certified action names.
pub(in crate::https_owner) trait ObserveInvocation:
    Send + Sync + 'static
{
    fn observe(
        &self,
        evidence: qos_relay::client::CertifiedAction,
        invocation: qos_actions::InvocationId,
        deadline: Instant,
    ) -> Observation<'_>;
}
impl ObserveInvocation for CloudAgentCommands {
    fn observe(
        &self,
        evidence: qos_relay::client::CertifiedAction,
        invocation: qos_actions::InvocationId,
        deadline: Instant,
    ) -> Observation<'_> {
        Box::pin(self.observe_certified(evidence, invocation, deadline))
    }
}
/// A non-terminal, certain observation: the only kind a session watches.
fn active(message: &ActionMessage) -> Option<qos_actions::InvocationId> {
    match message {
        ActionMessage::Observation {
            invocation,
            update: Some(update),
            uncertain: false,
            ..
        } if !update.is_terminal() => Some(*invocation),
        _ => None,
    }
}
/// Commit notifications for the transition observer, and the service wake-up
/// the broker side raises whenever a reply or transition becomes available.
pub(crate) struct PrivateObserver {
    commits: tokio::sync::watch::Sender<u64>,
    task: Option<tokio::task::JoinHandle<()>>,
    wake: Arc<tokio::sync::Notify>,
}
/// Wakes the service when dropped: after the owning broker task sent its
/// reply, or when it was aborted or panicked without one (the pending reply
/// then reports closed). Either way the waiting request is ready to pump.
struct WakeOnDrop(Arc<tokio::sync::Notify>);
impl Drop for WakeOnDrop {
    fn drop(&mut self) {
        self.0.notify_one();
    }
}
impl PrivateObserver {
    fn notify_commit(&self) {
        self.commits
            .send_modify(|count| *count = count.wrapping_add(1));
    }
    fn abort(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

/// A private carrier refusal keeps its retryable cause: an unavailable carrier
/// or a full, closed or late broker queue answers 503 (see `transient`);
/// wire and policy rejections stay a closed 403.
fn private_refusal(
    what: &'static str,
    error: &qos_session_peer::ingress::PrivateError,
) -> anyhow::Error {
    use qos_action_broker::peer_actions::CarrierError;
    use qos_session_peer::ingress::PrivateError;
    match error {
        PrivateError::Broker(CarrierError::Capacity) => {
            anyhow::Error::new(CloudDeliveryError::Capacity).context(format!("{what}: {error}"))
        }
        PrivateError::Unavailable
        | PrivateError::Broker(CarrierError::Closed | CarrierError::Deadline) => {
            anyhow::Error::new(CloudDeliveryError::Unavailable).context(format!("{what}: {error}"))
        }
        _ => anyhow!("{what}: {error}"),
    }
}
/// A fixed, content-free category for the refusal log.
fn private_reason(error: &anyhow::Error) -> &'static str {
    match error
        .chain()
        .find_map(|cause| cause.downcast_ref::<CloudDeliveryError>())
    {
        Some(CloudDeliveryError::Capacity) => "private_capacity",
        Some(CloudDeliveryError::Unavailable) => "private_unavailable",
        _ if error
            .chain()
            .any(|cause| cause.to_string().contains("unknown session")) =>
        {
            "unknown_session"
        }
        _ => "private_refused",
    }
}
/// Consume the private queue with the actual agent owner, then the existing
/// Store-backed collaboration broker. Active agent invocations answered here
/// are observed again after each Store commit; every changed durable status
/// is published as a streaming transition, which watching sessions receive on
/// `/private/events` after the carrier's fresh authorization and disclosure
/// checks. Status remains the recovery path for any lost event.
pub(crate) fn spawn_private_broker(
    root: PathBuf,
    machine: String,
    commands: Option<CloudAgentCommands>,
    policy: Arc<crate::agent_owner::RuntimePolicyClock>,
) -> (
    PeerActionQueue,
    tokio::task::JoinHandle<()>,
    PrivateObserver,
) {
    let (queue, mut receiver) = PeerActionQueue::channel();
    let (register, registrations) = tokio::sync::mpsc::channel::<Watched>(WATCHED);
    let (commits, committed) = tokio::sync::watch::channel(0u64);
    let wake = Arc::new(tokio::sync::Notify::new());
    let observer = commands.clone().map(|commands| {
        let wake = wake.clone();
        tokio::spawn(observe_transitions(
            commands,
            queue.clone(),
            registrations,
            committed,
            move || wake.notify_one(),
        ))
    });
    let answered = wake.clone();
    let task = tokio::spawn(async move {
        let mut work = tokio::task::JoinSet::new();
        loop {
            tokio::select! {
                request = receiver.recv() => {
                    let Some(request) = request else { break };
                    let root = root.clone();
                    let machine = machine.clone();
                    let commands = commands.clone();
                    let policy = policy.clone();
                    let register = register.clone();
                    let woken = WakeOnDrop(answered.clone());
                    work.spawn(async move {
                        let _woken = woken;
                        answer(request, &root, &machine, commands.as_ref(), policy, &register).await
                    });
                }
                Some(_) = work.join_next(), if !work.is_empty() => {}
            }
        }
        work.abort_all();
        while work.join_next().await.is_some() {}
    });
    (
        queue,
        task,
        PrivateObserver {
            commits,
            task: observer,
            wake,
        },
    )
}
/// Re-observe watched invocations after commits (and on a slow tick); publish
/// each changed certain status, and forget terminal/uncertain/expired ones.
pub(in crate::https_owner) async fn observe_transitions<O: ObserveInvocation>(
    commands: O,
    queue: PeerActionQueue,
    mut registrations: tokio::sync::mpsc::Receiver<Watched>,
    mut committed: tokio::sync::watch::Receiver<u64>,
    published: impl Fn() + Send + 'static,
) {
    let mut watched: Vec<Watched> = Vec::new();
    let mut tick = tokio::time::interval(OBSERVE_TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            registration = registrations.recv() => {
                let Some(registration) = registration else { break };
                let source = registration.evidence.action().source;
                watched.retain(|w| !(w.invocation == registration.invocation && w.evidence.action().source == source));
                if watched.len() == WATCHED {
                    watched.remove(0);
                }
                watched.push(registration);
                continue;
            }
            changed = committed.changed() => if changed.is_err() { break },
            _ = tick.tick() => {}
        }
        let now = Instant::now();
        let mut kept = Vec::with_capacity(watched.len());
        for mut watch in std::mem::take(&mut watched) {
            if now >= watch.until {
                continue;
            }
            let observed = commands
                .observe(
                    watch.evidence.clone(),
                    watch.invocation,
                    Instant::now() + Duration::from_secs(5),
                )
                .await;
            let Ok(Some(message)) = observed else {
                // Unavailable owner: keep the watch; Status stays authoritative.
                kept.push(watch);
                continue;
            };
            if message != watch.last {
                if let ActionMessage::Observation {
                    update: Some(update),
                    uncertain: false,
                    ..
                } = &message
                {
                    queue.publish(watch.evidence.clone(), update.clone());
                    published();
                }
            }
            if active(&message).is_some() {
                watch.last = message;
                kept.push(watch);
            }
        }
        watched = kept;
    }
}
async fn answer(
    request: QueueRequest,
    root: &std::path::Path,
    machine: &str,
    commands: Option<&CloudAgentCommands>,
    policy: Arc<crate::agent_owner::RuntimePolicyClock>,
    register: &tokio::sync::mpsc::Sender<Watched>,
) {
    let result = answer_inner(&request, root, machine, commands, policy).await;
    if let (Ok(message), Some(evidence)) = (&result, request.evidence.as_ref()) {
        if let Some(invocation) = active(message).filter(|_| request.notification.is_none()) {
            let _ = register.try_send(Watched {
                evidence: evidence.clone(),
                invocation,
                last: message.clone(),
                until: Instant::now() + WATCH_TTL,
            });
        }
    }
    let QueueRequest { reply, _permit, .. } = request;
    // The queue slot is released only after the reply leaves this consumer;
    // the carrier's pending reply retains the same slot through the response.
    let _ = reply.send(result);
    drop(_permit);
}
async fn answer_inner(
    request: &QueueRequest,
    root: &std::path::Path,
    machine: &str,
    commands: Option<&CloudAgentCommands>,
    policy: Arc<crate::agent_owner::RuntimePolicyClock>,
) -> std::result::Result<ActionMessage, ()> {
    let evidence = request.evidence.clone().ok_or(())?;
    // Qualified private paths always carry the owner's registered clock.
    let time = request.policy_time().cloned().ok_or(())?;
    if let Some(invocation) = request.poll {
        // A watch's transition: a fresh authorized observation of the watched
        // invocation, carrying the published durable update (as native).
        let commands = commands.ok_or(())?;
        let mut observation = commands
            .observe_certified(evidence, invocation, request.deadline)
            .await
            .map_err(|error| tracing::debug!(%error, "private watch observation refused"))?
            .ok_or(())?;
        if let Some(update) = request.notification.clone() {
            if update.invocation() != invocation {
                return Err(());
            }
            if let ActionMessage::Observation {
                update: retained @ Some(_),
                uncertain: false,
                ..
            } = &mut observation
            {
                *retained = Some(update);
            }
        }
        return Ok(observation);
    }
    if request.notification.is_some() {
        return Err(());
    }
    let message = &evidence.action().message;
    let desktop = matches!(message, ActionMessage::InvokeBound { app_id, .. }
        if app_id == qos_peer_state::desktop_command::APP_ID);
    let began = std::time::Instant::now();
    match commands {
        // Priority lane: the agent owner answers agent controls, agent reads,
        // the catalogue and retained status/cancel. Any other bound invocation
        // (desktop open/focus/close, collaboration admin) it answers `None`
        // before touching the Store, so it goes straight to the Store-backed
        // broker below and never waits in the owner's 32-slot command queue
        // behind agent reads, the worker pump or the owner tick (nor is it
        // refused when that queue is full). Its exactly-once custody is the
        // broker's Store commit with its invocation receipt, as before.
        Some(_) if ordinary_invoke(message) => {}
        Some(commands) => {
            let custody = AgentReadCustody::from_request(request);
            match commands
                .handle_certified_with_custody(evidence.clone(), request.deadline, custody)
                .await
            {
                Ok(reply) => {
                    let (message, _custody) = reply.into_parts();
                    if let Some(message) = message {
                        return Ok(message);
                    }
                }
                Err(error) => {
                    tracing::debug!(%error, "private agent action rejected or observation unavailable");
                    return Ok(ActionMessage::Rejected {
                        code: agent_rejection_code(&error.to_string()).into(),
                    });
                }
            }
        }
        // No agent execution owner is attached: agent control is unavailable,
        // ordinary application commands still use the Store-backed broker.
        None if is_agent_control(evidence.action()) => {
            return Ok(ActionMessage::Rejected {
                code: "agent_control_unavailable".into(),
            });
        }
        None => {}
    }
    let owner_us = qos_action_broker::agent_timing::us(began);
    let broker = std::time::Instant::now();
    let result = crate::broker::handle_with_policy_time(
        root,
        machine,
        evidence.certificate(),
        evidence.action(),
        &time,
        policy,
    )
    .await
    .map_err(|error| tracing::debug!(%error, "private broker action refused"));
    if desktop {
        // Content-free: opaque invocation, durations and the outcome bit.
        qos_action_broker::agent_timing::span(
            "desktop_command_commit",
            message.invocation().map(|invocation| invocation.0),
            began.elapsed(),
            &[
                ("owner_us", owner_us),
                ("broker_us", qos_action_broker::agent_timing::us(broker)),
                ("ok", u64::from(result.is_ok())),
            ],
        );
    }
    result
}
/// A bound invocation of an ordinary (non-agent) application: never served
/// by the agent owner (see the priority lane in `answer_inner`).
fn ordinary_invoke(message: &ActionMessage) -> bool {
    matches!(message, ActionMessage::InvokeBound { app_id, .. }
        if app_id != qos_action_broker::agent_control::APP_ID)
}

/// Rejection code of a refused private agent control. A stale descriptor or
/// grant offer (the control was composed before the catalogue changed) gets
/// its own code: that invocation can never succeed as sent, so the client
/// refetches the catalogue and sends a new invocation instead of retrying.
/// A Spawn refused because the workspace already holds its live-agent limit
/// (`MAX_LIVE_PRIVATE_AGENTS`) gets `agent_capacity_reached`, so the client
/// can say so instead of reporting changed controls.
pub(crate) fn agent_rejection_code(error: &str) -> &'static str {
    if error.contains("differs from current native descriptor")
        || error.contains("agent grant offer changed")
    {
        "agent_descriptor_changed"
    } else if error.contains(&format!(
        "Capacity {{ limit: {} }}",
        qos_peer_state::owner_commit::MAX_LIVE_PRIVATE_AGENTS
    )) {
        "agent_capacity_reached"
    } else {
        "agent_control_unavailable"
    }
}

fn is_agent_control(action: &DirectedAction) -> bool {
    matches!(&action.message, ActionMessage::AgentReadV1 { .. })
        || matches!(&action.message, ActionMessage::InvokeBound { app_id, .. }
            if app_id == qos_action_broker::agent_control::APP_ID)
}

struct Waiting {
    session: [u8; 16],
    pending: Option<CloudPendingPrivate>,
    request: Option<BufferedRequest>,
    /// Received but not yet observed under a current turn (owner was busy).
    unobserved: Option<DirectedAction>,
    reply: Option<DirectedAction>,
    /// Content-free host latency of this request (see `CommandTiming`).
    timing: CommandTiming,
}
/// Host-side stages of one private request, logged for commands (not reads
/// or event polls) as `private_command`: arrival at the HTTPS ingress to the
/// service actor, to the broker reply and to the disclosed response, and how
/// often a busy Store lock deferred it. Durations and counts only.
#[derive(Clone, Copy)]
struct CommandTiming {
    arrived: Instant,
    queued_us: u64,
    replied_us: Option<u64>,
    busy: u64,
    /// False for an events poll answered with a transition.
    command: bool,
}
/// The ingress deadline of a private request (qos-session-peer `DEADLINE`):
/// its arrival is this long before the deadline it carries.
const INGRESS_DEADLINE: Duration = Duration::from_secs(5);
impl CommandTiming {
    fn submitted(request: &BufferedRequest) -> Self {
        let now = Instant::now();
        let arrived = request
            .deadline()
            .checked_sub(INGRESS_DEADLINE)
            .filter(|arrived| *arrived <= now)
            .unwrap_or(now);
        Self {
            arrived,
            queued_us: now.duration_since(arrived).as_micros() as u64,
            replied_us: None,
            busy: 0,
            command: true,
        }
    }
    fn polled() -> Self {
        Self {
            arrived: Instant::now(),
            queued_us: 0,
            replied_us: None,
            busy: 0,
            command: false,
        }
    }
    fn replied(&mut self) {
        self.replied_us
            .get_or_insert(self.arrived.elapsed().as_micros() as u64);
    }
    fn responded(&self, reply: &DirectedAction) {
        if !self.command || matches!(reply.message, ActionMessage::AgentReadResultV1 { .. }) {
            return;
        }
        qos_action_broker::agent_timing::span(
            "private_command",
            reply.message.invocation().map(|invocation| invocation.0),
            self.arrived.elapsed(),
            &[
                ("queued_us", self.queued_us),
                ("replied_us", self.replied_us.unwrap_or(0)),
                ("busy", self.busy),
            ],
        );
    }
}
/// An admitted events poll with nothing ready, parked on its continuation
/// until an event, cancellation, session retirement or its idle deadline
/// (fix plan B6: long-poll instead of an immediate empty answer).
struct HeldPoll {
    session: [u8; 16],
    pending: qos_session_peer::ingress::PendingCarrier,
}
/// A held events poll answers idle this long before its original deadline.
const POLL_IDLE_MARGIN: Duration = Duration::from_millis(100);
/// Held events polls at once (one per session); beyond it a poll answers idle.
const MAX_HELD_POLLS: usize = 16;
pub(crate) struct PrivateLane {
    service: CloudPrivateService,
    waiting: Vec<Waiting>,
    polls: Vec<HeldPoll>,
    _broker: tokio::task::JoinHandle<()>,
    observer: PrivateObserver,
    /// The last pump kept work only because another Cloud component held the
    /// Store lock: nothing signals its release, so the service retries.
    retry: bool,
}
impl PrivateLane {
    /// Bind the carrier to the actual source's room under a current turn.
    pub(crate) fn install(
        source: &CloudStoreSource,
        queue: &PeerActionQueue,
        broker: tokio::task::JoinHandle<()>,
        observer: PrivateObserver,
    ) -> Result<Self> {
        let service = loop {
            match CloudPrivateService::new_unqualified(source, queue) {
                Current::Current(result) => {
                    break result.map_err(|error| anyhow!("private carrier: {error}"))?;
                }
                Current::Denied => bail!("Cloud owner denied"),
                Current::Busy => std::thread::sleep(BUSY_RETRY),
            }
        };
        Ok(Self {
            service,
            waiting: Vec::new(),
            polls: Vec::new(),
            _broker: broker,
            observer,
            retry: false,
        })
    }
    pub(super) fn busy(&self) -> bool {
        !self.waiting.is_empty()
    }
    /// Raised when a broker reply or a watched transition becomes available.
    pub(super) fn wake(&self) -> Arc<tokio::sync::Notify> {
        self.observer.wake.clone()
    }
    /// Whether waiting work needs a timed retry (Store lock contention).
    pub(super) fn needs_retry(&self) -> bool {
        self.retry
    }
    /// Earliest deadline of a waiting request (refused at that time) or of a
    /// held events poll (answered idle shortly before it).
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        let polls = self.polls.iter().map(|held| {
            let deadline = held.pending.original().deadline();
            deadline.checked_sub(POLL_IDLE_MARGIN).unwrap_or(deadline)
        });
        self.waiting
            .iter()
            .filter_map(|waiting| waiting.request.as_ref().map(BufferedRequest::deadline))
            .chain(polls)
            .min()
    }
    pub(super) fn is_closed(&self) -> bool {
        self.service.is_closed() || self._broker.is_finished()
    }
    /// Returns the untouched request when the Store was busy before its
    /// admission (the service defers and retries it).
    pub(super) fn handle(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        registry: &mut Registries,
        pins: &Pins,
        request: BufferedRequest,
    ) -> Option<BufferedRequest> {
        if self.is_closed() {
            // The private lane is gone (its broker task ended): retryable 503.
            let _ = request.respond(
                StatusCode::SERVICE_UNAVAILABLE,
                b"temporarily-unavailable".to_vec(),
            );
            if let Some(suppressed) = ops_log::admit("private_closed") {
                tracing::info!(
                    event = "cloud_private_refused",
                    reason = "private_closed",
                    suppressed,
                    "Cloud private request refused"
                );
            }
            return None;
        }
        let result = match request.route() {
            Route::PrivatePoll => self.poll(source, pool, registry, pins, request),
            _ => self.submit(source, pool, registry, pins, request),
        };
        match result {
            Ok(deferred) => deferred,
            Err(error) => {
                // Content-free: fixed refusal categories only, rate limited.
                let reason = private_reason(&error);
                if let Some(suppressed) = ops_log::admit(reason) {
                    tracing::info!(
                        event = "cloud_private_refused",
                        reason,
                        suppressed,
                        "Cloud private request refused"
                    );
                }
                tracing::debug!(%error, "private request refused");
                None
            }
        }
    }
    fn submit(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        registry: &mut Registries,
        pins: &Pins,
        request: BufferedRequest,
    ) -> Result<Option<BufferedRequest>> {
        let service = &mut self.service;
        let submitted = (|| {
            let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
            let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
            with_turn(source, pool, DeliveryOperation::Replay, |turn| {
                let intent = registry.session_intent(proof.session_id())?;
                let context = turn.carrier_context(
                    pins,
                    intent.device(),
                    intent.requested(),
                    intent.mandatory(),
                )?;
                let admitted = registry.admit_in(&proof, &exact, &context.verification())?;
                let session = admitted.session().session_id();
                let pending = service
                    .submit(admitted, turn, request.deadline())
                    .map_err(|error| private_refusal("private submit refused", &error))?;
                Ok((session, pending))
            })
        })();
        match submitted {
            Ok((session, pending)) => {
                self.waiting.push(Waiting {
                    session,
                    pending: Some(pending),
                    timing: CommandTiming::submitted(&request),
                    request: Some(request),
                    unobserved: None,
                    reply: None,
                });
                Ok(None)
            }
            Err(error) if deferrable(&error) => Ok(Some(request)),
            Err(error) => {
                // A full or briefly closed broker queue is a retryable 503,
                // never an authorization refusal (a 403 here made clients
                // re-enroll and re-challenge membership in a loop).
                refuse_error(request, &error);
                Err(error)
            }
        }
    }
    /// A ready watched transition answers now; otherwise the admitted poll
    /// is held until an event or its idle deadline.
    fn poll(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        registry: &mut Registries,
        pins: &Pins,
        request: BufferedRequest,
    ) -> Result<Option<BufferedRequest>> {
        let service = &mut self.service;
        let polled = (|| {
            let proof = RequestProof::decode(request.proof().context("missing proof")?)?;
            let exact = Request::new(request.method().as_str(), request.target(), request.body())?;
            with_turn(source, pool, DeliveryOperation::Replay, |turn| {
                let intent = registry.session_intent(proof.session_id())?;
                let context = turn.carrier_context(
                    pins,
                    intent.device(),
                    intent.requested(),
                    intent.mandatory(),
                )?;
                let (admitted, ticket) = registry.admit_buffered_in(
                    &proof,
                    &exact,
                    &request,
                    &context.verification(),
                )?;
                let session = admitted.session().session_id();
                let event = service
                    .poll_events(admitted, turn)
                    .map_err(|error| private_refusal("private poll refused", &error))?;
                Ok((session, event, ticket))
            })
        })();
        match polled {
            Ok((session, Some((pending, reply)), _)) => {
                self.waiting.push(Waiting {
                    session,
                    pending: Some(pending),
                    request: Some(request),
                    unobserved: None,
                    reply: Some(reply),
                    timing: CommandTiming::polled(),
                });
                Ok(None)
            }
            Ok((session, None, ticket)) => {
                self.polls
                    .retain(|held| !held.pending.original().is_cancelled());
                let idle = request
                    .deadline()
                    .checked_sub(POLL_IDLE_MARGIN)
                    .is_none_or(|idle| Instant::now() >= idle);
                if idle
                    || self.polls.len() >= MAX_HELD_POLLS
                    || self.polls.iter().any(|held| held.session == session)
                {
                    let _ = request.respond(StatusCode::OK, Vec::new());
                    return Ok(None);
                }
                // Park re-checks the exact original bytes, proof, deadline
                // and channel; a replaced original is rejected by dropping it.
                if let Ok(pending) = ticket.park(request) {
                    self.polls.push(HeldPoll { session, pending });
                }
                Ok(None)
            }
            Err(error) if deferrable(&error) => Ok(Some(request)),
            Err(error) => {
                refuse_error(request, &error);
                Err(error)
            }
        }
    }
    /// Answer held events polls: an event once one is ready for the session,
    /// idle at the deadline. A busy Store keeps the poll for a later pump.
    fn pump_polls(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        registry: &mut Registries,
        pins: &Pins,
    ) {
        let now = Instant::now();
        let ready = self.service.has_ready_event();
        for held in std::mem::take(&mut self.polls) {
            if held.pending.original().is_cancelled() {
                continue;
            }
            let deadline = held.pending.original().deadline();
            let before_idle = deadline
                .checked_sub(POLL_IDLE_MARGIN)
                .is_some_and(|idle| now < idle);
            if !ready {
                if before_idle {
                    self.polls.push(held);
                } else {
                    let _ = held
                        .pending
                        .into_request()
                        .respond(StatusCode::OK, Vec::new());
                }
                continue;
            }
            let service = &mut self.service;
            let resumed = with_turn(source, pool, DeliveryOperation::Replay, |turn| {
                let intent = registry.session_intent(held.pending.session_id())?;
                let context = turn.carrier_context(
                    pins,
                    intent.device(),
                    intent.requested(),
                    intent.mandatory(),
                )?;
                let session =
                    registry.resume_buffered_in(&held.pending, &context.verification())?;
                service
                    .poll_events_resumed(session, held.pending.original(), turn)
                    .map_err(|error| anyhow!("private poll refused: {error}"))
            });
            match resumed {
                Ok(Some((pending, reply))) => self.waiting.push(Waiting {
                    session: held.session,
                    pending: Some(pending),
                    request: Some(held.pending.into_request()),
                    unobserved: None,
                    reply: Some(reply),
                    timing: CommandTiming::polled(),
                }),
                Ok(None) if before_idle => self.polls.push(held),
                Err(error) if before_idle && deferrable(&error) => {
                    self.retry = true;
                    self.polls.push(held);
                }
                Ok(None) => {
                    let _ = held
                        .pending
                        .into_request()
                        .respond(StatusCode::OK, Vec::new());
                }
                Err(error) if deferrable(&error) => {
                    let _ = held
                        .pending
                        .into_request()
                        .respond(StatusCode::OK, Vec::new());
                }
                Err(error) => {
                    tracing::debug!(%error, "held private poll refused");
                    refuse(held.pending.into_request());
                }
            }
        }
    }
    /// Deliver ready replies. Busy retains the original request, pending
    /// custody and reply for the next pump; nothing is disclosed early.
    pub(super) fn pump(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        registry: &mut Registries,
        pins: &Pins,
    ) {
        if let Err(error) = self.service.pump() {
            tracing::debug!(%error, "private watch pump closed");
        }
        self.retry = false;
        // First: a held poll answered with an event joins `waiting` and is
        // disclosed by the loop below in this same pump.
        self.pump_polls(source, pool, registry, pins);
        let mut kept = Vec::new();
        for mut waiting in std::mem::take(&mut self.waiting) {
            let expired = waiting.request.as_ref().is_none_or(|request| {
                request.is_cancelled() || Instant::now() >= request.deadline()
            });
            if expired {
                if let Some(request) = waiting.request.take() {
                    refuse(request);
                }
                continue;
            }
            if waiting.reply.is_none() {
                let pending = waiting.pending.as_mut().expect("retained pending");
                let received = match waiting.unobserved.take() {
                    Some(reply) => Ok(Some(reply)),
                    None => pending.try_reply(),
                };
                match received {
                    Ok(Some(reply)) => {
                        waiting.timing.replied();
                        match source
                            .with_current(|turn| self.service.observe_reply(pending, &reply, turn))
                        {
                            Current::Current(Ok(())) => waiting.reply = Some(reply),
                            // Another Cloud component holds the Store lock:
                            // keep the reply undisclosed until a later pump.
                            Current::Busy => {
                                waiting.unobserved = Some(reply);
                                waiting.timing.busy += 1;
                                self.retry = true;
                                kept.push(waiting);
                                continue;
                            }
                            Current::Current(Err(error)) => {
                                tracing::debug!(%error, "private reply observation refused");
                                if let Some(request) = waiting.request.take() {
                                    refuse(request);
                                }
                                continue;
                            }
                            Current::Denied => {
                                if let Some(request) = waiting.request.take() {
                                    refuse(request);
                                }
                                continue;
                            }
                        }
                    }
                    Ok(None) => {
                        kept.push(waiting);
                        continue;
                    }
                    Err(error) => {
                        tracing::debug!(%error, "private reply unavailable");
                        if let Some(request) = waiting.request.take() {
                            refuse(request);
                        }
                        continue;
                    }
                }
            }
            let reply = waiting.reply.clone().expect("ready reply");
            let mut credits = Some(ResponseCredits::default());
            match CloudPendingPrivate::respond(
                &mut waiting.pending,
                &reply,
                &mut waiting.request,
                &mut credits,
            ) {
                Current::Current(Ok(())) => waiting.timing.responded(&reply),
                Current::Current(Err(error)) => {
                    tracing::debug!(%error, "private response refused");
                    if let Some(request) = waiting.request.take() {
                        refuse(request);
                    }
                }
                Current::Busy => {
                    self.retry = true;
                    waiting.timing.busy += 1;
                    kept.push(waiting);
                }
                Current::Denied => {
                    if let Some(request) = waiting.request.take() {
                        refuse(request);
                    }
                }
            }
        }
        self.waiting = kept;
    }
    pub(super) fn retire_session(&mut self, id: [u8; 16]) {
        for held in std::mem::take(&mut self.polls) {
            if held.session == id {
                refuse(held.pending.into_request());
            } else {
                self.polls.push(held);
            }
        }
        self.service.retire_session(id);
        for mut waiting in std::mem::take(&mut self.waiting) {
            if waiting.session == id {
                if let Some(request) = waiting.request.take() {
                    refuse(request);
                }
            } else {
                self.waiting.push(waiting);
            }
        }
    }
    pub(super) fn refuse_all(&mut self) {
        for held in std::mem::take(&mut self.polls) {
            let _ = held
                .pending
                .into_request()
                .respond(StatusCode::OK, Vec::new());
        }
        for mut waiting in std::mem::take(&mut self.waiting) {
            self.service.retire_session(waiting.session);
            if let Some(request) = waiting.request.take() {
                refuse(request);
            }
        }
    }
    /// A Store commit may have changed a watched invocation's durable status.
    pub(super) fn notify_commit(&self) {
        self.observer.notify_commit();
    }
    pub(super) fn close(&mut self) {
        self.refuse_all();
        self.service.close();
        self._broker.abort();
        self.observer.abort();
    }
}

#[cfg(test)]
mod rejection_code_tests {
    #[test]
    fn stale_agent_descriptor_or_offer_has_its_own_rejection_code() {
        use super::agent_rejection_code as code;
        assert_eq!(
            code("agent invocation differs from current native descriptor"),
            "agent_descriptor_changed"
        );
        assert_eq!(
            code("agent grant offer changed"),
            "agent_descriptor_changed"
        );
        assert_eq!(
            code("agent mutation services unavailable"),
            "agent_control_unavailable"
        );
        // The Store's live-agent refusal, as the admission path renders it.
        let full = qos_peer_state::owner_commit::AgentCommitError::Capacity {
            limit: qos_peer_state::owner_commit::MAX_LIVE_PRIVATE_AGENTS,
        };
        assert_eq!(code(&full.to_string()), "agent_capacity_reached");
        // The total admission ledger bound is not the live-agent limit.
        let ledger = qos_peer_state::owner_commit::AgentCommitError::Capacity {
            limit: qos_peer_state::owner_commit::MAX_PRIVATE_AGENT_ADMISSIONS,
        };
        assert_eq!(code(&ledger.to_string()), "agent_control_unavailable");
    }
}

#[cfg(test)]
mod priority_lane_tests {
    use super::*;
    use crate::{Store, agent_runtime::CloudAgentRuntime};

    /// A desktop command reaches its durable commit and reply while every
    /// agent-owner command slot is taken by queued agent work: it never
    /// enters that queue (before the priority lane it was refused there).
    #[tokio::test]
    async fn desktop_command_commits_while_agent_owner_queue_is_full() {
        let (dir, owner) = crate::agent_workers::tests::selected();
        drop(owner);
        let store = Store::open(dir.path(), "agent-admission").unwrap();
        let room: uuid::Uuid = store.state.room.parse().unwrap();
        let invocation = qos_actions::InvocationId::new();
        let evidence = crate::agent_runtime::read_attachment::tests::certified(
            &store,
            ActionMessage::InvokeBound {
                host: room,
                invocation,
                app_peer: qos_relay::room_workspace_id(&store.state.room)
                    .unwrap()
                    .to_string(),
                app_id: qos_peer_state::desktop_command::APP_ID.into(),
                record_id: String::new(),
                resource: format!(
                    "{}so.tilo.notes",
                    qos_agent_control_protocol::OPEN_APP_RESOURCE_PREFIX
                ),
                action: qos_agent_control_protocol::OPEN_APP_ACTION.into(),
                argument: None,
                confirmation: None,
                revision: None,
            },
        );
        let generation = store.state.generation;
        drop(store);
        let mut runtime = CloudAgentRuntime::start(dir.path(), "agent-admission", true)
            .await
            .unwrap();
        let commands = runtime.certified_commands();
        let policy = commands.policy_context();
        let time = policy.capture().unwrap();
        let (queue, broker, _observer) = spawn_private_broker(
            dir.path().to_path_buf(),
            "agent-admission".into(),
            Some(commands),
            policy,
        );
        let queued = runtime.occupy_command_queue();
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut pending = queue
            .submit_certified_with_policy_time(evidence, deadline, time)
            .unwrap();
        let reply = loop {
            match pending.try_reply().unwrap() {
                Some(reply) => break reply,
                None => tokio::time::sleep(Duration::from_millis(5)).await,
            }
        };
        assert!(
            matches!(
                &reply.message,
                ActionMessage::Observation {
                    invocation: actual,
                    update: Some(qos_actions::InvocationUpdate::Succeeded { .. }),
                    ..
                } if *actual == invocation
            ),
            "{:?}",
            reply.message
        );
        drop(queued);
        broker.abort();
        runtime.close().await.unwrap();
        assert!(
            Store::open(dir.path(), "agent-admission")
                .unwrap()
                .state
                .generation
                > generation,
            "the desktop command was durably committed"
        );
    }
}

#[cfg(test)]
mod refusal_tests {
    use super::*;
    use qos_action_broker::peer_actions::CarrierError;
    use qos_session_peer::ingress::PrivateError;

    /// A full, closed or late broker queue (many agents' reads and commands
    /// in flight) is a retryable 503, never the 403 that sent clients into a
    /// re-enrollment and membership re-challenge loop.
    #[test]
    fn broker_capacity_and_closure_are_transient_and_wire_rejections_stay_closed() {
        for error in [
            PrivateError::Broker(CarrierError::Capacity),
            PrivateError::Broker(CarrierError::Closed),
            PrivateError::Broker(CarrierError::Deadline),
            PrivateError::Unavailable,
        ] {
            let refusal = private_refusal("private submit refused", &error);
            assert!(transient(&refusal), "{error} must answer 503");
            assert!(!deferrable(&refusal), "{error} is answered, not held");
            assert!(private_reason(&refusal).starts_with("private_"));
        }
        let rejected = private_refusal(
            "private submit refused",
            &PrivateError::Broker(CarrierError::Rejected),
        );
        assert!(!transient(&rejected), "a rejection stays a closed 403");
        assert_eq!(private_reason(&rejected), "private_refused");
    }
}
