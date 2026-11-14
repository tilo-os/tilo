//! The host a launched task agent's run takes its steps through: every
//! invocation it makes is a child of that run, admitted by the run
//! ([`super::tracker_runs`]): an application's action through the run's
//! child path, a host step through the run's admission and then the host
//! step door as the task agent. A stopped run, a newer run of the issue or a
//! revoked device cuts its authority.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, pending,
};

use super::durable_local_invocation::LocalCaller;
use super::native_task_execution::ChildReask;
use super::published_invocation::InvocationCaller;
use super::{AdmittedJoinIdentity, AppBrokerState, CrdtRoute, InvocationOwner, PendingTaskRun};

/// How an invocation of this connection is read back.
#[derive(Clone)]
enum Started {
    /// A child of the run: owned by the run, asked of `asked`.
    Child {
        wire: u64,
        owner: InvocationOwner,
        asked: Box<Asked>,
    },
    /// A host step, admitted by the run and taken by the task agent.
    HostStep,
    /// A child the run is still admitting (an app it launches for it):
    /// accepted until its admission answers.
    Starting,
    /// A child the run refused, and why: what a caller that stopped
    /// waiting for the admission reads when it looks again.
    Refused(String),
}

/// What a child of the run was asked of: where its outcome is read back when the broker keeps it
/// under an identity of its own (an application's launch, a host activation).
#[derive(Clone)]
struct Asked {
    peer_id: u64,
    provider: String,
    resource: String,
    action: String,
    argument: Option<String>,
}

/// The run a host works in, as the broker delivered it.
struct RunRef {
    run_id: u64,
    lease: String,
    task_id: String,
}

struct Inner {
    state: Arc<AppBrokerState>,
    peer: u64,
    route: CrdtRoute,
    admitted: AdmittedJoinIdentity,
    agent: String,
    person: String,
    run: RunRef,
    /// Who the task agent is to the catalogue and the host steps, in this
    /// connection's execution.
    caller: Caller,
    started: Mutex<HashMap<tilo_actions::InvocationId, Started>>,
}

/// A launched task agent, working in its task's run.
#[derive(Clone)]
pub struct TaskRunHost(Arc<Inner>);

impl tilo_broker_core::steps::TaskRun for TaskRunHost {
    fn door(&self) -> Arc<dyn ActionHost + Send + Sync> {
        Arc::new(self.clone())
    }
}

impl std::fmt::Debug for TaskRunHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskRunHost")
            .field("agent", &self.0.agent)
            .field("issue", &self.0.run.task_id)
            .finish_non_exhaustive()
    }
}

impl AppBrokerState {
    /// The host a launched task agent's run takes its host steps through
    /// (a task's programs): on the agent's own channel (`peer` on `route`, admitted as `admitted`), in
    /// its delivered run `run_id` at `lease`.
    pub(super) fn launched_task_run_host(
        self: &Arc<Self>,
        (peer, route, admitted): (u64, CrdtRoute, AdmittedJoinIdentity),
        (run_id, lease, task_id): (u64, &str, &str),
    ) -> Result<TaskRunHost, String> {
        let agent = admitted
            .agent_cert
            .as_deref()
            .map(|cert| cert.agent_id.clone())
            .or_else(|| admitted.agent_id.clone())
            .ok_or("a task's steps are its agent's")?;
        let run = RunRef {
            run_id,
            lease: lease.to_owned(),
            task_id: task_id.to_owned(),
        };
        Ok(TaskRunHost::new(self, (peer, route, admitted), agent, run))
    }
}

impl TaskRunHost {
    fn new(
        state: &Arc<AppBrokerState>,
        (peer, route, admitted): (u64, CrdtRoute, AdmittedJoinIdentity),
        agent: String,
        run: RunRef,
    ) -> Self {
        let person = admitted.principal_id.clone();
        let caller = Caller::local_person(person.clone())
            .in_workspace(&route.cluster_id)
            .in_new_execution()
            .delegated_to(agent.clone());
        Self(Arc::new(Inner {
            state: Arc::clone(state),
            peer,
            route,
            admitted,
            agent,
            person,
            run,
            caller,
            started: Mutex::default(),
        }))
    }

    /// Who the task agent is to the host steps it takes in its run.
    pub(super) fn caller(&self) -> &Caller {
        &self.0.caller
    }

    /// The connection the run was delivered to.
    pub(super) fn peer(&self) -> u64 {
        self.0.peer
    }

    pub(super) fn state(&self) -> Arc<AppBrokerState> {
        Arc::clone(&self.0.state)
    }

    /// Admit a step of the run on a record outside its app: `action` of the
    /// app `app` on `resource`, needing `kind`. The run decides it as it
    /// decides its other steps (its delegation, its issue's and its
    /// starter's live grants, only while it goes), for `caller`, this
    /// host's own execution.
    pub(super) fn admit_record_step(
        &self,
        caller: &Caller,
        (app, resource, action): (&str, &str, &str),
        kind: tilo_action_policy::ActionKind,
    ) -> Result<(), ActionHostError> {
        self.check(caller)?;
        let refused = |reason: String| ActionHostError::Refused { reason };
        let run = self.pending_run().map_err(refused)?;
        let requirement = crate::services::NativeTaskActionRequirement {
            binding: run.native.clone(),
            app: app.to_owned(),
            resource_kind: String::new(),
            resource: resource.to_owned(),
            action: action.to_owned(),
            need: tilo_actions::ActionNeed {
                kind,
                field: None,
                privileged: false,
            },
            consequential: false,
            attended: false,
            confirmed: false,
        };
        self.0
            .state
            .admit_task_launch(&run, &requirement, &mut || Ok(()))
            .map_err(|reason| {
                if crate::services::NativeTaskActionRequirement::awaits_confirmation(&reason) {
                    ActionHostError::confirmation_required()
                } else {
                    refused(reason)
                }
            })
    }
}

impl TaskRunHost {
    /// The task agent this connection is, as its delegation names it.
    #[must_use]
    pub fn agent(&self) -> &str {
        &self.0.agent
    }

    /// The issue whose run this connection works in, by its address.
    #[must_use]
    pub fn issue(&self) -> &str {
        &self.0.run.task_id
    }

    /// The person the run acts for.
    #[must_use]
    pub fn person(&self) -> &str {
        &self.0.person
    }

    /// The workspace the run is in.
    #[must_use]
    pub fn workspace(&self) -> &str {
        &self.0.route.cluster_id
    }

    /// The run as the broker holds it for this connection.
    fn pending_run(&self) -> Result<PendingTaskRun, String> {
        self.0.state.task_run_for(
            self.0.peer,
            &self.0.agent,
            self.0.run.run_id,
            &self.0.run.lease,
        )
    }

    fn local_caller(&self) -> Result<LocalCaller, String> {
        LocalCaller::admitted_on(
            &self.0.state,
            self.0.peer,
            self.0.route.clone(),
            &self.0.admitted,
        )
    }

    /// The caller is this connection's own execution, still live.
    fn check(&self, caller: &Caller) -> Result<(), ActionHostError> {
        if !caller.execution_is_live() || !self.0.caller.execution_is_live() {
            return Err(ActionHostError::NotPermitted);
        }
        Ok(())
    }

    fn remember(&self, invocation: tilo_actions::InvocationId, started: Started) {
        if let Ok(mut map) = self.0.started.lock() {
            if map.len() >= 1024 {
                map.clear();
            }
            map.insert(invocation, started);
        }
    }

    fn started(&self, invocation: &tilo_actions::InvocationId) -> Option<Started> {
        self.0.started.lock().ok()?.get(invocation).cloned()
    }

    /// A host step, admitted by the run like any of its children (its
    /// grants, its issue and its starter decide), taken on the host by the
    /// task agent, and listed in history in the run's group. The step is
    /// handed this host, so
    /// what it does in turn (a task's program: its actions, its reads and
    /// changes of records) is the run's too.
    pub(super) fn invoke_host_step(
        &self,
        steps: &dyn crate::services::HostStepServices,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionHostError> {
        let refused = |reason: String| ActionHostError::Refused { reason };
        let run = self.pending_run().map_err(refused)?;
        let argument = request.argument.to_slot();
        let descriptor = steps
            .invocation(&request.target.address, argument.as_deref())
            .ok_or(ActionHostError::UnknownAction)?;
        let requirement = crate::services::NativeTaskActionRequirement {
            binding: run.native.clone(),
            app: steps.app_id().to_owned(),
            resource_kind: descriptor.resource_kind.clone(),
            resource: descriptor.address.resource.clone(),
            action: descriptor.address.action.clone(),
            need: descriptor.contract.need(),
            consequential: descriptor.contract.asks_for_argument(argument.as_deref()),
            attended: descriptor.contract.is_attended(),
            confirmed: false,
        };
        // A step marked for turns is admitted by the turn itself, below, where a grant that
        // asks first raises the turn's question on the person's sheet
        // (`host_steps::admit_turn_step`): its ask is not a refusal here, and asking twice
        // would raise the question twice.
        if let Err(reason) = self
            .0
            .state
            .admit_task_launch(&run, &requirement, &mut || Ok(()))
            && !(steps.for_turns()
                && crate::services::NativeTaskActionRequirement::awaits_confirmation(&reason))
        {
            return Err(refused(reason));
        }
        let origin = self.origin().map_err(refused)?;
        // The run's group, so history lists the step under the run.
        let mut request = request.clone();
        let mut context = request.context.take().unwrap_or_default();
        context.group.clone_from(&run.action_context.group);
        context.purpose.clone_from(&run.action_context.purpose);
        request.context = Some(context);
        // The run's turn is the turn a step marked for turns answers: the run host's own
        // connection carries the person's delegation, and the run it serves names the message.
        let turn = self
            .pending_run()
            .ok()
            .and_then(|run| self.0.state.tracker_run_of(&run).ok())
            .and_then(|run| run.turn);
        let receipt = self.0.state.invoke_host_step_in_turn(
            &self.0.caller,
            Some(&origin),
            steps,
            &request,
            Some(self),
            turn.as_ref(),
        )?;
        self.remember(request.invocation, Started::HostStep);
        Ok(receipt)
    }

    /// This connection as the host steps' admission reads it.
    fn origin(&self) -> Result<InvocationCaller, String> {
        let time = self
            .0
            .state
            .registered_policy_time()?
            .ok_or("the policy clock is unavailable")?;
        Ok(InvocationCaller::Local {
            route: self.0.route.clone(),
            admitted: Box::new(self.0.admitted.clone()),
            time: Some(time),
        })
    }
}

impl tilo_actions::remote::ActionHost for TaskRunHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        if let Err(error) = self.check(caller) {
            return Pending::ready(Err(error));
        }
        if let Err(reason) = self.pending_run() {
            return Pending::ready(Err(ActionHostError::Unavailable { reason }));
        }
        self.0.state.action_query(&self.0.caller, &query)
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if let Err(refused) = tilo_actions::appearance::refuse_here(&request) {
            return Pending::ready(Err(refused));
        }
        if let Err(error) = self.check(caller) {
            return Pending::ready(Err(error));
        }
        let provider = request.target.provider.as_str();
        if let Some(steps) = super::host_steps::for_provider(&self.0.state.services, provider) {
            return Pending::ready(self.invoke_host_step(steps.as_ref(), &request));
        }
        if provider.starts_with(super::canonical_route::PROVIDER_PREFIX) {
            return Pending::ready(Err(ActionHostError::Refused {
                reason: "an action reached through an app's directory is not yet taken in a run; \
                         use the provider its window publishes"
                    .into(),
            }));
        }
        // A live publication by its connection, or an app the run may launch.
        let (peer_id, launch) = provider
            .parse::<u64>()
            .map_or_else(|_| (0, provider.to_owned()), |peer| (peer, String::new()));
        let host = self.clone();
        let (complete, answer) = pending();
        let runtime = match super::launchable::delivery_runtime() {
            Ok(runtime) => runtime,
            Err(reason) => return Pending::ready(Err(ActionHostError::Unavailable { reason })),
        };
        runtime.spawn(async move {
            let result = host.start_child(peer_id, &launch, &request).await;
            complete.complete(result);
        });
        answer
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.check(caller).ok()?;
        match self.started(id)? {
            Started::Child { wire, owner, asked } => {
                if let Some(update) = self.0.state.invocation_status_for_owner(wire, &owner) {
                    return Some(
                        InvocationState::new(*id, update)
                            .with_untrusted(self.0.state.untrusted_job(wire)),
                    );
                }
                // An application's launch is kept under the activation's own identity, not the
                // child's: read back as the run reads its children, as a launched agent does.
                let run = self.pending_run().ok()?;
                let caller = self.local_caller().ok()?;
                let target = runtime_collab::TaskAction {
                    peer_id: asked.peer_id,
                    provider: &asked.provider,
                    resource: &asked.resource,
                    action: &asked.action,
                    argument: asked.argument.as_deref(),
                };
                let (_, update) = self
                    .0
                    .state
                    .native_task_observation(&run, *id, target, &caller)
                    .ok()??;
                Some(InvocationState::new(*id, update))
            }
            Started::HostStep => self.0.state.action_observe(&self.0.caller, id),
            Started::Starting => Some(InvocationState::new(
                *id,
                tilo_actions::InvocationUpdate::Accepted { invocation: *id },
            )),
            Started::Refused(reason) => Some(InvocationState::new(
                *id,
                tilo_actions::InvocationUpdate::Failed {
                    invocation: *id,
                    error: tilo_actions::ActionFailure {
                        code: "run_refused".into(),
                        message: reason,
                        details: None,
                        retryable: false,
                    },
                },
            )),
        }
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        self.check(caller).ok()?;
        match self.started(id)? {
            Started::Child { .. } | Started::Starting => {
                let mut seen = self.0.state.invocation_changes();
                let runtime = tokio::runtime::Handle::try_current().ok()?;
                let (changed, change) = pending();
                runtime.spawn(async move {
                    tokio::select! {
                        _ = seen.changed() => {}
                        () = tilo_rt::sleep(super::action_host::CHANGE_SAFETY_INTERVAL) => {}
                        () = changed.abandoned() => return,
                    }
                    changed.complete(());
                });
                Some(change)
            }
            Started::HostStep => self
                .0
                .state
                .action_next_change_after_now(&self.0.caller, id),
            Started::Refused(_) => None,
        }
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Pending<Result<(), ActionHostError>> {
        if let Err(error) = self.check(caller) {
            return Pending::ready(Err(error));
        }
        match self.started(id) {
            Some(Started::Child { wire, owner, .. }) => super::action_host::answered_when_durable(
                self.0
                    .state
                    .request_cancellation_for_owner(wire, &owner)
                    .map_err(|reason| ActionHostError::NotCancellable { reason }),
            ),
            Some(Started::HostStep) => self.0.state.action_cancel(&self.0.caller, id),
            Some(Started::Starting) => Pending::ready(Err(ActionHostError::NotCancellable {
                reason: "the run is still admitting it".into(),
            })),
            Some(Started::Refused(reason)) => {
                Pending::ready(Err(ActionHostError::NotCancellable { reason }))
            }
            None => Pending::ready(Err(ActionHostError::UnknownInvocation)),
        }
    }

    /// Records outside their apps, read as steps of the run.
    fn record_reads(&self) -> Option<Arc<dyn tilo_actions::remote::RecordReads>> {
        Some(Arc::new(super::record_read::RunRecords(self.clone())))
    }

    /// Records outside their apps, changed as steps of the run.
    fn record_changes(&self) -> Option<Arc<dyn tilo_actions::remote::RecordChanges>> {
        self.0.state.services.record_changes.as_ref()?;
        Some(Arc::new(super::record_read::RunRecords(self.clone())))
    }
}

impl TaskRunHost {
    /// Start `request` as a child of the run: on the live publication of
    /// connection `peer_id`, or, with `launch`, in the app that provider
    /// names, launched for the run.
    async fn start_child(
        &self,
        peer_id: u64,
        launch: &str,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionHostError> {
        let refused = |reason: String| ActionHostError::Refused { reason };
        let run = self.pending_run().map_err(refused)?;
        let caller = self.local_caller().map_err(refused)?;
        let argument = request.argument.to_slot();
        let reask = ChildReask {
            peer_id: self.0.peer,
            agent_id: &self.0.agent,
            run_id: self.0.run.run_id,
            lease: &self.0.run.lease,
            invocation: request.invocation,
            target: runtime_collab::TaskAction {
                peer_id,
                provider: launch,
                resource: &request.target.address.resource,
                action: &request.target.address.action,
                argument: argument.as_deref(),
            },
            caller: &caller,
        };
        self.remember(request.invocation, Started::Starting);
        let started = self
            .0
            .state
            .invoke_native_task_child(&reask, None, (&self.0.route, &self.0.admitted))
            .await;
        let (wire, update) = match started {
            Ok(started) => started,
            Err(error) => {
                let error = error.to_string();
                self.remember(request.invocation, Started::Refused(error.clone()));
                return Err(refused(error));
            }
        };
        self.remember(
            request.invocation,
            Started::Child {
                wire,
                owner: InvocationOwner::NativeTask {
                    agent: run.agent_id.clone(),
                    task: run.task_id.clone(),
                    epoch: run.native.epoch,
                },
                asked: Box::new(Asked {
                    peer_id,
                    provider: launch.to_owned(),
                    resource: request.target.address.resource.clone(),
                    action: request.target.address.action.clone(),
                    argument: argument.clone(),
                }),
            },
        );
        Ok(InvocationState::new(request.invocation, update).receipt())
    }
}
