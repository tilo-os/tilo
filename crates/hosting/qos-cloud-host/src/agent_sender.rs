//! Cloud managed sender for enrolled task children.
//!
//! One installed [`SenderRuntime`] over the Cloud's own private sender file
//! (`actions/sender`, Cloud profile scope). Every child of an enrolled task is
//! a durable outbox row *and* a durable parent step (`TaskStepBound`) before
//! any attempt leaves; its exact terminal result and the next-step
//! continuation are committed by the task's parent (`TaskStepCommitted`)
//! before the row is released. Attempts go to the host's own broker through
//! the host-local transport and its task-owned durable child path, so a lost
//! reply or restart observes the original UUID instead of re-executing.
use crate::agent_runtime_adapter::RuntimeOwner;
use automerge::resource_limits::NativeAllowance;
use qos_action_broker::{
    collab_broker::AppActionBroker,
    collab_broker_capnp::{EnrolledChildOwner, EnrolledChildRequest},
    enrolled_dispatch::{
        ChildTicket, ParentOwner, ParentReceipt, ParentStep, VerifiedEnrolledDispatch,
    },
};
use std::{path::Path, sync::Arc, time::Instant};
use ui::{
    host_worker::{
        managed_sender::{
            AttemptKind, ChildIntent, LocalFailure, LocalRequest, ManagedAttempt, Recovered, Reply,
            SenderCaller, SenderError, SenderRuntime, StepStart,
        },
        private_actions::{
            PrivateActionsFile, PrivateReadOutcome, SenderScope,
            administration::{AdministrativeExpectation, AdministrativeFile},
        },
    },
    portable_storage::PortableStorage,
};

const SENDER_DIRECTORY: &str = "actions/sender";
const SENDER_APP: &str = "so.tilo.agent";
const WORK_BYTES: usize = 32 * 1024 * 1024;

/// The parent's next-step continuation, derived only from the exact
/// committed child observation (never an in-memory copy of a reply).
pub(crate) fn continuation(_: ParentStep, json: &str) -> Result<Vec<u8>, String> {
    let update: qos_actions::InvocationUpdate =
        serde_json::from_str(json).map_err(|e| e.to_string())?;
    let value = match update {
        qos_actions::InvocationUpdate::Succeeded { output, .. } => {
            serde_json::json!({ "succeeded": output })
        }
        qos_actions::InvocationUpdate::Failed { error, .. } => {
            serde_json::json!({ "failed": error.code })
        }
        qos_actions::InvocationUpdate::Cancelled { .. } => serde_json::json!({ "cancelled": true }),
        _ => return Err("child observation is not terminal".into()),
    };
    serde_json::to_vec(&value).map_err(|e| e.to_string())
}

fn storage(code: impl std::fmt::Debug) -> String {
    format!("cloud sender storage: {code:?}")
}

/// Outcome of one progressed step.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum StepProgress {
    /// The child is durable and mapped; its terminal result is not yet known.
    Pending(ChildTicket),
    /// The parent committed the exact result and continuation; the child
    /// obligation is released.
    Settled {
        receipt: ParentReceipt,
        continuation: Vec<u8>,
    },
}

pub(crate) struct CloudSender {
    runtime: SenderRuntime,
    work: NativeAllowance,
    _cache: PortableStorage,
}
impl CloudSender {
    /// Open (or, only when no bound child can exist, create) the Cloud's
    /// selected sender file and install the runtime over its hydrated state.
    /// Identity is derived from the Store's private host secret, so a restart
    /// reopens the same instance; a missing file while the task ledger holds
    /// an uncommitted bound step refuses rather than silently dropping it.
    pub(crate) fn install(owner: &RuntimeOwner, root: &Path) -> Result<Self, String> {
        let (room, host_root, device, machine, incarnation, instance, open_step) = {
            let store = owner.store()?;
            let peer = store.peer().map_err(|e| e.to_string())?;
            let authority = peer.authority().clone();
            let seed = store.sender_seed();
            let derive = |label: &str| -> [u8; 16] {
                blake3::derive_key(label, &seed)[..16]
                    .try_into()
                    .expect("16-byte prefix")
            };
            (
                uuid::Uuid::parse_str(peer.room()).map_err(|e| e.to_string())?,
                authority.user_public_key,
                authority.device_public_key,
                store.state.machine_id.clone(),
                derive("qos cloud sender incarnation v1"),
                derive("qos cloud sender instance v1"),
                store.any_open_task_step().map_err(|e| e.to_string())?,
            )
        };
        let scope = || SenderScope::cloud(room, host_root, device, &machine, incarnation);
        let directory = root.join(SENDER_DIRECTORY);
        atomic_file::provision_directory(&directory, atomic_file::Privacy::OwnerOnly)
            .map_err(|e| e.to_string())?;
        let exists = directory
            .join("private-actions-v1")
            .join("private-actions-v1.bin")
            .try_exists()
            .map_err(|e| e.to_string())?;
        let expectation = match (exists, open_step) {
            (true, _) => AdministrativeExpectation::Active,
            (false, false) => AdministrativeExpectation::Prepared,
            (false, true) => {
                return Err(
                    "cloud sender file missing while a bound child step is uncommitted; explicit recovery required"
                        .into(),
                );
            }
        };
        let work = NativeAllowance::new(WORK_BYTES).map_err(storage)?;
        let scoped = scope().map_err(storage)?;
        let credit = work
            .reserve(AdministrativeFile::required_bytes(&scoped).map_err(storage)?)
            .map_err(storage)?;
        let administrative = AdministrativeFile::open(
            atomic_file::StorageDirectory::open(&directory).map_err(|e| e.to_string())?,
            scoped,
            instance,
            expectation,
            credit,
        )
        .map_err(|e| storage(e.code()))?;
        drop(administrative);
        let cache = PortableStorage::new("cloud-agent-sender", &hex::encode(device))?;
        let scoped = scope().map_err(storage)?;
        let credit = work
            .reserve(PrivateActionsFile::required_bytes(&scoped).map_err(storage)?)
            .map_err(storage)?;
        let file = PrivateActionsFile::from_held(
            atomic_file::StorageDirectory::open(&directory).map_err(|e| e.to_string())?,
            scoped,
            &cache,
            credit,
        )
        .map_err(|e| storage(e.code()))?;
        let credit = work
            .reserve(file.read_required_bytes().map_err(storage)?)
            .map_err(storage)?;
        let read = match file.read_existing(credit) {
            PrivateReadOutcome::Committed(read) => read,
            PrivateReadOutcome::Failed(error) => return Err(storage(error.code())),
        };
        let credit = work
            .reserve(read.hydrate_required_bytes().map_err(storage)?)
            .map_err(storage)?;
        let state = read
            .hydrate_current(credit)
            .map_err(|e| storage(e.code()))?;
        let caller = SenderCaller::new(host_root, device, SENDER_APP, "", None).map_err(storage)?;
        let runtime =
            SenderRuntime::install(state, caller, &work).map_err(|e| storage(e.error()))?;
        Ok(Self {
            runtime,
            work,
            _cache: cache,
        })
    }

    /// Exact current target for a child of `dispatch`'s task.
    pub(crate) fn child_intent(
        broker: &AppActionBroker,
        dispatch: &VerifiedEnrolledDispatch,
        peer: u64,
        resource: &str,
        action: &str,
        argument: Option<String>,
    ) -> Result<ChildIntent, String> {
        let (binding, revision) =
            broker.enrolled_child_binding(dispatch, peer, resource, action)?;
        Ok(ChildIntent {
            binding,
            revision,
            resource: resource.into(),
            action: action.into(),
            argument,
        })
    }

    fn deadline() -> Instant {
        Instant::now() + std::time::Duration::from_secs(5)
    }

    fn deliver(
        &mut self,
        attempt: ManagedAttempt,
        broker: &AppActionBroker,
        dispatch: Option<&VerifiedEnrolledDispatch>,
        owner: Option<&EnrolledChildOwner>,
    ) -> Result<Reply, SenderError> {
        let route = self.runtime.route();
        let result = attempt
            .deliver_local(&route, |request| {
                let (host, invocation, result) = match request {
                    LocalRequest::Invoke {
                        invocation,
                        binding,
                        revision,
                        resource,
                        action,
                        argument,
                        ..
                    } => {
                        let dispatch = dispatch.ok_or(LocalFailure::Unavailable)?;
                        let request = EnrolledChildRequest {
                            invocation: *invocation,
                            binding: binding.clone(),
                            revision: revision.into(),
                            resource: resource.into(),
                            action: action.into(),
                            argument: argument.map(str::to_owned),
                        };
                        let result =
                            broker
                                .invoke_enrolled_child(dispatch, &request)
                                .map_err(|error| {
                                    tracing::debug!(%error, "enrolled child invoke refused");
                                    LocalFailure::Unavailable
                                })?;
                        (binding.host, *invocation, result)
                    }
                    LocalRequest::Observe { host, invocation } => {
                        let owner = owner.ok_or(LocalFailure::Unavailable)?;
                        let result = broker
                            .observe_enrolled_child(owner, *host, *invocation)
                            .map_err(|_| LocalFailure::Unavailable)?;
                        (*host, *invocation, result)
                    }
                    // Enrolled children expose no host-local cancellation.
                    LocalRequest::Cancel { .. } => return Err(LocalFailure::Unavailable),
                };
                Ok(runtime_collab::PublishedInvocationObservation {
                    host,
                    invocation,
                    result,
                })
            })
            .ok_or(SenderError::Busy)?;
        self.runtime
            .accept(result, &self.work)
            .map_err(|refusal| refusal.error().clone())
    }

    /// Begin (or resume) one step of an accepted enrolled task and drive it
    /// once: a fresh or retained original child is invoked with its original
    /// UUID. When the child is terminal the parent commits the exact result
    /// and continuation, then the row is released.
    pub(crate) fn step(
        &mut self,
        broker: &AppActionBroker,
        dispatch: &VerifiedEnrolledDispatch,
        parent: &mut dyn ParentOwner,
        step: ParentStep,
        intent: &ChildIntent,
        owner: &EnrolledChildOwner,
    ) -> Result<StepProgress, SenderError> {
        use qos_action_broker::agent_timing::us;
        let began = Instant::now();
        let child = match self.runtime.begin_step(parent, step, intent, &self.work)? {
            StepStart::Committed(_) => return self.settled(parent, step),
            StepStart::Fresh(child) | StepStart::Recovered(child) => child,
        };
        let begin_us = us(began);
        let stage = Instant::now();
        let attempt = self.runtime.attempt(
            parent,
            step,
            AttemptKind::Invoke,
            "",
            Self::deadline(),
            &self.work,
        )?;
        let attempt_us = us(stage);
        let stage = Instant::now();
        self.deliver(attempt, broker, Some(dispatch), Some(owner))?;
        let deliver_us = us(stage);
        let stage = Instant::now();
        let progress = self.settle_if_terminal(parent, step, child);
        qos_action_broker::agent_timing::span(
            "sender_step",
            Some(dispatch.invocation()),
            began.elapsed(),
            &[
                ("begin_us", begin_us),
                ("attempt_us", attempt_us),
                ("deliver_us", deliver_us),
                ("settle_us", us(stage)),
                (
                    "settled",
                    u64::from(matches!(progress, Ok(StepProgress::Settled { .. }))),
                ),
            ],
        );
        progress
    }

    /// Observe a mapped step's original child again (no new invocation).
    pub(crate) fn observe(
        &mut self,
        broker: &AppActionBroker,
        parent: &mut dyn ParentOwner,
        step: ParentStep,
        owner: &EnrolledChildOwner,
    ) -> Result<StepProgress, SenderError> {
        use qos_action_broker::agent_timing::us;
        let began = Instant::now();
        let record = parent.lookup(step)?.ok_or(SenderError::Unmapped)?;
        if record.committed.is_some() {
            return self.settled(parent, step);
        }
        let lookup_us = us(began);
        let stage = Instant::now();
        let attempt = self.runtime.attempt(
            parent,
            step,
            AttemptKind::Observe,
            "",
            Self::deadline(),
            &self.work,
        )?;
        let attempt_us = us(stage);
        let stage = Instant::now();
        self.deliver(attempt, broker, None, Some(owner))?;
        let deliver_us = us(stage);
        let stage = Instant::now();
        let progress = self.settle_if_terminal(parent, step, record.child);
        qos_action_broker::agent_timing::slow(
            "sender_observe",
            Some(uuid::Uuid::from_bytes(step.parent())),
            began,
            &[
                ("lookup_us", lookup_us),
                ("attempt_us", attempt_us),
                ("deliver_us", deliver_us),
                ("settle_us", us(stage)),
                (
                    "settled",
                    u64::from(matches!(progress, Ok(StepProgress::Settled { .. }))),
                ),
            ],
        );
        progress
    }

    fn settle_if_terminal(
        &mut self,
        parent: &mut dyn ParentOwner,
        step: ParentStep,
        child: ChildTicket,
    ) -> Result<StepProgress, SenderError> {
        match self
            .runtime
            .settle(parent, step, &mut continuation, &self.work)
        {
            Ok(_) => self.settled(parent, step),
            Err(SenderError::NotTerminal) => Ok(StepProgress::Pending(child)),
            Err(error) => Err(error),
        }
    }

    fn settled(
        &mut self,
        parent: &mut dyn ParentOwner,
        step: ParentStep,
    ) -> Result<StepProgress, SenderError> {
        let record = parent.lookup(step)?.ok_or(SenderError::Unmapped)?;
        let committed = record.committed.ok_or(SenderError::NotTerminal)?;
        // A committed step whose row is still retained is released here.
        let receipt = self
            .runtime
            .settle(parent, step, &mut continuation, &self.work)?;
        Ok(StepProgress::Settled {
            receipt,
            continuation: committed.continuation,
        })
    }

    /// Restart reconciliation with the host recovery parent: committed steps
    /// are released, terminal children are committed then released, and
    /// every other mapped child is observed by its original UUID only.
    pub(crate) fn recover(
        &mut self,
        owner: &Arc<RuntimeOwner>,
        broker: &AppActionBroker,
    ) -> Result<Vec<(ParentStep, StepProgress)>, String> {
        let mut parent = owner.recovery_parent();
        let recovery = self
            .runtime
            .recover(&mut parent, &mut continuation, &self.work)
            .map_err(|e| format!("{e:?}"))?;
        let mut progress = vec![];
        for recovered in recovery.iter() {
            match recovered {
                Recovered::Settled(step) => {
                    let record = parent
                        .lookup(step)
                        .map_err(|e| format!("{e:?}"))?
                        .and_then(|record| record.committed)
                        .ok_or("recovered step lost its commit")?;
                    progress.push((
                        step,
                        StepProgress::Settled {
                            receipt: ParentReceipt {
                                step,
                                child: parent
                                    .lookup(step)
                                    .map_err(|e| format!("{e:?}"))?
                                    .ok_or("recovered step lost its mapping")?
                                    .child,
                                result: record.result,
                            },
                            continuation: record.continuation,
                        },
                    ));
                }
                Recovered::Drive(step, _) => {
                    let child_owner = owner
                        .store()?
                        .enrolled_child_owner(uuid::Uuid::from_bytes(step.parent()))
                        .map_err(|e| e.to_string())?;
                    let state = self
                        .observe(broker, &mut parent, step, &child_owner)
                        .map_err(|e| format!("{e:?}"))?;
                    progress.push((step, state));
                }
                // Never sent: only an exact-intent begin_step may adopt it.
                Recovered::Unmapped(_) => {}
            }
        }
        Ok(progress)
    }
}

use qos_action_broker::enrolled_dispatch::{
    EnrolledStepRequest, EnrolledStepResult, EnrolledStepService, EnrolledTaskParent,
};
use tokio::sync::{mpsc, oneshot};

pub(crate) type StepJob = (EnrolledStepRequest, oneshot::Sender<EnrolledStepResult>);
const STEP_QUEUE: usize = 32;
const STEPS_PER_PUMP: usize = 8;

/// Wakes the runtime owner loop when a step arrives, so a submitted step is
/// begun at once instead of at the next owner tick. A wake grants nothing;
/// a spurious one only runs an ordinary tick early.
pub(crate) static STEP_WAKE: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Broker-facing door: a bounded queue into the runtime owner thread.
pub(crate) struct CloudStepService(mpsc::Sender<StepJob>);
impl CloudStepService {
    pub(crate) fn channel() -> (Self, mpsc::Receiver<StepJob>) {
        let (sender, receiver) = mpsc::channel(STEP_QUEUE);
        (Self(sender), receiver)
    }
}
impl EnrolledStepService for CloudStepService {
    fn submit(
        &self,
        request: EnrolledStepRequest,
    ) -> Result<oneshot::Receiver<EnrolledStepResult>, String> {
        let (reply, receiver) = oneshot::channel();
        self.0
            .try_send((request, reply))
            .map_err(|_| "enrolled step queue full or closed; retry the same step".to_string())?;
        STEP_WAKE.notify_one();
        Ok(receiver)
    }
}

struct PendingStep {
    dispatch: VerifiedEnrolledDispatch,
    parent: EnrolledTaskParent,
    step: ParentStep,
    owner: EnrolledChildOwner,
    child: ChildTicket,
    reply: Option<oneshot::Sender<EnrolledStepResult>>,
    /// Last full (Store-checked) observation of this step.
    observed: Instant,
}
/// Longest interval between full observations of a pending step whose child
/// row still reads nonterminal. A full observation rechecks the task custody
/// under the Store lock (several milliseconds); the cheap journal peek in
/// between grants nothing and only decides whether to take it now.
const FULL_OBSERVE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
/// Whether the child row may be terminal: a cheap read of the broker's own
/// retained journal row. Unknown or unreadable rows count as possibly
/// terminal, so they always get the full observation.
fn child_maybe_terminal(
    broker: &AppActionBroker,
    owner: &EnrolledChildOwner,
    child: ChildTicket,
) -> bool {
    match broker.observe_enrolled_child(owner, child.host(), child.invocation()) {
        Ok(Some(result)) => {
            result.uncertain
                || serde_json::from_str::<qos_actions::InvocationUpdate>(&result.update_json)
                    .map_or(true, |update| {
                        matches!(
                            update,
                            qos_actions::InvocationUpdate::Succeeded { .. }
                                | qos_actions::InvocationUpdate::Failed { .. }
                                | qos_actions::InvocationUpdate::Cancelled { .. }
                        )
                    })
        }
        Ok(None) | Err(_) => true,
    }
}

/// Owner-thread step driver: startup recovery, then live step requests.
/// A pending child is observed each pump until its parent commit; a dropped
/// broker reply never abandons the child obligation.
pub(crate) struct AgentSteps {
    pub(crate) sender: CloudSender,
    requests: mpsc::Receiver<StepJob>,
    pending: Vec<PendingStep>,
    recovering: Vec<ParentStep>,
    /// Last observation pass over `recovering`.
    recovered_at: Option<Instant>,
    pub(crate) recovered: Vec<(ParentStep, StepProgress)>,
}
/// Interval between observation passes over steps recovered at startup.
/// Their original children are observed by UUID only (never re-invoked), so
/// this bounds only how soon a late terminal row is settled; running the pass
/// on every owner tick held the Store lock for tens of milliseconds per tick
/// and delayed every live step behind it.
const RECOVERY_OBSERVE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
impl AgentSteps {
    pub(crate) fn install(
        owner: &Arc<RuntimeOwner>,
        root: &Path,
        broker: &AppActionBroker,
        requests: mpsc::Receiver<StepJob>,
    ) -> Result<Self, String> {
        let mut sender = CloudSender::install(owner, root)?;
        let recovered = sender.recover(owner, broker)?;
        let recovering = recovered
            .iter()
            .filter(|(_, progress)| matches!(progress, StepProgress::Pending(_)))
            .map(|(step, _)| *step)
            .collect();
        Ok(Self {
            sender,
            requests,
            pending: vec![],
            recovering,
            recovered_at: None,
            recovered,
        })
    }
    fn reply(owner: &RuntimeOwner, step: ParentStep, continuation: Vec<u8>) -> EnrolledStepResult {
        let update_json = owner
            .store()?
            .committed_step_result(uuid::Uuid::from_bytes(step.parent()), step.index())
            .map_err(|e| e.to_string())?
            .ok_or("committed step result missing")?;
        Ok(qos_action_broker::enrolled_dispatch::EnrolledStepReply {
            update_json,
            continuation,
        })
    }
    fn begin(
        &mut self,
        request: EnrolledStepRequest,
        reply: oneshot::Sender<EnrolledStepResult>,
        owner: &Arc<RuntimeOwner>,
        broker: &AppActionBroker,
    ) {
        let invocation = request.dispatch.invocation();
        let began = Instant::now();
        qos_action_broker::agent_timing::mark("step_begin", Some(invocation));
        let prepared = (|| -> Result<_, String> {
            let step = ParentStep::new(*invocation.as_bytes(), request.step)
                .map_err(|e| format!("{e:?}"))?;
            let parent = EnrolledTaskParent::new(&request.dispatch)?;
            let intent = CloudSender::child_intent(
                broker,
                &request.dispatch,
                request.peer,
                &request.resource,
                &request.action,
                request.argument.clone(),
            )?;
            let child_owner = owner
                .store()?
                .enrolled_child_owner(invocation)
                .map_err(|e| e.to_string())?;
            Ok((step, parent, intent, child_owner))
        })();
        qos_action_broker::agent_timing::span(
            "step_prepare",
            Some(invocation),
            began.elapsed(),
            &[],
        );
        let (step, mut parent, intent, child_owner) = match prepared {
            Ok(value) => value,
            Err(error) => {
                let _ = reply.send(Err(error));
                return;
            }
        };
        // Lane E: the agent's own durable action log (agent_history).
        crate::agent_history::note_step(
            owner.root(),
            &request.dispatch.envelope().recipient_agent,
            invocation,
            request.step,
            &intent,
        );
        // A retry of a step already being driven only replaces its reply.
        if let Some(existing) = self.pending.iter_mut().find(|p| p.step == step) {
            existing.reply = Some(reply);
            return;
        }
        match self.sender.step(
            broker,
            &request.dispatch,
            &mut parent,
            step,
            &intent,
            &child_owner,
        ) {
            Ok(StepProgress::Settled { continuation, .. }) => {
                let _ = reply.send(Self::reply(owner, step, continuation));
                qos_action_broker::agent_timing::mark("step_replied", Some(invocation));
            }
            Ok(StepProgress::Pending(child)) => self.pending.push(PendingStep {
                dispatch: request.dispatch,
                parent,
                step,
                owner: child_owner,
                child,
                reply: Some(reply),
                observed: Instant::now(),
            }),
            Err(error) => {
                let _ = reply.send(Err(format!("{error:?}")));
            }
        }
    }
    /// Pending, recovering or queued steps exist.
    pub(crate) fn busy(&self) -> bool {
        !self.pending.is_empty() || !self.recovering.is_empty() || !self.requests.is_empty()
    }
    pub(crate) fn pump(&mut self, owner: &Arc<RuntimeOwner>, broker: &AppActionBroker) {
        if !self.recovering.is_empty()
            && self
                .recovered_at
                .is_none_or(|at| at.elapsed() >= RECOVERY_OBSERVE_INTERVAL)
        {
            self.recovered_at = Some(Instant::now());
            let began = Instant::now();
            let count = self.recovering.len() as u64;
            let mut parent = owner.recovery_parent();
            let mut still = vec![];
            for step in std::mem::take(&mut self.recovering) {
                let child_owner = match owner.store().and_then(|store| {
                    store
                        .enrolled_child_owner(uuid::Uuid::from_bytes(step.parent()))
                        .map_err(|e| e.to_string())
                }) {
                    Ok(value) => value,
                    Err(error) => {
                        tracing::debug!(%error, "recovered step owner unavailable");
                        still.push(step);
                        continue;
                    }
                };
                match self.sender.observe(broker, &mut parent, step, &child_owner) {
                    Ok(StepProgress::Settled { .. }) => {}
                    Ok(StepProgress::Pending(_)) => still.push(step),
                    Err(error) => {
                        tracing::debug!(?error, "recovered step still pending");
                        still.push(step);
                    }
                }
            }
            qos_action_broker::agent_timing::slow(
                "steps_recovery_pass",
                None,
                began,
                &[("steps", count), ("pending", still.len() as u64)],
            );
            self.recovering = still;
        }
        for _ in 0..STEPS_PER_PUMP {
            let Ok((request, reply)) = self.requests.try_recv() else {
                break;
            };
            self.begin(request, reply, owner, broker);
        }
        let mut index = 0;
        while index < self.pending.len() {
            let pending = &mut self.pending[index];
            if pending.observed.elapsed() < FULL_OBSERVE_INTERVAL
                && !child_maybe_terminal(broker, &pending.owner, pending.child)
            {
                index += 1;
                continue;
            }
            pending.observed = Instant::now();
            match self
                .sender
                .observe(broker, &mut pending.parent, pending.step, &pending.owner)
            {
                Ok(StepProgress::Pending(_)) => index += 1,
                Ok(StepProgress::Settled { continuation, .. }) => {
                    let done = self.pending.swap_remove(index);
                    if let Some(reply) = done.reply {
                        let _ = reply.send(Self::reply(owner, done.step, continuation));
                        qos_action_broker::agent_timing::mark(
                            "step_replied",
                            Some(uuid::Uuid::from_bytes(done.step.parent())),
                        );
                    }
                    drop(done.dispatch);
                }
                Err(error) => {
                    // The child stays durably mapped; later pumps (or the
                    // restart recovery parent) resume it by its original UUID.
                    tracing::debug!(?error, "enrolled step observation deferred");
                    if let Some(reply) = pending.reply.take() {
                        let _ = reply.send(Err(format!("{error:?}")));
                    }
                    index += 1;
                }
            }
        }
    }
}
