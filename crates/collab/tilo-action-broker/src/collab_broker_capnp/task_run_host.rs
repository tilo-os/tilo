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
    TaskSubscription, mpsc,
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
    /// A child the run is still admitting (an app it launches for it):
    /// accepted until its admission answers.
    Starting,
    /// A child the run refused, and why: what a caller that stopped
    /// waiting for the admission reads when it looks again.
    Refused(String),
}

/// The run a host works in, as the broker delivered it.
struct RunRef {
    run_id: u64,
    lease: String,
    task_id: String,
}

struct Inner {
    state: Arc<CapnpBrokerState>,
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
    /// Whether this host took the run's delivery itself (the agent door's
    /// connection), so the run's stream ends with it; a launched task
    /// agent's stream is its channel's.
    owns_stream: bool,
}

impl Drop for Inner {
    fn drop(&mut self) {
        if self.owns_stream {
            self.state.disconnect_native_task_stream(self.peer);
        }
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
        let run = RunRef {
            run_id: run.run_id,
            lease: run.lease,
            task_id: run.task_id,
        };
        Ok(TaskRunHost::new(
            self,
            (peer, route, admitted),
            agent,
            run,
            true,
        ))
    }

    /// The host a launched task agent's run takes its host steps through
    /// (a task's programs), the same the agent door's connection is: on the
    /// agent's own channel (`peer` on `route`, admitted as `admitted`), in
    /// its delivered run `run_id` at `lease`.
    pub(super) fn launched_task_run_host(
        self: &Arc<Self>,
        (peer, route, admitted): (u64, CrdtRoute, AdmittedJoinIdentity),
        (run_id, lease, task_id): (u64, &str, &str),
    ) -> Result<TaskRunHost, String> {
        let agent = admitted
            .task
            .as_deref()
            .map(|task| task.agent_id.clone())
            .or_else(|| admitted.agent_id.clone())
            .ok_or("a task's steps are its agent's")?;
        let run = RunRef {
            run_id,
            lease: lease.to_owned(),
            task_id: task_id.to_owned(),
        };
        Ok(TaskRunHost::new(
            self,
            (peer, route, admitted),
            agent,
            run,
            false,
        ))
    }
}

impl TaskRunHost {
    fn new(
        state: &Arc<CapnpBrokerState>,
        (peer, route, admitted): (u64, CrdtRoute, AdmittedJoinIdentity),
        agent: String,
        run: RunRef,
        owns_stream: bool,
    ) -> Self {
        let person = admitted.principal_id.clone();
        let caller = Caller::local_person(person.clone())
            .in_room(&route.cluster_id)
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
            owns_stream,
        }))
    }

    /// Who the task agent is to the host steps it takes in its run.
    pub(super) fn caller(&self) -> &Caller {
        &self.0.caller
    }

    pub(super) fn state(&self) -> Arc<CapnpBrokerState> {
        Arc::clone(&self.0.state)
    }

    /// Admit a step of the run on a record outside its app: `action` of the
    /// app `app` on `resource`, needing `kind`. The run decides it as it
    /// decides its other steps (its delegation, its issue's and its
    /// starter's live grants, only while it goes), for `caller`, this
    /// host's own execution.
    ///
    /// # Errors
    /// The caller is not this host's, the run no longer goes or is paused,
    /// or the run's grants refuse the step or would ask first.
    pub(super) fn admit_record_step(
        &self,
        caller: &Caller,
        (app, resource, action): (&str, &str, &str),
        kind: tilo_action_policy::ActionKind,
    ) -> Result<(), ActionHostError> {
        self.check(caller)?;
        let refused = |reason: String| ActionHostError::Refused { reason };
        let (run, lease) = self.pending_run().map_err(refused)?;
        let mut binding = run.native.clone();
        binding.revision = runtime_collab::native_task_revision(&lease)
            .ok_or_else(|| refused("invalid native task lease".into()))?
            .1;
        let requirement = crate::services::NativeTaskActionRequirement {
            binding: binding.clone(),
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
            .admit_task_launch(&run, &binding, &requirement, &mut || Ok(()))
            .map_err(|reason| {
                if crate::services::NativeTaskActionRequirement::awaits_confirmation(&reason) {
                    ActionHostError::ConfirmationRequired
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
        let run = self.0.state.task_run_for(
            self.0.peer,
            &self.0.agent,
            self.0.run.run_id,
            &self.0.run.lease,
        )?;
        let applied = self.0.state.applied_task_revision(&run)?;
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

    /// A host step, admitted by the run like any of its children (its
    /// grants, its issue and its starter decide), taken on the host by the
    /// task agent, and listed in history in the run's group. The one path
    /// every task agent takes a host step by: the agent door's connection
    /// and a launched task agent alike. The step is handed this host, so
    /// what it does in turn (a task's program: its actions, its reads and
    /// changes of records) is the run's too.
    pub(super) fn invoke_host_step(
        &self,
        steps: &dyn crate::services::HostStepServices,
        request: &InvokeRequest,
    ) -> Result<InvocationReceipt, ActionHostError> {
        let refused = |reason: String| ActionHostError::Refused { reason };
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
        self.0
            .state
            .admit_task_launch(&run, &binding, &requirement, &mut || Ok(()))
            .map_err(refused)?;
        let origin = self.origin().map_err(refused)?;
        // The run's group, so history lists the step under the run.
        let mut request = request.clone();
        let mut context = request.context.take().unwrap_or_default();
        context.group.clone_from(&run.action_context.group);
        context.purpose.clone_from(&run.action_context.purpose);
        request.context = Some(context);
        let receipt = self.0.state.invoke_host_step_in_run(
            &self.0.caller,
            Some(&origin),
            steps,
            &request,
            Some(self),
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
            Started::Child { wire, owner } => {
                let update = self.0.state.invocation_status_for_owner(wire, &owner)?;
                Some(
                    InvocationState::new(*id, update)
                        .with_untrusted(self.0.state.untrusted_job(wire)),
                )
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
            Some(Started::Child { wire, owner }) => super::action_host::answered_when_durable(
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
                // The run pauses for its person: the question goes on its
                // issue, as a launched agent puts it there, and the agent
                // asks again with the same arguments once they answered.
                if crate::services::NativeTaskActionRequirement::awaits_confirmation(&error)
                    && let Err(why) = self
                        .ask_on_issue(peer_id, request, argument.as_deref())
                        .await
                {
                    tracing::warn!(%why, "agent door: the run's question could not be put on its issue");
                }
                return Err(refused(error));
            }
        };
        if let Err(why) = self.settle_on_issue(request, argument.as_deref()).await {
            tracing::warn!(%why, "agent door: the run's answered question could not be settled");
        }
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

    /// The tracker's own action `action` on the run's issue, taken as a
    /// report of the run (a child of it, as a launched agent reports).
    async fn tracker_step(
        &self,
        action: &str,
        arguments: &serde_json::Value,
        invocation: tilo_actions::InvocationId,
    ) -> Result<(), String> {
        use crate::services::{TRACKER_APP, TRACKER_RESOURCE};
        let room = self.room().to_owned();
        let (peer_id, resource) = self
            .0
            .state
            .surfaces
            .lock()
            .map_err(|_| "task surface unavailable")?
            .values()
            .filter(|surface| {
                surface.live && surface.app_id == TRACKER_APP && surface.route.cluster_id == room
            })
            .find_map(|surface| {
                surface
                    .actions
                    .iter()
                    .find(|offered| {
                        offered.action == action
                            && tilo_actions::selection::logical_resource_id(&offered.resource)
                                == TRACKER_RESOURCE
                    })
                    .map(|offered| (surface.peer_id, offered.resource.clone()))
            })
            .ok_or("Tasks does not offer the run's issue here")?;
        let (_, lease) = self.pending_run()?;
        let caller = self.local_caller()?;
        let argument = arguments.to_string();
        let reask = ChildReask {
            peer_id: self.0.peer,
            agent_id: &self.0.agent,
            run_id: self.0.run.run_id,
            lease: &lease,
            invocation,
            target: runtime_collab::TaskAction {
                peer_id,
                provider: "",
                resource: &resource,
                action,
                argument: Some(&argument),
            },
            caller: &caller,
        };
        self.0
            .state
            .invoke_native_task_child(&reask, None, (&self.0.route, &self.0.admitted))
            .await
            .map(drop)
            .map_err(|error| error.to_string())
    }

    /// Put the question `request` waits on on the run's issue: its draft,
    /// as the action live on `peer_id` renders these arguments, for the
    /// person to answer there.
    async fn ask_on_issue(
        &self,
        peer_id: u64,
        request: &InvokeRequest,
        argument: Option<&str>,
    ) -> Result<(), String> {
        let (app, offered) = self
            .0
            .state
            .surfaces
            .lock()
            .map_err(|_| "task surface unavailable")?
            .get(&peer_id)
            .and_then(|surface| {
                surface
                    .action_projection(
                        &request.target.address.resource,
                        &request.target.address.action,
                    )
                    .map(|(surface, offered)| (surface.app_id, offered))
            })
            .ok_or("the action asked about is no longer published")?;
        let arguments = arguments_of(argument)?;
        let draft = tilo_actions::Draft::render(
            &offered.descriptor(tilo_actions::ExecutionLocus::ClientLocal),
            &arguments,
        );
        let id = question_id(request, &arguments);
        let question = serde_json::json!({
            "id": id,
            "app": app,
            "resource": tilo_actions::selection::logical_resource_id(&request.target.address.resource),
            "action": request.target.address.action,
            "arguments_digest": hex::encode(tilo_actions::draft::arguments_digest(&arguments)),
            "draft": draft,
        });
        self.tracker_step(
            "ask-confirmation",
            &serde_json::json!({ "issue": issue_id(self.issue()), "question": question }),
            derived_invocation("ask", &id),
        )
        .await
    }

    /// Spend the person's answer to the run's question about `request`, now
    /// that the confirmed step started. Nothing when it asked nothing.
    async fn settle_on_issue(
        &self,
        request: &InvokeRequest,
        argument: Option<&str>,
    ) -> Result<(), String> {
        let (run, _) = self.pending_run()?;
        let current = self.0.state.tracker_run_of(&run)?;
        let arguments = arguments_of(argument)?;
        let digest = hex::encode(tilo_actions::draft::arguments_digest(&arguments));
        let Some(question) = current.question.filter(|question| {
            question.action == request.target.address.action
                && question.arguments_digest == digest
                && question.answer != crate::services::TrackerAnswer::Waiting
        }) else {
            return Ok(());
        };
        self.tracker_step(
            "settle-confirmation",
            &serde_json::json!({ "issue": issue_id(self.issue()), "id": question.id }),
            derived_invocation("settle", &question.id),
        )
        .await
    }
}

/// The JSON argument map an invocation's slot carries.
fn arguments_of(
    argument: Option<&str>,
) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    let Some(text) = argument else {
        return Ok(serde_json::Map::new());
    };
    let json = text
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .unwrap_or(text);
    serde_json::from_str(json).map_err(|_| "the action's arguments are not a JSON map".into())
}

/// The issue's id, as the tracker's actions name it.
fn issue_id(issue: &str) -> &str {
    issue
        .strip_prefix(crate::services::TRACKER_ISSUE_PREFIX)
        .unwrap_or(issue)
}

/// The id of the question about `request` with `arguments`: the same each
/// time the step asks, so asking again finds the person's answer.
fn question_id(
    request: &InvokeRequest,
    arguments: &serde_json::Map<String, serde_json::Value>,
) -> String {
    let key = format!(
        "{}\0{}\0{}",
        tilo_actions::selection::logical_resource_id(&request.target.address.resource),
        request.target.address.action,
        hex::encode(tilo_actions::draft::arguments_digest(arguments))
    );
    blake3::hash(key.as_bytes())
        .to_hex()
        .as_str()
        .chars()
        .take(32)
        .collect()
}

/// A stable invocation id for the run's report `what` about question `id`,
/// so asking or settling twice is one report.
fn derived_invocation(what: &str, id: &str) -> tilo_actions::InvocationId {
    let digest = blake3::hash(format!("agent-door-{what}\0{id}").as_bytes());
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest.as_bytes()[..16]);
    tilo_actions::InvocationId(uuid::Builder::from_random_bytes(bytes).into_uuid())
}
