//! Concrete managed-descriptor installation and private enrolled RPC delivery.
use super::*;
use crate::enrolled_dispatch::{EnrolledConnection, EnrolledDispatchResult};

impl CapnpBrokerState {
    /// Owner revocation (security review MUST-10): close every enrolled
    /// connection of `agent`. The host commits the revocation fact first.
    pub fn close_enrolled_agent(&self, agent: &str) -> usize {
        self.enrolled.close_agent(agent)
    }
    pub fn enrolled_recipient_ready(&self, channel: &str) -> bool {
        self.is_accepting() && self.enrolled.ready(channel)
    }

    pub(super) fn enrolled_action_catalogue_json(&self, peer: u64) -> Result<String, String> {
        let connection = self.enrolled.connection(peer)?;
        // Exactly the scopes the step authorization applies to the
        // connection's live task (frozen grants plus that task's opens).
        let scopes =
            self.enrolled_task_scopes(&connection, connection.active_task_id()?.as_deref())?;
        let json = self.action_catalogue_json_for(
            &connection.facts().binding.room,
            &BrokerActor::person(principal_id_for_root(connection.caller().user_public_key)),
            None,
        )?;
        // Scope-aware filter (v1 tuples stay exact degenerate scopes).
        crate::agent_control::scoped::filter_catalogue_by_scopes(&json, &scopes)
    }
    /// The one effective scope set of `task` on `connection`, shared by the
    /// enrolled catalogue and [`Self::authorize_enrolled_scoped`] so the two
    /// cannot drift. `None` (no accepted live task) adds no opened records.
    fn enrolled_task_scopes(
        &self,
        connection: &EnrolledConnection,
        task: Option<&str>,
    ) -> Result<Vec<qos_agent_control_protocol::GrantScope>, String> {
        enrolled_task_scopes(&connection.grants()?, self.membership.as_deref(), task)
    }
    /// The broker half of the shared per-step authorization (security review
    /// MUST-1): agent principal (role_of_agent_at) AND delegating owner, on
    /// the Action and on the route record's Artifact, scope coverage on the
    /// canonical live route record, explicit manifest class, frozen schema,
    /// confirmation and bulk refused unattended, argument bounds. The Store
    /// half runs in `capture_task_action`.
    fn authorize_enrolled_scoped(
        &self,
        connection: &EnrolledConnection,
        task: &str,
        surface: &PublishedSurface,
        action: &PublishedAction,
        argument: Option<&str>,
    ) -> Result<(), String> {
        let membership = self
            .membership
            .as_ref()
            .ok_or("workspace authority unavailable")?;
        // Frozen scopes, plus the records this task opened with a covered
        // open action (from the host's durable record of each open).
        let scopes = self.enrolled_task_scopes(connection, Some(task))?;
        let now = self.policy_now_ns()? / 1_000_000_000;
        let tree = membership.snapshot()?;
        let workspace = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == surface.route.cluster_id)
            .ok_or("workspace no longer hosted")?;
        let delegator = principal_id_for_root(connection.caller().user_public_key);
        crate::agent_control::scoped::authorize_published(
            workspace,
            now,
            qos_agent_control_protocol::InvokingActor::Agent {
                agent: &connection.facts().binding.agent,
                delegator: &delegator,
            },
            Some(&scopes),
            surface,
            action,
            argument,
        )
        .map(drop)
        .map_err(|refusal| refusal.to_string())
    }
    /// Takes the exact socket authenticated alongside this proof. A PID, stored
    /// process description, or arbitrary replacement socket cannot enter here.
    pub fn install_enrolled_connection(
        &self,
        admitted: launcher::managed::channel::AdmittedManagedConnection,
        caller: tilo_user_identity::DeviceCert,
    ) -> Result<String, String> {
        if !self.is_accepting() {
            return Err("broker unavailable".into());
        }
        let owner = self
            .services
            .enrolled_agents
            .clone()
            .ok_or("private agent Store adapter unavailable")?;
        let facts = admitted.process().facts()?;
        let room =
            uuid::Uuid::parse_str(&facts.binding.room).map_err(|_| "invalid managed room")?;
        self.certificate_authority
            .get()
            .ok_or("current certificate authority unavailable")?
            .authorize_join(room, &caller)?;
        let (process, stream) = admitted.into_parts();
        self.install_enrolled_process(
            crate::enrolled_dispatch::EnrolledProcess::managed(process),
            stream,
            caller,
            owner,
            facts,
        )
    }
    /// In-process enrolled Agent connection over a fixture process (test and
    /// `test-support` builds only). Same owner authorization, certificate
    /// authority join check, worker admission and RPC surface as production.
    #[cfg(any(test, feature = "test-support"))]
    pub fn install_fixture_enrolled_connection(
        &self,
        process: crate::enrolled_dispatch::EnrolledProcess,
        stream: std::os::unix::net::UnixStream,
        caller: tilo_user_identity::DeviceCert,
    ) -> Result<String, String> {
        if !self.is_accepting() {
            return Err("broker unavailable".into());
        }
        let owner = self
            .services
            .enrolled_agents
            .clone()
            .ok_or("private agent Store adapter unavailable")?;
        let facts = process.facts()?;
        let room =
            uuid::Uuid::parse_str(&facts.binding.room).map_err(|_| "invalid managed room")?;
        self.certificate_authority
            .get()
            .ok_or("current certificate authority unavailable")?
            .authorize_join(room, &caller)?;
        self.install_enrolled_process(process, stream, caller, owner, facts)
    }
    fn install_enrolled_process(
        &self,
        process: crate::enrolled_dispatch::EnrolledProcess,
        stream: std::os::unix::net::UnixStream,
        caller: tilo_user_identity::DeviceCert,
        owner: Arc<dyn crate::enrolled_dispatch::EnrolledAgentOwner>,
        facts: launcher::managed::channel::ChannelProcessFacts,
    ) -> Result<String, String> {
        let connection = EnrolledConnection::new(process, caller.clone(), owner)?;
        let mut workers = self
            .workers
            .lock()
            .map_err(|_| "worker admission unavailable")?;
        if workers.len() >= 32 {
            return Err("worker admission queue full".into());
        }
        stream.set_nonblocking(true).map_err(|e| e.to_string())?;
        workers.push(PendingWorker {
            stream,
            binding: WorkerBinding {
                host_scope: None,
                registration: None,
                route: CrdtRoute::app(crate::agent_control::APP_ID, &facts.binding.room),
                root: caller.user_public_key,
                device: caller.device_public_key,
                enrolled: Some(connection),
            },
        });
        Ok(facts.channel_id)
    }
    pub fn enrolled_task_dispatch(
        &self,
        channel: &str,
        invocation: uuid::Uuid,
    ) -> Result<crate::enrolled_dispatch::VerifiedEnrolledDispatch, String> {
        if !self.is_accepting() {
            return Err("broker unavailable".into());
        }
        self.enrolled.task_dispatch(channel, invocation)
    }
    pub fn dispatch_enrolled_task(
        &self,
        invocation: uuid::Uuid,
        caller: &tilo_user_identity::DeviceCert,
        envelope: runtime_collab::EnrolledTaskEnvelope,
    ) -> Result<EnrolledDispatchResult, String> {
        if !self.is_accepting() {
            return Err("broker unavailable".into());
        }
        self.enrolled.dispatch(invocation, caller, envelope)
    }
    /// Submit one step of an accepted enrolled task to the host step service.
    #[allow(
        clippy::too_many_arguments,
        reason = "explicit protocol inputs stay separate parameters"
    )]
    pub(super) fn enrolled_task_step(
        &self,
        peer: u64,
        run: u64,
        lease: &str,
        step: u32,
        target: u64,
        resource: &str,
        action: &str,
        argument: Option<String>,
    ) -> Result<tokio::sync::oneshot::Receiver<crate::enrolled_dispatch::EnrolledStepResult>, String>
    {
        let began = std::time::Instant::now();
        let connection = self.enrolled.connection(peer)?;
        let service = self
            .services
            .enrolled_steps
            .clone()
            .ok_or("enrolled task steps unavailable")?;
        let surface = self
            .surfaces
            .lock()
            .map_err(|_| "action publications unavailable")?
            .get(&target)
            .cloned()
            .filter(|s| s.live && s.route.cluster_id == connection.facts().binding.room)
            .ok_or("task action is no longer live")?;
        let offered = surface
            .actions
            .iter()
            .find(|a| a.resource == resource && a.action == action)
            .ok_or("task action is no longer published")?;
        let task_id = connection.task(run, lease)?.task_id.clone();
        self.authorize_enrolled_scoped(
            &connection,
            &task_id,
            &surface,
            offered,
            argument.as_deref(),
        )?;
        let dispatch = connection.capture_task_action(
            run,
            lease,
            &surface.app_id,
            &surface.route.record_id,
            resource,
            action,
        )?;
        crate::agent_timing::span(
            "step_captured",
            Some(dispatch.invocation()),
            began.elapsed(),
            &[("step", u64::from(step))],
        );
        service.submit(crate::enrolled_dispatch::EnrolledStepRequest {
            dispatch,
            step,
            peer: target,
            resource: resource.into(),
            action: action.into(),
            argument,
        })
    }
    pub(super) fn start_enrolled_task_action(
        &self,
        peer: u64,
        run: u64,
        lease: &str,
        target: u64,
        resource: &str,
        action: &str,
        argument: Option<&str>,
    ) -> Result<InvocationJob, String> {
        let connection = self.enrolled.connection(peer)?;
        let task = connection.task(run, lease)?;
        let surface = self
            .surfaces
            .lock()
            .map_err(|_| "action publications unavailable")?
            .get(&target)
            .cloned()
            .filter(|s| s.live && s.route.cluster_id == connection.facts().binding.room)
            .ok_or("task action is no longer live")?;
        let definition = surface
            .actions
            .iter()
            .find(|a| a.resource == resource && a.action == action)
            .ok_or("task action is no longer published")?;
        if !matches!(
            definition.contract.confirmation,
            tilo_actions::ConfirmationPolicy::None
        ) {
            return Err("enrolled task action requires interactive confirmation".into());
        }
        self.authorize_enrolled_scoped(&connection, &task.task_id, &surface, definition, argument)?;
        let grant =
            enrolled_action_grant(&surface.app_id, &surface.route.record_id, resource, action)?;
        let original = connection.capture_task_action(
            run,
            lease,
            &surface.app_id,
            &surface.route.record_id,
            resource,
            action,
        )?;
        let owner = enrolled_tool_owner(connection.caller(), &task);
        let context: tilo_actions::ActionContext =
            serde_json::from_value(task.task.action_context).map_err(|e| e.to_string())?;
        connection.reserve_action_slot()?;
        let job = self.start_invocation_confirmed_for_origin(
            target,
            resource,
            action,
            argument,
            &BrokerActor::person(principal_id_for_root(connection.caller().user_public_key)),
            None,
            Some(context),
            Some(owner),
            Some(InvocationCaller::Enrolled(original)),
        )?;
        connection.remember_action(job.id, task.message_id, grant)?;
        Ok(job)
    }
}
/// Exact immutable request of one enrolled task's child. Built by the host's
/// sender from its durable outbox row; never decoded from a wire request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnrolledChildRequest {
    pub invocation: [u8; 16],
    pub binding: runtime_collab::PublishedInvocationBinding,
    pub revision: String,
    pub resource: String,
    pub action: String,
    pub argument: Option<String>,
}
impl EnrolledChildRequest {
    fn intent(&self) -> serde_json::Value {
        let b = &self.binding;
        serde_json::json!({"enrolled_child":1,"binding":{"host":b.host,"peer_id":b.peer_id,
            "app_id":b.app_id,"record_id":b.record_id,"root":b.root,"device":b.device,
            "target_binding":b.target_binding},"revision":self.revision,
            "resource":self.resource,"action":self.action,"argument":self.argument})
    }
}
/// Retained journal ownership of one enrolled task's children (caller root
/// and device, task, run and channel), as recorded in the host's own ledger.
/// It is observation-only data for the host's recovery sender: it can read a
/// retained child row, never admit a new invocation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EnrolledChildOwner {
    pub root: [u8; 32],
    pub device: [u8; 32],
    pub task: String,
    pub run: String,
    pub channel: String,
    pub room: String,
}
fn child_result(
    record: &DurableInvocation,
) -> Result<runtime_collab::PublishedInvocationResult, String> {
    Ok(runtime_collab::PublishedInvocationResult {
        id: record.wire_id,
        update_json: serde_json::to_string(&record.update).map_err(|e| e.to_string())?,
        uncertain: record.uncertain,
        cancellation_requested: record.cancellation_requested,
    })
}
impl CapnpBrokerState {
    fn journal_record(&self, key: &InvocationKey) -> Result<Option<DurableInvocation>, String> {
        self.expire_stale_invocations();
        self.journal
            .lock()
            .map_err(|_| "invocation journal unavailable")?
            .as_ref()
            .map_err(Clone::clone)?
            .as_ref()
            .ok_or("durable admission unavailable")?
            .lookup(key)
    }
    /// Current exact target of an eager published action for an enrolled
    /// task child: host, worker registration identity and a stable revision
    /// of the offered action. A replaced worker or changed action differs.
    pub fn enrolled_child_binding(
        &self,
        dispatch: &crate::enrolled_dispatch::VerifiedEnrolledDispatch,
        peer: u64,
        resource: &str,
        action: &str,
    ) -> Result<(runtime_collab::PublishedInvocationBinding, String), String> {
        let facts = dispatch.revalidate()?;
        let surface = self
            .surfaces
            .lock()
            .map_err(|_| "action publications unavailable")?
            .get(&peer)
            .cloned()
            .filter(|s| s.live && s.route.cluster_id == facts.binding.room)
            .ok_or("task action is no longer live")?;
        let offered = surface
            .actions
            .iter()
            .find(|a| a.resource == resource && a.action == action)
            .ok_or("task action is no longer published")?;
        let host = self.invocation_host()?;
        // Targets: a registered host worker, or the broker's own desktop
        // command surface (opening an application, as a person does from the
        // launcher). Nothing else an app connection publishes qualifies.
        let desktop = *self
            .desktop_command_peer
            .lock()
            .map_err(|_| "desktop command registry unavailable")?
            == Some(peer)
            && surface.app_id == qos_peer_state::desktop_command::APP_ID;
        let (target, root, device) = if desktop {
            let target = blake3::derive_key(
                "qos enrolled child desktop target v1",
                &postcard::to_stdvec(&(peer, &surface.app_id, &surface.route.cluster_id))
                    .map_err(|e| e.to_string())?,
            );
            // The broker itself serves this target: its identity is derived
            // from the invocation host and room (never a registered worker).
            let identity = |label: &str| {
                blake3::derive_key(
                    label,
                    &[
                        host.as_bytes().as_slice(),
                        surface.route.cluster_id.as_bytes(),
                    ]
                    .concat(),
                )
            };
            (
                target,
                identity("qos enrolled child desktop root v1"),
                identity("qos enrolled child desktop device v1"),
            )
        } else {
            let registration = self
                .host_workers
                .lock()
                .map_err(|_| "worker registry unavailable")?
                .get(&peer)
                .and_then(|worker| worker.worker_registration.clone())
                .ok_or("task action target is not a registered host worker")?;
            let target = blake3::derive_key(
                "qos enrolled child eager target v1",
                &postcard::to_stdvec(&(
                    peer,
                    &registration.route.app_id,
                    &registration.route.cluster_id,
                    &registration.route.record_id,
                    registration.identity.as_bytes(),
                    registration.certificate.device_public_key,
                ))
                .map_err(|e| e.to_string())?,
            );
            (
                target,
                registration.certificate.user_public_key,
                registration.certificate.device_public_key,
            )
        };
        let revision = format!(
            "e1-{}",
            blake3::hash(
                &serde_json::to_vec(&serde_json::json!({
                    "resource": offered.resource, "action": offered.action,
                    "contract": offered.contract, "params": offered.params,
                }))
                .map_err(|e| e.to_string())?
            )
            .to_hex()
        );
        Ok((
            runtime_collab::PublishedInvocationBinding {
                host: *host.as_bytes(),
                peer_id: peer,
                app_id: surface.app_id.clone(),
                record_id: surface.route.record_id.clone(),
                root,
                device,
                target_binding: target,
            },
            revision,
        ))
    }
    /// Task-owned durable child invocation for an accepted enrolled task.
    /// The caller-owned UUID is journaled under the task's enrolled owner:
    /// a repeat with the same request returns the retained row (no second
    /// effect); a changed request is refused. Fresh admission rechecks the
    /// live task custody, its immutable grants and the exact current target.
    pub fn invoke_enrolled_child(
        &self,
        dispatch: &crate::enrolled_dispatch::VerifiedEnrolledDispatch,
        request: &EnrolledChildRequest,
    ) -> Result<Option<runtime_collab::PublishedInvocationResult>, String> {
        if !self.is_accepting() {
            return Err("broker unavailable".into());
        }
        let began = std::time::Instant::now();
        let result = self.invoke_enrolled_child_timed(dispatch, request);
        crate::agent_timing::span(
            "child_invoke",
            Some(dispatch.invocation()),
            began.elapsed(),
            &[],
        );
        result
    }
    fn invoke_enrolled_child_timed(
        &self,
        dispatch: &crate::enrolled_dispatch::VerifiedEnrolledDispatch,
        request: &EnrolledChildRequest,
    ) -> Result<Option<runtime_collab::PublishedInvocationResult>, String> {
        let connection = dispatch.live_connection()?;
        let envelope = dispatch.envelope().clone();
        let owner = enrolled_tool_owner(connection.caller(), &envelope);
        let host = self.invocation_host()?;
        if request.binding.host != *host.as_bytes() || request.invocation == [0; 16] {
            return Err("durable invocation host mismatch".into());
        }
        let invocation = tilo_actions::InvocationId(uuid::Uuid::from_bytes(request.invocation));
        let key = InvocationKey {
            room: connection.facts().binding.room.clone(),
            host,
            owner: owner.clone(),
            invocation,
        };
        let intent = request.intent();
        let existing =
            |state: &Self| -> Result<Option<runtime_collab::PublishedInvocationResult>, String> {
                match state.journal_record(&key)? {
                    Some(record) if record.intent["request"] != intent => {
                        Err("invocation identity reused with different intent".into())
                    }
                    Some(record) => child_result(&record).map(Some),
                    None => Ok(None),
                }
            };
        if let Some(result) = existing(self)? {
            return Ok(Some(result));
        }
        let (binding, revision) = self.enrolled_child_binding(
            dispatch,
            request.binding.peer_id,
            &request.resource,
            &request.action,
        )?;
        if binding != request.binding || revision != request.revision {
            return Err("original invocation target binding changed".into());
        }
        let task = &envelope.task;
        let surface = self
            .surfaces
            .lock()
            .map_err(|_| "action publications unavailable")?
            .get(&request.binding.peer_id)
            .cloned()
            .ok_or("task action is no longer live")?;
        let offered = surface
            .actions
            .iter()
            .find(|a| a.resource == request.resource && a.action == request.action)
            .cloned()
            .ok_or("task action is no longer published")?;
        if !matches!(
            offered.contract.confirmation,
            tilo_actions::ConfirmationPolicy::None
        ) {
            return Err("enrolled task action requires interactive confirmation".into());
        }
        self.authorize_enrolled_scoped(
            &connection,
            &envelope.task_id,
            &surface,
            &offered,
            request.argument.as_deref(),
        )?;
        let grant = enrolled_action_grant(
            &surface.app_id,
            &surface.route.record_id,
            &request.resource,
            &request.action,
        )?;
        let original = connection.capture_task_action(
            task.run_id,
            &task.lease,
            &surface.app_id,
            &surface.route.record_id,
            &request.resource,
            &request.action,
        )?;
        let context: tilo_actions::ActionContext =
            serde_json::from_value(task.action_context.clone()).map_err(|e| e.to_string())?;
        let actor = BrokerActor::person(principal_id_for_root(connection.caller().user_public_key));
        let _admission = self
            .admission_lock
            .lock()
            .map_err(|_| "invocation admission unavailable")?;
        if let Some(result) = existing(self)? {
            return Ok(Some(result));
        }
        connection.reserve_action_slot()?;
        let job = self.dispatch_invocation_from_offered_owned(
            request.binding.peer_id,
            (surface, offered),
            request.argument.as_deref(),
            &actor,
            None,
            Some(context),
            None,
            // An eager surface has no directory revision; the exact offered
            // action is bound by the request's own revision digest above.
            None,
            Some(owner),
            Some(InvocationCaller::Enrolled(original)),
            None,
            Some(EnrolledDurable {
                invocation,
                intent: intent.clone(),
            }),
        )?;
        connection.remember_action(job.id, envelope.message_id.clone(), grant)?;
        drop(job.completion);
        existing(self)?
            .ok_or_else(|| "admitted enrolled child missing from journal".into())
            .map(Some)
    }
    /// Retained row of an enrolled child for the host's recovery sender. It
    /// admits nothing; an absent row stays absent (never a new invocation).
    pub fn observe_enrolled_child(
        &self,
        owner: &EnrolledChildOwner,
        host: [u8; 16],
        invocation: [u8; 16],
    ) -> Result<Option<runtime_collab::PublishedInvocationResult>, String> {
        if !self.is_accepting() {
            return Err("broker unavailable".into());
        }
        let current = self.invocation_host()?;
        if host != *current.as_bytes() || invocation == [0; 16] {
            return Err("durable invocation host mismatch".into());
        }
        let key = InvocationKey {
            room: owner.room.clone(),
            host: current,
            owner: InvocationOwner::Enrolled {
                root: owner.root,
                device: owner.device,
                task: owner.task.clone(),
                run: owner.run.clone(),
                channel: owner.channel.clone(),
            },
            invocation: tilo_actions::InvocationId(uuid::Uuid::from_bytes(invocation)),
        };
        self.journal_record(&key)?
            .as_ref()
            .map(child_result)
            .transpose()
    }
}
impl CapnpChannelHandle {
    pub(super) fn enrolled_connection(&self) -> Result<Arc<EnrolledConnection>, capnp::Error> {
        let connection = self
            .state
            .enrolled
            .connection(self.peer_id)
            .map_err(capnp::Error::failed)?;
        let facts = connection.revalidate().map_err(capnp::Error::failed)?;
        let route = self.route();
        if !self.verified_identity.enrolled
            || route.app_id != crate::agent_control::APP_ID
            || route.cluster_id != facts.binding.room
            || !route.record_id.is_empty()
            || self.verified_identity.agent_id.as_deref() != Some(facts.binding.agent.as_str())
            || self.verified_identity.verified.user_public_key
                != connection.caller().user_public_key
            || self.verified_identity.verified.device_public_key
                != connection.caller().device_public_key
        {
            return Err(capnp::Error::failed(
                "enrolled RPC route or original caller mismatch".into(),
            ));
        }
        let room = uuid::Uuid::parse_str(&route.cluster_id)
            .map_err(|_| capnp::Error::failed("invalid managed room".into()))?;
        self.state
            .certificate_authority
            .get()
            .ok_or_else(|| {
                capnp::Error::failed("current certificate authority unavailable".into())
            })?
            .authorize_join(room, connection.caller())
            .map_err(capnp::Error::failed)?;
        connection.authorize().map_err(capnp::Error::failed)?;
        Ok(connection)
    }
    pub(super) fn task_identity(&self) -> Result<AdmittedJoinIdentity, capnp::Error> {
        if self.state.enrolled.connection(self.peer_id).is_ok() {
            let connection = self.enrolled_connection()?;
            // Re-derive the admitted identity (role included) from current
            // authority instead of returning the join-time copy.
            let mut current = super::verify_enrolled_join_identity(
                &self.route(),
                &connection,
                self.state.membership.as_ref(),
                self.state.certificate_authority.get().map(Arc::as_ref),
            )
            .map_err(capnp::Error::failed)?;
            current.agent_id = self.verified_identity.agent_id.clone();
            current.display_handle = self.verified_identity.display_handle.clone();
            current.enrolled = self.verified_identity.enrolled;
            Ok(current)
        } else {
            self.state
                .revalidate_admission(&self.route(), &self.verified_identity)
        }
    }
}

pub(super) struct EnrolledTaskStreamServer {
    pub(super) connection: Arc<EnrolledConnection>,
    pub(super) receiver: Rc<AsyncMutex<mpsc::Receiver<String>>>,
}
impl runtime_collab::raw::enrolled_task_stream::Server for EnrolledTaskStreamServer {
    fn read(
        self: Rc<Self>,
        _params: runtime_collab::raw::enrolled_task_stream::ReadParams,
        mut results: runtime_collab::raw::enrolled_task_stream::ReadResults,
    ) -> Promise<(), capnp::Error> {
        Promise::from_future(async move {
            self.connection.authorize().map_err(capnp::Error::failed)?;
            let json = self
                .receiver
                .lock()
                .await
                .recv()
                .await
                .ok_or_else(|| capnp::Error::disconnected("enrolled stream closed".into()))?;
            self.connection.authorize().map_err(capnp::Error::failed)?;
            results.get().set_task_json(&json);
            Ok(())
        })
    }
}

/// The canonical grant spelling never changes the actual executor's record route.
/// A literal record with the reserved spelling cannot alias primary-document authority.
/// Frozen grant scopes plus the records `task` opened, from the host's
/// durable record of each open. See
/// [`crate::agent_control::scoped::task_effective_scopes`].
fn enrolled_task_scopes(
    grants: &[qos_agent_control_protocol::GrantAction],
    authority: Option<&dyn crate::services::WorkspaceAuthority>,
    task: Option<&str>,
) -> Result<Vec<qos_agent_control_protocol::GrantScope>, String> {
    let opened = match (authority, task) {
        (Some(authority), Some(task)) => authority.task_opened_records(task)?,
        _ => Vec::new(),
    };
    crate::agent_control::scoped::task_effective_scopes(grants, &opened)
}
fn grant_record_id(record: &str) -> Option<&str> {
    match record {
        "" => Some("@qos:package-primary"),
        "@qos:package-primary" => None,
        other => Some(other),
    }
}
fn enrolled_action_grant(
    app: &str,
    record: &str,
    resource: &str,
    action: &str,
) -> Result<qos_agent_control_protocol::GrantAction, String> {
    Ok(qos_agent_control_protocol::GrantAction {
        app_id: app.into(),
        record_id: grant_record_id(record)
            .ok_or("reserved primary-record grant alias")?
            .into(),
        resource: resource.into(),
        action: action.into(),
    })
}

fn filter_enrolled_catalogue(
    json: &str,
    grants: &[qos_agent_control_protocol::GrantAction],
) -> Result<String, String> {
    let mut catalogue: serde_json::Value = serde_json::from_str(json).map_err(|e| e.to_string())?;
    let surfaces = catalogue
        .get_mut("surfaces")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or("invalid native catalogue")?;
    for surface in surfaces.iter_mut() {
        let app = surface
            .get("app_id")
            .and_then(serde_json::Value::as_str)
            .ok_or("catalogue app missing")?
            .to_owned();
        let record = surface
            .get("record_id")
            .and_then(serde_json::Value::as_str)
            .ok_or("catalogue record missing")?
            .to_owned();
        let live = surface.get("live").and_then(serde_json::Value::as_bool) == Some(true);
        let actions = surface
            .get_mut("actions")
            .and_then(serde_json::Value::as_array_mut)
            .ok_or("catalogue actions missing")?;
        let Some(grant_record) = grant_record_id(&record) else {
            actions.clear();
            continue;
        };
        actions.retain(|action| {
            live && grants.iter().any(|grant| {
                grant.app_id == app
                    && grant.record_id == grant_record
                    && action.get("resource").and_then(serde_json::Value::as_str)
                        == Some(grant.resource.as_str())
                    && action.get("action").and_then(serde_json::Value::as_str)
                        == Some(grant.action.as_str())
            })
        });
    }
    surfaces.retain(|surface| {
        surface
            .get("actions")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|actions| !actions.is_empty())
    });
    serde_json::to_string(&catalogue).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn enrolled_catalogue_primary_grant_preserves_raw_route_and_rejects_alias() {
        let json = serde_json::json!({"version":1,"surfaces":[
            {"app_id":"so.tilo.chat","record_id":"","live":true,"actions":[{"resource":"category","action":"collapse"}]},
            {"app_id":"so.tilo.chat","record_id":"other","live":true,"actions":[{"resource":"category","action":"collapse"}]},
            {"app_id":"so.tilo.chat","record_id":"@qos:package-primary","live":true,"actions":[{"resource":"category","action":"collapse"}]}
        ]});
        let grant = qos_agent_control_protocol::GrantAction {
            app_id: "so.tilo.chat".into(),
            record_id: "@qos:package-primary".into(),
            resource: "category".into(),
            action: "collapse".into(),
        };
        let remembered = enrolled_action_grant("so.tilo.chat", "", "category", "collapse").unwrap();
        assert_eq!(remembered, grant);
        assert!(
            enrolled_action_grant(
                "so.tilo.chat",
                "@qos:package-primary",
                "category",
                "collapse"
            )
            .is_err()
        );
        assert_eq!(
            enrolled_action_grant("so.tilo.chat", "other", "category", "collapse")
                .unwrap()
                .record_id,
            "other"
        );
        let result: serde_json::Value =
            serde_json::from_str(&filter_enrolled_catalogue(&json.to_string(), &[grant]).unwrap())
                .unwrap();
        assert_eq!(result["surfaces"].as_array().unwrap().len(), 1);
        assert_eq!(
            result["surfaces"][0]["record_id"], "",
            "grant sentinel selects only actual primary route and never rewrites it"
        );
    }
    #[test]
    fn enrolled_catalogue_requires_exact_current_live_grant_tuple() {
        let json = r#"{"version":1,"surfaces":[{"app_id":"notes","record_id":"one","live":true,"actions":[{"resource":"document","action":"insert"},{"resource":"document","action":"delete"}]},{"app_id":"notes","record_id":"two","live":true,"actions":[{"resource":"document","action":"insert"}]},{"app_id":"notes","record_id":"one","live":false,"actions":[{"resource":"document","action":"insert"}]}]}"#;
        let grant = qos_agent_control_protocol::GrantAction {
            app_id: "notes".into(),
            record_id: "one".into(),
            resource: "document".into(),
            action: "insert".into(),
        };
        let value: serde_json::Value =
            serde_json::from_str(&filter_enrolled_catalogue(json, &[grant]).unwrap()).unwrap();
        assert_eq!(value["surfaces"].as_array().unwrap().len(), 1);
        assert_eq!(
            value["surfaces"][0]["actions"],
            serde_json::json!([{"resource":"document","action":"insert"}])
        );
        let value: serde_json::Value =
            serde_json::from_str(&filter_enrolled_catalogue(json, &[]).unwrap()).unwrap();
        assert!(value["surfaces"].as_array().unwrap().is_empty());
    }

    mod opened_record_scopes {
        //! The enrolled catalogue lists exactly the operations the step
        //! authorization admits: both run on [`enrolled_task_scopes`].
        use super::super::enrolled_task_scopes;
        use crate::agent_control::scoped::{authorize_published, filter_catalogue_by_scopes};
        use crate::collab_broker_capnp::{PublishedAction, PublishedInput, PublishedSurface};
        use crate::policy::WorkspacePolicy;
        use crate::route::CrdtRoute;
        use crate::services::WorkspaceAuthority;
        use crate::test_support::{Workspace, WorkspaceId};
        use qos_agent_control_protocol::{
            DESKTOP_APP_ID, GrantAction, GrantScope, GrantVerb, InvokingActor, NOTES_APP_ID,
            OPEN_APP_ACTION, OPEN_APP_RESOURCE_PREFIX, OpenedRecord, OperationClass,
            PRIMARY_RECORD_ALIAS, PackagePin, RecordScope, ResourceScope, WRITER_APP_ID,
            scope_carrier,
        };
        use std::collections::{BTreeSet, HashMap};

        const ROOM: &str = "room-7";
        const OWNER: &str = "owner-person";
        const AGENT: &str = "agent-1";
        const TASK: &str = "task-a";
        const OTHER_TASK: &str = "task-b";
        const WRITER_RECORD: &str = "tab:0000000000000001";
        const OPENED: &str = "tab:00000000000000aa";
        const OTHER_TASK_OPENED: &str = "tab:00000000000000bb";
        const PERSON_OPENED: &str = "tab:00000000000000cc";

        /// Host durable opens, keyed by the task that opened them. A record a
        /// person opened has no task receipt at all.
        struct Opens(HashMap<&'static str, Vec<OpenedRecord>>);
        impl WorkspaceAuthority for Opens {
            fn snapshot(&self) -> Result<qos_desktop::WorkspaceTree, String> {
                Err("not used".into())
            }
            fn task_opened_records(&self, task: &str) -> Result<Vec<OpenedRecord>, String> {
                Ok(self.0.get(task).cloned().unwrap_or_default())
            }
        }
        fn opens() -> Opens {
            let notes = |record: &str| OpenedRecord {
                app_id: NOTES_APP_ID.into(),
                record_id: record.into(),
            };
            Opens(HashMap::from([
                (TASK, vec![notes(OPENED)]),
                (OTHER_TASK, vec![notes(OTHER_TASK_OPENED)]),
            ]))
        }
        fn pin(app: &str) -> PackagePin {
            let registry = crate::agent_control::scoped::delegation_registry();
            let manifest = registry.manifest(app).unwrap();
            PackagePin {
                package_id: manifest.package_id.clone(),
                version: manifest.version.clone(),
                schema_digest: crate::agent_control::scoped::schema_digest(app).unwrap(),
            }
        }
        fn writer_scope() -> GrantScope {
            GrantScope {
                app_id: WRITER_APP_ID.into(),
                record: RecordScope::Exact(WRITER_RECORD.into()),
                verb: GrantVerb::Write,
                classes: vec![OperationClass::Document, OperationClass::View],
                resource: ResourceScope::Any,
                actions: None,
                deny_actions: vec![],
                package: Some(pin(WRITER_APP_ID)),
                budgets: None,
                explicit_workspace_consent: false,
            }
        }
        fn desktop_open_scope() -> GrantScope {
            GrantScope {
                app_id: DESKTOP_APP_ID.into(),
                record: RecordScope::Exact(PRIMARY_RECORD_ALIAS.into()),
                classes: vec![OperationClass::AppOpen],
                resource: ResourceScope::Prefix(OPEN_APP_RESOURCE_PREFIX.into()),
                actions: Some(vec![OPEN_APP_ACTION.into()]),
                package: Some(pin(DESKTOP_APP_ID)),
                ..writer_scope()
            }
        }
        fn grants(scopes: &[GrantScope]) -> Vec<GrantAction> {
            scopes.iter().map(|s| scope_carrier(s).unwrap()).collect()
        }
        fn action(
            resource: &str,
            action: &str,
            kind: tilo_actions::OperationKind,
            confirm: bool,
        ) -> PublishedAction {
            let mut contract = tilo_actions::ActionContract::default();
            if confirm {
                contract.confirmation = tilo_actions::ConfirmationPolicy::Confirm;
            }
            PublishedAction {
                resource: resource.into(),
                action: action.into(),
                resource_kind: resource.into(),
                resource_label: resource.into(),
                action_label: action.into(),
                description: None,
                operation_kind: kind,
                params: vec![],
                contract,
                input: PublishedInput::None,
            }
        }
        fn surface(app: &str, record: &str, live: bool) -> PublishedSurface {
            use tilo_actions::OperationKind::{Document, Effect, View};
            let actions = if app == NOTES_APP_ID {
                vec![
                    action("notes.notebook", "new-page", Document, false),
                    action("notes.notebook", "replace-text", Document, false),
                    action("notes.notebook", "open", View, false),
                    // Host filesystem effect: outside every task scope.
                    action("notes.notebook", "export-markdown", Effect, false),
                    // Confirmation-requiring: refused unattended.
                    action("notes.notebook", "delete", Document, true),
                    // Not in the delegation manifest.
                    action("notes.notebook", "frobnicate", Document, false),
                ]
            } else {
                vec![
                    action("word-processor.document", "type-text", Document, false),
                    action("word-processor.document", "save", Effect, false),
                ]
            };
            PublishedSurface {
                peer_id: 9,
                app_id: app.into(),
                route: CrdtRoute::record(app, ROOM, record),
                actions,
                live,
            }
        }
        fn catalogue_json(surfaces: &[PublishedSurface]) -> String {
            let surfaces: Vec<_> = surfaces
                .iter()
                .map(|s| {
                    let actions: Vec<_> = s
                        .actions
                        .iter()
                        .map(|a| {
                            serde_json::json!({"resource":a.resource,"action":a.action,
                                "resource_kind":a.resource_kind,
                                "operation_kind":a.operation_kind,"contract":a.contract})
                        })
                        .collect();
                    serde_json::json!({"app_id":s.app_id,"record_id":s.route.record_id,
                        "live":s.live,"actions":actions})
                })
                .collect();
            serde_json::json!({"version":1,"surfaces":surfaces}).to_string()
        }
        type Op = (String, String, String);
        /// Asserts listed == authorized for `task` and returns that set.
        fn listed_equals_authorized(
            grants: &[GrantAction],
            task: Option<&str>,
            surfaces: &[PublishedSurface],
        ) -> BTreeSet<Op> {
            let authority = opens();
            let scopes = enrolled_task_scopes(grants, Some(&authority), task).unwrap();
            let catalogue: serde_json::Value = serde_json::from_str(
                &filter_catalogue_by_scopes(&catalogue_json(surfaces), &scopes).unwrap(),
            )
            .unwrap();
            let listed: BTreeSet<Op> = catalogue["surfaces"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|s| {
                    let record = s["record_id"].as_str().unwrap().to_owned();
                    s["actions"].as_array().unwrap().iter().map(move |a| {
                        (
                            record.clone(),
                            a["resource"].as_str().unwrap().to_owned(),
                            a["action"].as_str().unwrap().to_owned(),
                        )
                    })
                })
                .collect();
            let mut workspace = Workspace::test_default(WorkspaceId(7), "Room");
            workspace.set_owner(OWNER.to_string());
            let agent = InvokingActor::Agent {
                agent: AGENT,
                delegator: OWNER,
            };
            let authorized: BTreeSet<Op> = surfaces
                .iter()
                .flat_map(|s| s.actions.iter().map(move |a| (s, a)))
                .filter(|(s, a)| {
                    authorize_published(&workspace, 10, agent, Some(&scopes), s, a, None).is_ok()
                })
                .map(|(s, a)| {
                    (
                        s.route.record_id.clone(),
                        a.resource.clone(),
                        a.action.clone(),
                    )
                })
                .collect();
            assert_eq!(
                listed, authorized,
                "catalogue must equal the authorized set"
            );
            listed
        }
        fn op(record: &str, resource: &str, action: &str) -> Op {
            (record.into(), resource.into(), action.into())
        }
        fn all_surfaces(opened_live: bool) -> Vec<PublishedSurface> {
            vec![
                surface(WRITER_APP_ID, WRITER_RECORD, true),
                surface(NOTES_APP_ID, OPENED, opened_live),
                surface(NOTES_APP_ID, OTHER_TASK_OPENED, true),
                surface(NOTES_APP_ID, PERSON_OPENED, true),
            ]
        }
        fn notes_ops(set: &BTreeSet<Op>, record: &str) -> BTreeSet<String> {
            set.iter()
                .filter(|(r, _, _)| r == record)
                .map(|(_, _, a)| a.clone())
                .collect()
        }

        #[test]
        fn catalogue_lists_the_records_its_own_task_opened_exactly_as_authorized() {
            let frozen = grants(&[writer_scope(), desktop_open_scope()]);
            let set = listed_equals_authorized(&frozen, Some(TASK), &all_surfaces(true));
            assert_eq!(
                notes_ops(&set, OPENED),
                BTreeSet::from(["new-page".into(), "replace-text".into(), "open".into()]),
            );
            assert!(set.contains(&op(WRITER_RECORD, "word-processor.document", "type-text")));
            // Another task's open and a person's open never widen this task.
            assert!(notes_ops(&set, OTHER_TASK_OPENED).is_empty());
            assert!(notes_ops(&set, PERSON_OPENED).is_empty());
            // Outside both the frozen and the opened scopes: neither.
            for (record, resource, action) in [
                (OPENED, "notes.notebook", "export-markdown"),
                (OPENED, "notes.notebook", "delete"),
                (OPENED, "notes.notebook", "frobnicate"),
                (WRITER_RECORD, "word-processor.document", "save"),
            ] {
                assert!(!set.contains(&op(record, resource, action)));
            }
            // The other task sees its own open, not this one's.
            let other = listed_equals_authorized(&frozen, Some(OTHER_TASK), &all_surfaces(true));
            assert!(notes_ops(&other, OPENED).is_empty());
            assert!(notes_ops(&other, OTHER_TASK_OPENED).contains("new-page"));
        }

        #[test]
        fn ending_the_task_revoking_the_open_grant_or_closing_the_record_removes_them() {
            let frozen = grants(&[writer_scope(), desktop_open_scope()]);
            // No accepted live task (terminal or revoked): frozen scopes only.
            let set = listed_equals_authorized(&frozen, None, &all_surfaces(true));
            assert!(notes_ops(&set, OPENED).is_empty());
            assert!(set.contains(&op(WRITER_RECORD, "word-processor.document", "type-text")));
            // The open grant withdrawn: the durable open no longer counts.
            let set = listed_equals_authorized(
                &grants(&[writer_scope()]),
                Some(TASK),
                &all_surfaces(true),
            );
            assert!(notes_ops(&set, OPENED).is_empty());
            // The opened record closed (its surface is no longer live).
            let set = listed_equals_authorized(&frozen, Some(TASK), &all_surfaces(false));
            assert!(notes_ops(&set, OPENED).is_empty());
            assert!(set.contains(&op(WRITER_RECORD, "word-processor.document", "type-text")));
        }
    }
}

/// Host-set invoker of one admitted invocation (security review T15/T16,
/// MUST-11). Whatever invoker a caller or task envelope put in `context` is
/// discarded: the actor is the admitted `actor`, and the participant is
/// derived by the host from what it admitted, never read from the envelope:
/// - an enrolled task child: the participant of the owner's admitted task;
/// - a legacy agent task run: the participant of the host-built task
///   context's `task` group (the broker's own pending run record);
/// - anyone else: no participant (the actor's default view).
///
/// The application worker keeps the participant only for this exact actor
/// (`tilo_actions::ActionInvoker::delivered`), and the captured original
/// request binds it, so the worker's view of the invoker is exactly this.
pub(super) fn host_set_invoker(
    context: Option<tilo_actions::ActionContext>,
    actor: &BrokerActor,
    owner: &InvocationOwner,
) -> Result<Option<tilo_actions::ActionContext>, String> {
    let participant = match owner {
        InvocationOwner::Enrolled { task, .. } => {
            Some(tilo_actions::ParticipantId::for_agent_task(task)?)
        }
        _ if actor.is_agent => context
            .as_ref()
            .and_then(|context| context.group.as_ref())
            .filter(|group| group.kind == "task")
            .map(|group| tilo_actions::ParticipantId::for_agent_task(&group.id))
            .transpose()?,
        _ => None,
    };
    Ok(match (context, participant) {
        (None, None) => None,
        (context, participant) => Some(
            context
                .unwrap_or_default()
                .with_host_invoker(actor.id.clone(), participant),
        ),
    })
}

/// The participant a host-set context routes to (see [`host_set_invoker`]).
pub(super) fn invoker_participant(
    context: Option<&tilo_actions::ActionContext>,
) -> Option<tilo_actions::ParticipantId> {
    context?.invoker.as_ref()?.participant.clone()
}

#[cfg(test)]
mod invoker_tests {
    use super::*;

    const TASK: &str = "11111111-1111-4111-8111-111111111111";

    fn enrolled_owner() -> InvocationOwner {
        InvocationOwner::Enrolled {
            root: [1; 32],
            device: [2; 32],
            task: TASK.into(),
            run: TASK.into(),
            channel: TASK.into(),
        }
    }

    fn forged() -> tilo_actions::ActionContext {
        tilo_actions::ActionContext::default()
            .with_group(TASK, TASK, "task")
            .with_invoker(
                tilo_actions::ActionInvoker::actor("person:owner")
                    .with_participant(tilo_actions::ParticipantId::new("owner-window").unwrap()),
            )
    }

    #[test]
    fn forged_envelope_invoker_is_overwritten_for_an_enrolled_task() {
        let actor = BrokerActor::person("root:agent-caller");
        let context = host_set_invoker(Some(forged()), &actor, &enrolled_owner())
            .unwrap()
            .unwrap();
        let invoker = context.invoker.clone().unwrap();
        assert_eq!(invoker.actor, "root:agent-caller");
        assert_eq!(
            invoker.participant,
            Some(tilo_actions::ParticipantId::for_agent_task(TASK).unwrap())
        );
        // Everything else the host built is kept.
        assert_eq!(context.group.as_ref().map(|g| g.id.as_str()), Some(TASK));
        assert_eq!(
            invoker_participant(Some(&context)),
            invoker.participant,
            "the captured request and the delivered context agree"
        );
        // The worker derives exactly this invoker from the delivery.
        assert_eq!(
            tilo_actions::ActionInvoker::delivered(&actor.id, Some(&context)),
            invoker
        );
        // A task child with no context still gets its host-derived view.
        let bare = host_set_invoker(None, &actor, &enrolled_owner())
            .unwrap()
            .unwrap();
        assert_eq!(bare.invoker, Some(invoker));
    }

    #[test]
    fn a_person_never_gets_a_participant_from_the_envelope() {
        let actor = BrokerActor::person("person:guest");
        let owner = InvocationOwner::Local {
            actor: actor.id.clone(),
        };
        let context = host_set_invoker(Some(forged()), &actor, &owner)
            .unwrap()
            .unwrap();
        assert_eq!(
            context.invoker,
            Some(tilo_actions::ActionInvoker::actor("person:guest"))
        );
        assert_eq!(invoker_participant(Some(&context)), None);
        // No context stays no context: ordinary deliveries are unchanged.
        assert_eq!(host_set_invoker(None, &actor, &owner).unwrap(), None);
    }

    #[test]
    fn a_legacy_agent_run_routes_to_its_host_task_participant() {
        let actor = BrokerActor::agent("agent:alpha");
        let owner = InvocationOwner::Local {
            actor: actor.id.clone(),
        };
        let context = host_set_invoker(Some(forged()), &actor, &owner)
            .unwrap()
            .unwrap();
        assert_eq!(
            context.invoker,
            Some(
                tilo_actions::ActionInvoker::actor("agent:alpha")
                    .with_participant(tilo_actions::ParticipantId::for_agent_task(TASK).unwrap())
            )
        );
        let untasked = tilo_actions::ActionContext::default();
        let context = host_set_invoker(Some(untasked), &actor, &owner)
            .unwrap()
            .unwrap();
        assert_eq!(invoker_participant(Some(&context)), None);
    }
}

/// Retained ownership data derived only after the caller has validated the live task.
pub(super) fn enrolled_tool_owner(
    caller: &tilo_user_identity::DeviceCert,
    task: &runtime_collab::EnrolledTaskEnvelope,
) -> InvocationOwner {
    InvocationOwner::Enrolled {
        root: caller.user_public_key,
        device: caller.device_public_key,
        task: task.task_id.clone(),
        run: task.run_id.clone(),
        channel: task.channel_lease.clone(),
    }
}
