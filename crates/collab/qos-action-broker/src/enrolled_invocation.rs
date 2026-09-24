//! Concrete managed-descriptor installation and private enrolled RPC delivery.
use super::*;
use crate::enrolled_dispatch::{EnrolledConnection, EnrolledDispatchResult};

impl CapnpBrokerState {
    pub fn enrolled_recipient_ready(&self, channel: &str) -> bool {
        self.is_accepting() && self.enrolled.ready(channel)
    }

    pub(super) fn enrolled_action_catalogue_json(&self, peer: u64) -> Result<String, String> {
        let connection = self.enrolled.connection(peer)?;
        let grants = connection.grants()?;
        let json = self.action_catalogue_json_for(
            &connection.facts().binding.room,
            &BrokerActor::person(principal_id_for_root(connection.caller().user_public_key)),
            None,
        )?;
        filter_enrolled_catalogue(&json, &grants)
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
        let dispatch = connection.capture_task_action(
            run,
            lease,
            &surface.app_id,
            &surface.route.record_id,
            resource,
            action,
        )?;
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
        let registration = self
            .host_workers
            .lock()
            .map_err(|_| "worker registry unavailable")?
            .get(&peer)
            .and_then(|worker| worker.worker_registration.clone())
            .ok_or("task action target is not a registered host worker")?;
        let host = self.invocation_host()?;
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
                root: registration.certificate.user_public_key,
                device: registration.certificate.device_public_key,
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
            self.enrolled_connection()?;
            Ok(self.verified_identity.clone())
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
