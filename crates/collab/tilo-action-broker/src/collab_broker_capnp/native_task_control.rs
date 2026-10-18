//! Private control observations on the admitted task-run subscription.
use super::*;
use futures::{StreamExt, future::BoxFuture, stream::FuturesUnordered};
use std::collections::HashSet;

// Includes detached, already-running blocking calls after a subscription closes.
// Dropping a read cannot create an unbounded backlog of synchronous owner RPCs.
static OBSERVATIONS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(32);

pub(super) async fn blocking<T: Send + 'static>(
    read: impl FnOnce() -> Result<T, capnp::Error> + Send + 'static,
) -> Result<T, capnp::Error> {
    let permit = OBSERVATIONS.try_acquire().map_err(|_| {
        capnp::Error::overloaded("task control observation capacity exhausted".into())
    })?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        read()
    })
    .await
    .map_err(|_| capnp::Error::failed("task control observation interrupted".into()))?
}

impl CapnpBrokerState {
    /// Wake existing subscriptions, never enqueue private contents or acknowledge mail.
    pub(super) fn refresh_task_control(&self) {
        self.task_mailbox_refresh.send_replace(());
        self.refresh_task_control_subscribers();
    }

    // Existing delivery reconciliation may refresh control observations on its
    // timer. Mailbox waits wake only on actual owner/broker change hints.
    pub(super) fn refresh_task_control_subscribers(&self) {
        if let Ok(subscribers) = self.task_subscribers.lock() {
            for subscriber in subscribers.values() {
                subscriber.control_refresh.send_replace(());
            }
        }
    }
}

struct Observation {
    run: PendingTaskRun,
    snapshot: tilo_peer_state::workspace_task::WorkspaceTaskSnapshot,
    control: Option<runtime_collab::TaskRunControl>,
}

enum ObservationStage {
    Armed(Box<PendingTaskRun>, Option<watch::Receiver<()>>),
    Observed(Box<Observation>),
}

type ObservationJob = BoxFuture<'static, (u64, u64, u64, Result<ObservationStage, capnp::Error>)>;

pub(super) struct TaskControlReader {
    state: Arc<CapnpBrokerState>,
    peer: u64,
    route: CrdtRoute,
    identity: AdmittedJoinIdentity,
    refresh: watch::Receiver<()>,
    seen: HashMap<u64, runtime_collab::TaskRunControl>,
    mailbox_changes: HashMap<u64, watch::Receiver<()>>,
    jobs: FuturesUnordered<ObservationJob>,
    running: HashSet<u64>,
    observed: HashMap<u64, u64>,
    generation: u64,
    read_sequence: u64,
}

impl TaskControlReader {
    pub(super) fn new(
        state: Arc<CapnpBrokerState>,
        peer: u64,
        route: CrdtRoute,
        identity: AdmittedJoinIdentity,
        refresh: watch::Receiver<()>,
    ) -> Self {
        Self {
            state,
            peer,
            route,
            identity,
            refresh,
            seen: HashMap::new(),
            mailbox_changes: HashMap::new(),
            jobs: FuturesUnordered::new(),
            running: HashSet::new(),
            observed: HashMap::new(),
            generation: 1,
            read_sequence: 0,
        }
    }

    fn cancel(&mut self) {
        self.jobs = FuturesUnordered::new();
        self.running.clear();
        self.mailbox_changes.clear();
        self.seen.clear();
        self.observed.clear();
    }

    fn check_open(&mut self) -> Result<(), capnp::Error> {
        if self.refresh.has_changed().is_err() || !self.state.is_accepting() {
            self.cancel();
            return Err(capnp::Error::disconnected(
                "task control subscription closed".into(),
            ));
        }
        Ok(())
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one multiplexed control turn: watch arming, observation, fresh validation and delivery"
    )]
    pub(super) async fn next(&mut self) -> Result<runtime_collab::TaskRunControl, capnp::Error> {
        self.read_sequence = self
            .read_sequence
            .checked_add(1)
            .ok_or_else(|| capnp::Error::failed("task control read sequence exhausted".into()))?;
        loop {
            self.check_open()?;
            // This is only a bounded local lease lookup. Certificate, current
            // source and owner checks may perform RPCs and belong on the worker.
            let agent = self.identity.agent_id.as_ref().ok_or_else(|| {
                capnp::Error::failed("task control requires an admitted agent".into())
            })?;
            let runs = self
                .state
                .pending_task_runs
                .lock()
                .map_err(|_| capnp::Error::failed("task run registry unavailable".into()))?
                .iter()
                .filter(|(_, run)| {
                    run.peer_id == self.peer
                        && run.agent_id == *agent
                        && run.cluster_id == self.route.cluster_id
                        && !run.terminal
                        && run.native.is_some()
                })
                .map(|(id, run)| (*id, run.clone()))
                .collect::<HashMap<_, _>>();
            self.seen.retain(|id, _| runs.contains_key(id));
            self.mailbox_changes.retain(|id, _| runs.contains_key(id));
            self.observed.retain(|id, _| runs.contains_key(id));
            for (id, run) in runs {
                if self.running.contains(&id) || self.observed.get(&id) == Some(&self.generation) {
                    continue;
                }
                let (state, route, identity, peer, generation, subscribe) = (
                    self.state.clone(),
                    self.route.clone(),
                    self.identity.clone(),
                    self.peer,
                    self.generation,
                    !self.mailbox_changes.contains_key(&id),
                );
                self.running.insert(id);
                self.jobs.push(Box::pin(async move {
                    let result = blocking(move || {
                        state.revalidate_admission(&route, &identity)?;
                        if subscribe {
                            exact_run(&state, peer, id, &run)?;
                            let binding = run.native.as_ref().ok_or_else(|| {
                                capnp::Error::failed("native task binding absent".into())
                            })?;
                            let service = state.native_tasks(&run.cluster_id).ok_or_else(|| {
                                capnp::Error::failed("task control owner unavailable".into())
                            })?;
                            let changes = service
                                .message_changes_for_run(&run.cluster_id, binding)
                                .map_err(capnp::Error::failed)?;
                            Ok(ObservationStage::Armed(Box::new(run), changes))
                        } else {
                            observe(&state, peer, id, run).map(|observation| {
                                ObservationStage::Observed(Box::new(observation))
                            })
                        }
                    })
                    .await;
                    (id, generation, 0, result)
                }));
            }
            let mailbox = async {
                if self.mailbox_changes.is_empty() {
                    return std::future::pending().await;
                }
                let changes = self
                    .mailbox_changes
                    .values_mut()
                    .map(|receiver| Box::pin(receiver.changed()))
                    .collect::<Vec<_>>();
                futures::future::select_all(changes).await.0
            };
            tokio::select! {
                changed = self.refresh.changed() => {
                    if changed.is_err() {
                        self.cancel();
                        return Err(capnp::Error::disconnected("task control subscription closed".into()));
                    }
                    self.generation = self.generation.checked_add(1)
                        .ok_or_else(|| capnp::Error::failed("task control generation exhausted".into()))?;
                }
                changed = mailbox => {
                    if changed.is_err() {
                        self.cancel();
                        return Err(capnp::Error::disconnected("task control owner notification closed".into()));
                    }
                    self.generation = self.generation.checked_add(1)
                        .ok_or_else(|| capnp::Error::failed("task control generation exhausted".into()))?;
                }
                Some((id, generation, validated_read, result)) = self.jobs.next(), if !self.jobs.is_empty() => {
                    self.running.remove(&id);
                    self.observed.insert(id, generation);
                    let stage = match result {
                        Ok(stage) => stage,
                        Err(error) => {
                            // A superseded private body is inert, not a closed
                            // multiplexed stream. Retry that run from current
                            // owner authority; never strand its live siblings.
                            if matches!(error.extra.as_str(),
                                "task control owner changed during observation"
                                | "native task authority changed during private observation")
                            {
                                self.observed.remove(&id);
                                continue;
                            }
                            if matches!(error.extra.as_str(),
                                "private task has no active run" | "private task run ended"
                                | "task run ended")
                            {
                                // Refuse the ended run's captured body to the
                                // caller. The client retries this typed transient
                                // response; admission/custody errors do not retry.
                                self.cancel();
                                return Err(capnp::Error::overloaded(
                                    "task control observation superseded".into()));
                            }
                            // Admission and custody failures remain terminal.
                            #[cfg(test)]
                            eprintln!("task control observation refused: {error}");
                            self.cancel(); return Err(error);
                        }
                    };
                    let observation = match stage {
                        ObservationStage::Armed(run, changes) => {
                            // Own the watch before starting a potentially slow
                            // private read, so disconnect cancels it immediately.
                            if let Some(changes) = changes { self.mailbox_changes.insert(id, changes); }
                            let (state, route, identity, peer) = (
                                self.state.clone(), self.route.clone(), self.identity.clone(), self.peer,
                            );
                            self.running.insert(id);
                            self.jobs.push(Box::pin(async move {
                                let result = blocking(move || {
                                    state.revalidate_admission(&route, &identity)?;
                                    observe(&state, peer, id, *run).map(|observation| ObservationStage::Observed(Box::new(observation)))
                                }).await;
                                (id, generation, 0, result)
                            }));
                            continue;
                        }
                        ObservationStage::Observed(observation) => *observation,
                    };
                    let Some(control) = observation.control.as_ref() else { continue };
                    if !has_new_context(self.seen.get(&id), control) { continue; }
                    if validated_read != self.read_sequence {
                        // A completed observation is inert, including one queued
                        // behind an earlier read. Validate for THIS delivery.
                        // Validation is also per-run and multiplexed: a delayed
                        // governor check for A must not hold B or disconnect.
                        let (state, route, identity, peer, sequence) = (
                            self.state.clone(), self.route.clone(), self.identity.clone(),
                            self.peer, self.read_sequence,
                        );
                        self.running.insert(id);
                        self.jobs.push(Box::pin(async move {
                            let result = blocking(move || {
                                validate_observation(&state, peer, id, &observation.run, &observation.snapshot)?;
                                // Actual admitted certificate/delegation, never
                                // an actor reconstructed from a starter label.
                                state.revalidate_admission(&route, &identity)?;
                                Ok(ObservationStage::Observed(Box::new(observation)))
                            }).await;
                            (id, generation, sequence, result)
                        }));
                        continue;
                    }
                    self.check_open()?;
                    exact_run(&self.state, self.peer, id, &observation.run)?;
                    let control = observation.control.ok_or_else(|| capnp::Error::failed("task control observation absent".into()))?;
                    self.seen.insert(id, control.clone());
                    return Ok(control);
                }
            }
        }
    }
}

fn exact_run(
    state: &CapnpBrokerState,
    peer: u64,
    id: u64,
    run: &PendingTaskRun,
) -> Result<(), capnp::Error> {
    let exact = state
        .task_run_for(peer, &run.agent_id, id, &run.lease)
        .map_err(capnp::Error::failed)?;
    if exact.lease != run.lease
        || exact.cluster_id != run.cluster_id
        || exact.task_id != run.task_id
        || !exact
            .native
            .as_ref()
            .zip(run.native.as_ref())
            .is_some_and(|(exact, expected)| {
                exact.task_id == expected.task_id
                    && exact.agent_id == expected.agent_id
                    && exact.epoch == expected.epoch
                    && exact.revision == expected.revision
                    && exact.starter == expected.starter
            })
    {
        return Err(capnp::Error::failed("task control lease changed".into()));
    }
    Ok(())
}

fn observe(
    state: &CapnpBrokerState,
    peer: u64,
    id: u64,
    run: PendingTaskRun,
) -> Result<Observation, capnp::Error> {
    exact_run(state, peer, id, &run)?;
    let binding = run
        .native
        .as_ref()
        .ok_or_else(|| capnp::Error::failed("native task binding absent".into()))?;
    let service = state
        .native_tasks(&run.cluster_id)
        .ok_or_else(|| capnp::Error::failed("task control owner unavailable".into()))?;
    let snapshot = service
        .agent_snapshot(&run.cluster_id, &run.agent_id)
        .map_err(capnp::Error::failed)?;
    let owner = snapshot.controls.get(&run.task_id);
    if owner.is_some_and(|control| control.run_epoch == binding.epoch)
        && snapshot
            .tasks
            .iter()
            .any(|task| task.id == run.task_id && task.status == WorkspaceTaskStatus::Cancelled)
    {
        // Refuse the stopped run's observation, including a retry after a
        // captured private body was superseded. Silence would leave that read
        // pending forever instead of letting the client retry live siblings.
        return Err(capnp::Error::failed("private task run ended".into()));
    }
    let active = owner
        .and_then(|owner| owner.active_run.as_ref())
        .filter(|active| {
            active.epoch == binding.epoch
                && active.agent_id == binding.agent_id
                && active.approval.as_ref().map(|approval| &approval.person)
                    == binding.starter.as_ref()
                && owner
                    .and_then(|owner| owner.execution.as_ref())
                    .is_some_and(|execution| execution.run_epoch == binding.epoch)
        });
    let control = if let Some(active) = active {
        let (base, _) = runtime_collab::native_task_revision(&run.lease)
            .ok_or_else(|| capnp::Error::failed("invalid native task lease".into()))?;
        let lease = format!("{base}:{}", active.applied_direction_revision);
        state
            .read_native_task_status(&run, &lease)
            .map_err(capnp::Error::failed)?
            .control
    } else {
        None
    };
    Ok(Observation {
        run,
        snapshot,
        control,
    })
}

fn validate_observation(
    state: &CapnpBrokerState,
    peer: u64,
    id: u64,
    run: &PendingTaskRun,
    expected: &tilo_peer_state::workspace_task::WorkspaceTaskSnapshot,
) -> Result<(), capnp::Error> {
    let service = state
        .native_tasks(&run.cluster_id)
        .ok_or_else(|| capnp::Error::failed("task control owner unavailable".into()))?;
    // A fresh governor read also rechecks physical host custody/current source.
    // Never perform this RPC inside an owner callback or local lease lock.
    let current = service
        .agent_snapshot(&run.cluster_id, &run.agent_id)
        .map_err(capnp::Error::failed)?;
    if current.controls.get(&run.task_id) != expected.controls.get(&run.task_id)
        || current.tasks.iter().find(|task| task.id == run.task_id)
            != expected.tasks.iter().find(|task| task.id == run.task_id)
    {
        return Err(capnp::Error::failed(
            "task control owner changed during observation".into(),
        ));
    }
    exact_run(state, peer, id, run)
}

fn has_new_context(
    prior: Option<&runtime_collab::TaskRunControl>,
    control: &runtime_collab::TaskRunControl,
) -> bool {
    control.directions.iter().any(|direction| {
        prior.is_none_or(|prior| {
            !prior
                .directions
                .iter()
                .any(|old| old.revision == direction.revision)
        })
    }) || control.messages.iter().any(|message| {
        prior.is_none_or(|prior| {
            !prior
                .messages
                .iter()
                .any(|old| old.message_id == message.message_id)
        })
    })
}
