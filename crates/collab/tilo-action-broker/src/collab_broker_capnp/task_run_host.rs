//! A task agent the broker serves in this process: the desktop's agent door.
//!
//! The person admits a client at the door (an AI agent in a terminal), and
//! the desktop asks Tasks for an issue and a run for it, assigned to a task
//! agent it minted for that connection (docs/architecture/agents.md, "Task
//! agents and collaborators"). The connection then joins here as that task
//! agent, under the task delegation its device signed, exactly as a launched
//! task agent joins over its channel ([`super::task_agents`]), and receives
//! its run the same way: a task subscription the tracker's going runs are
//! delivered to.
//!
//! Every invocation it makes is a child of that run, admitted by the run
//! ([`super::tracker_runs`]): an application's action through the run's
//! child path, a host step through the run's admission and then the host
//! step door as the task agent. There is no other way in: a stopped run, a
//! newer run of the issue or a revoked device cuts the connection's
//! authority, as it cuts a launched task agent's.
//!
//! Discovery is visibility, not admission: the catalogue is the one a
//! person's device shows the agent it hands a session to, and every action
//! taken from it is decided by the run.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tilo_actions::remote::{
    ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt, InvocationState,
    InvokeRequest, Pending, pending,
};

use super::durable_local_invocation::LocalCaller;
use super::native_task_execution::ChildReask;
use super::published_invocation::InvocationCaller;
use super::{
    AdmittedJoinIdentity, CapnpBrokerState, CrdtRoute, InvocationOwner, PendingTaskRun,
    TaskRunEnvelope, TaskSubscription, mpsc,
};

/// The route a task agent's connection is admitted on: the workspace agent
/// service's, as a launched task agent joins.
pub const TASK_AGENT_APP: &str = "so.tilo.agent";

/// How an invocation of this connection is read back.
#[derive(Clone)]
enum Started {
    /// A child of the run: owned by the run.
    Child { wire: u64, owner: InvocationOwner },
    /// A host step, admitted by the run and taken by the task agent.
    HostStep,
}

struct Inner {
    state: Arc<CapnpBrokerState>,
    peer: u64,
    route: CrdtRoute,
    admitted: AdmittedJoinIdentity,
    agent: String,
    person: String,
    run: TaskRunEnvelope,
    /// Who the task agent is to the catalogue and the host steps, in this
    /// connection's execution.
    caller: Caller,
    started: Mutex<HashMap<tilo_actions::InvocationId, Started>>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        self.state.disconnect_native_task_stream(self.peer);
    }
}

/// One admitted connection of the agent door, working in its task's run.
#[derive(Clone)]
pub struct TaskRunHost(Arc<Inner>);

impl std::fmt::Debug for TaskRunHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskRunHost")
            .field("agent", &self.0.agent)
            .field("issue", &self.0.run.task_id)
            .finish_non_exhaustive()
    }
}

impl CapnpBrokerState {
    /// Join `presentation`, a task agent's (the device's certificate, the
    /// task delegation it signed and the task key's proof), and take its
    /// run: the going run of its issue the tracker holds for it.
    ///
    /// # Errors
    /// The presentation does not verify, its task does not go, or the
    /// tracker delivered it no run.
    pub fn task_run_host(self: &Arc<Self>, presentation: &[u8]) -> Result<TaskRunHost, String> {
        let decoded = runtime_collab::TaskAgentJoinIdentity::decode(presentation)
            .map_err(|error| format!("the task agent's presentation is malformed: {error}"))?
            .ok_or("the agent door joins only as a task agent")?;
        let room = decoded.workspace_id;
        let route = CrdtRoute::record(TASK_AGENT_APP, room.clone(), "");
        let admitted = super::verify_join_identity(
            &route,
            presentation,
            self.membership.as_ref(),
            self.trusted_devices.as_deref().map(Vec::as_slice),
            self.certificate_authority.get().map(Arc::as_ref),
        )?;
        let task = admitted.task.as_deref().ok_or("task delegation missing")?;
        let agent = task.agent_id.clone();
        self.admit_task_join(&room, &agent, &admitted)?;
        let peer = self.next_peer_id();
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.task_subscribers
            .lock()
            .map_err(|_| "task subscription registry unavailable")?
            .insert(
                peer,
                TaskSubscription {
                    agent_id: agent.clone(),
                    cluster_id: room.clone(),
                    tx,
                },
            );
        // Delivery is synchronous: a going run is in the channel now.
        self.refresh_tracker_runs(&std::iter::once(room.clone()).collect());
        let Ok(run) = rx.try_recv() else {
            self.disconnect_native_task_stream(peer);
            return Err("the task's run is not going, so it was delivered nothing".into());
        };
        let person = admitted.principal_id.clone();
        let caller = Caller::local_person(person.clone())
            .in_room(&room)
            .in_new_execution()
            .delegated_to(agent.clone());
        Ok(TaskRunHost(Arc::new(Inner {
            state: Arc::clone(self),
            peer,
            route,
            admitted,
            agent,
            person,
            run,
            caller,
            started: Mutex::default(),
        })))
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

    /// The room the run is in.
    #[must_use]
    pub fn room(&self) -> &str {
        &self.0.route.cluster_id
    }

    /// Whether the run still goes: its issue holds it going, for this
    /// agent. The connection's authority ends with it.
    ///
    /// # Errors
    /// Why it no longer goes.
    pub fn still_goes(&self) -> Result<(), String> {
        let task = self
            .0
            .admitted
            .task
            .as_deref()
            .ok_or("task delegation missing")?;
        self.0.state.task_goes(self.room(), task)
    }

    /// The run as the broker holds it for this connection, with its lease
    /// at the messages the agent took in.
    fn pending_run(&self) -> Result<(PendingTaskRun, String), String> {
        let state = &self.0.state;
        let run = state.task_run_for(
            self.0.peer,
            &self.0.agent,
            self.0.run.run_id,
            &self.0.run.lease,
        )?;
        let applied = state.applied_task_revision(&run)?;
        let (base, _) = runtime_collab::native_task_revision(&self.0.run.lease)
            .ok_or("invalid native task lease")?;
        Ok((run, format!("{base}:{applied}")))
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

    /// A host step, admitted by the run and taken by the task agent: its
    /// grants, the run's issue and its starter decide, and it goes to
    /// history in the run's group.
    fn invoke_host_step(
        &self,
        steps: &dyn crate::services::HostStepServices,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionHostError> {
        let refused = |reason: String| ActionHostError::Refused { reason };
        let state = &self.0.state;
        let (run, lease) = self.pending_run().map_err(refused)?;
        let argument = request.argument.to_slot();
        let descriptor = steps
            .invocation(&request.target.address, argument.as_deref())
            .ok_or(ActionHostError::UnknownAction)?;
        let mut binding = run.native.clone();
        binding.revision = runtime_collab::native_task_revision(&lease)
            .ok_or_else(|| refused("invalid native task lease".into()))?
            .1;
        let requirement = crate::services::NativeTaskActionRequirement {
            binding: binding.clone(),
            app: steps.app_id().to_owned(),
            resource_kind: descriptor.resource_kind.clone(),
            resource: descriptor.address.resource.clone(),
            action: descriptor.address.action.clone(),
            need: descriptor.contract.need(),
            consequential: descriptor.contract.asks_for_argument(argument.as_deref()),
            attended: descriptor.contract.is_attended(),
            confirmed: false,
        };
        state
            .admit_task_launch(&run, &binding, &requirement, &mut || Ok(()))
            .map_err(refused)?;
        let time = state
            .registered_policy_time()
            .map_err(refused)?
            .ok_or_else(|| refused("the policy clock is unavailable".into()))?;
        let origin = InvocationCaller::Local {
            route: self.0.route.clone(),
            admitted: Box::new(self.0.admitted.clone()),
            time: Some(time),
        };
        // The run's group, so history lists the step under the run.
        let mut request = request.clone();
        let mut context = request.context.take().unwrap_or_default();
        context.group.clone_from(&run.action_context.group);
        context.purpose.clone_from(&run.action_context.purpose);
        request.context = Some(context);
        let receipt = state.invoke_host_step(&self.0.caller, Some(&origin), steps, &request)?;
        self.remember(request.invocation, Started::HostStep);
        Ok(receipt)
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
        if let Err(reason) = self.still_goes() {
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
        // optional-decode: a provider that is no peer number names a launchable app
        let (peer_id, launch) = provider
            .parse::<u64>()
            .map_or((0, provider.to_owned()), |peer| (peer, String::new()));
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
            Started::Child { wire, owner } => {
                let update = self.0.state.invocation_status_for_owner(wire, &owner)?;
                Some(
                    InvocationState::new(*id, update)
                        .with_untrusted(self.0.state.untrusted_job(wire)),
                )
            }
            Started::HostStep => self.0.state.action_observe(&self.0.caller, id),
        }
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        self.check(caller).ok()?;
        match self.started(id)? {
            Started::Child { .. } => {
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
            Some(Started::Child { wire, owner }) => super::action_host::answered_when_durable(
                self.0
                    .state
                    .request_cancellation_for_owner(wire, &owner)
                    .map_err(|reason| ActionHostError::NotCancellable { reason }),
            ),
            Some(Started::HostStep) => self.0.state.action_cancel(&self.0.caller, id),
            None => Pending::ready(Err(ActionHostError::UnknownInvocation)),
        }
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
        let (run, lease) = self.pending_run().map_err(refused)?;
        let caller = self.local_caller().map_err(refused)?;
        let argument = request.argument.to_slot();
        let reask = ChildReask {
            peer_id: self.0.peer,
            agent_id: &self.0.agent,
            run_id: self.0.run.run_id,
            lease: &lease,
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
        let (wire, update) = self
            .0
            .state
            .invoke_native_task_child(&reask, None, (&self.0.route, &self.0.admitted))
            .await
            .map_err(|error| refused(error.to_string()))?;
        self.remember(
            request.invocation,
            Started::Child {
                wire,
                owner: InvocationOwner::NativeTask {
                    agent: run.agent_id.clone(),
                    task: run.task_id.clone(),
                    epoch: run.native.epoch,
                },
            },
        );
        Ok(InvocationState::new(request.invocation, update).receipt())
    }
}
