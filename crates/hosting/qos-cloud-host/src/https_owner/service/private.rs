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
    match commands {
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
                        code: "agent_control_unavailable".into(),
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
    crate::broker::handle_with_policy_time(
        root,
        machine,
        evidence.certificate(),
        evidence.action(),
        &time,
        policy,
    )
    .await
    .map_err(|error| tracing::debug!(%error, "private broker action refused"))
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
}
pub(crate) struct PrivateLane {
    service: CloudPrivateService,
    waiting: Vec<Waiting>,
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
    /// Earliest deadline of a waiting request; it is refused at that time.
    pub(super) fn next_deadline(&self) -> Option<Instant> {
        self.waiting
            .iter()
            .filter_map(|waiting| waiting.request.as_ref().map(BufferedRequest::deadline))
            .min()
    }
    pub(super) fn is_closed(&self) -> bool {
        self.service.is_closed() || self._broker.is_finished()
    }
    pub(super) fn handle(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        registry: &mut Registries,
        pins: &Pins,
        request: BufferedRequest,
    ) {
        if self.is_closed() {
            return refuse(request);
        }
        let result = match request.route() {
            Route::PrivatePoll => self.poll(source, pool, registry, pins, request),
            _ => self.submit(source, pool, registry, pins, request),
        };
        if let Err(error) = result {
            tracing::debug!(%error, "private request refused");
        }
    }
    fn submit(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        registry: &mut Registries,
        pins: &Pins,
        request: BufferedRequest,
    ) -> Result<()> {
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
                    .map_err(|error| anyhow!("private submit refused: {error}"))?;
                Ok((session, pending))
            })
        })();
        match submitted {
            Ok((session, pending)) => {
                self.waiting.push(Waiting {
                    session,
                    pending: Some(pending),
                    request: Some(request),
                    unobserved: None,
                    reply: None,
                });
                Ok(())
            }
            Err(error) => {
                refuse(request);
                Err(error)
            }
        }
    }
    /// A ready watched transition answers now; otherwise the poll is idle.
    fn poll(
        &mut self,
        source: &CloudStoreSource,
        pool: &HostBytes,
        registry: &mut Registries,
        pins: &Pins,
        request: BufferedRequest,
    ) -> Result<()> {
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
                let admitted = registry.admit_in(&proof, &exact, &context.verification())?;
                let session = admitted.session().session_id();
                let event = service
                    .poll_events(admitted, turn)
                    .map_err(|error| anyhow!("private poll refused: {error}"))?;
                Ok((session, event))
            })
        })();
        match polled {
            Ok((session, Some((pending, reply)))) => {
                self.waiting.push(Waiting {
                    session,
                    pending: Some(pending),
                    request: Some(request),
                    unobserved: None,
                    reply: Some(reply),
                });
                Ok(())
            }
            Ok((_, None)) => {
                let _ = request.respond(StatusCode::OK, Vec::new());
                Ok(())
            }
            Err(error) => {
                refuse(request);
                Err(error)
            }
        }
    }
    /// Deliver ready replies. Busy retains the original request, pending
    /// custody and reply for the next pump; nothing is disclosed early.
    pub(super) fn pump(&mut self, source: &CloudStoreSource) {
        if let Err(error) = self.service.pump() {
            tracing::debug!(%error, "private watch pump closed");
        }
        let mut kept = Vec::new();
        self.retry = false;
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
                        match source
                            .with_current(|turn| self.service.observe_reply(pending, &reply, turn))
                        {
                            Current::Current(Ok(())) => waiting.reply = Some(reply),
                            // Another Cloud component holds the Store lock:
                            // keep the reply undisclosed until a later pump.
                            Current::Busy => {
                                waiting.unobserved = Some(reply);
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
                Current::Current(Ok(())) => {}
                Current::Current(Err(error)) => {
                    tracing::debug!(%error, "private response refused");
                    if let Some(request) = waiting.request.take() {
                        refuse(request);
                    }
                }
                Current::Busy => {
                    self.retry = true;
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
