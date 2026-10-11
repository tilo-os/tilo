//! Original invocation custody, minted only by dispatch and broker-private app custody.
//! Launcher-attested Chat connections retain live connection/process provenance;
//! managed Agent channels do not gain Person publisher authority through this door.
use super::{
    Actor, AdmittedJoinIdentity, Arc, CapDecision, CapnpBrokerState, CrdtRoute,
    PrivateWorkerRegistration, PublishedAction, PublishedInput, ResourceRef, Verb,
    VerifiedActionCaller, Weak,
};
use ui::invocation_scope::{
    PublishedInvocationAuthorizer, PublishedInvocationLease, PublishedInvocationRequest,
    PublishedInvocationSpec,
};

#[derive(Clone)]
pub(super) enum InvocationCaller {
    NativePerson {
        invocation: tilo_actions::InvocationId,
        room: String,
        certificate: tilo_user_identity::DeviceCert,
        time: crate::peer_actions::PolicyTime,
    },
    Certified {
        caller: Box<VerifiedActionCaller>,
        // A host-owned restriction may be stricter than the signed certificate.
        // Retain it as a restriction, never by modifying certificate evidence.
        foreign_restriction: bool,
    },
    Local {
        route: CrdtRoute,
        admitted: Box<AdmittedJoinIdentity>,
    },
}
#[derive(Clone)]
pub(super) struct OriginalInvocationData {
    request: PublishedInvocationRequest,
    route: CrdtRoute,
    spec: PublishedInvocationSpec,
    resource_identity: Option<tilo_actions::ResourceSemanticIdentity>,
    caller: InvocationCaller,
    time: crate::peer_actions::PolicyTime,
    execution: Option<tilo_actions::remote::ExecutionLease>,
    semantic: Option<tilo_actions::ResourceSemanticIdentity>,
}

enum InspectionPrincipal {
    Person {
        certificate: tilo_user_identity::DeviceCert,
        foreign_restriction: bool,
    },
    Agent(String),
}

impl OriginalInvocationData {
    fn current_local_caller(
        &self,
        state: &CapnpBrokerState,
        route: &CrdtRoute,
        admitted: &AdmittedJoinIdentity,
    ) -> Result<AdmittedJoinIdentity, String> {
        if route.cluster_id != self.route.cluster_id {
            return Err("original local caller room changed".into());
        }
        state
            .revalidate_admission(route, admitted)
            .map_err(|error| error.to_string())
    }

    pub(super) fn is_addressed_message(&self) -> bool {
        self.route.app_id == "so.tilo.chat"
            && self.route.record_id.is_empty()
            && self.request.address.action == "message"
            && self.spec.kind == tilo_actions::OperationKind::Document
            && self
                .semantic
                .as_ref()
                .is_some_and(|identity| identity.kind == "chat.agent")
    }

    pub(super) fn message_facts(
        &self,
        state: &CapnpBrokerState,
        envelope: &super::InvocationEnvelope,
    ) -> Result<super::workspace_messages::OriginalMessageFacts, String> {
        if !self.is_addressed_message() {
            return Err("original invocation is not canonical addressed Message".into());
        }
        let caller = self.owner_person_certificate(state, &self.route.cluster_id, envelope)?;
        let semantic = self
            .semantic
            .as_ref()
            .ok_or("original Message semantic target unavailable")?;
        let agent = message_agent_from_semantic_key(&semantic.key)
            .ok_or("original Message semantic target is not canonical")?;
        Ok(super::workspace_messages::OriginalMessageFacts {
            agent: agent.to_owned(),
            request: self.request.clone(),
            caller,
            operation: tilo_joined_room::AgentMessageOperationData {
                app: self.route.app_id.clone(),
                resource: self.request.address.resource.clone(),
                action: self.request.address.action.clone(),
                contract: self.spec.contract.clone(),
                foreign_restriction: matches!(
                    &self.caller,
                    InvocationCaller::Certified {
                        foreign_restriction: true,
                        ..
                    }
                ),
            },
            spec: self.spec.clone(),
        })
    }

    pub(super) fn display_caller(
        self: &Arc<Self>,
        state: &Arc<CapnpBrokerState>,
        peer: u64,
        worker: &PrivateWorkerRegistration,
        mutates: bool,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        if self.route != worker.route
            || !self.spec.contract.permission.covers(if mutates {
                tilo_actions::PermissionRequirement::Admin
            } else {
                tilo_actions::PermissionRequirement::Read
            })
        {
            return Err("The original invocation does not admit display administration.".into());
        }
        let original = OriginalInvocation {
            broker: Arc::downgrade(state),
            peer,
            worker: TargetCustody::Registered(Box::new(worker.clone())),
            data: self.clone(),
        };
        original.check_lifecycle()?;
        if mutates && original.agent_id().is_some() {
            return Err("An Agent invocation cannot administer device displays.".into());
        }
        let caller = original.certificate()?.clone();
        drop(original);
        if mutates
            && (caller.user_public_key != worker.certificate.user_public_key
                || caller.device_public_key != worker.certificate.device_public_key)
        {
            return Err("The original caller does not own this Settings device channel.".into());
        }
        Ok(caller)
    }

    pub(super) fn permits_clipboard_command(
        &self,
        command: &runtime_collab::file_clipboard::Command,
    ) -> bool {
        use runtime_collab::file_clipboard::Command;
        let action = self.request.address.action.as_str();
        match command {
            Command::Publish { cut: false, .. } => {
                self.spec.kind == tilo_actions::OperationKind::Effect
                    && matches!(action, "copy" | "copy-selection")
            }
            Command::Publish { cut: true, .. } => {
                self.spec.kind == tilo_actions::OperationKind::Effect
                    && matches!(action, "cut" | "cut-selection")
            }
            Command::Paste { .. } => {
                self.spec.kind == tilo_actions::OperationKind::Effect && action == "paste"
            }
            Command::RestoreTrash { .. } => {
                self.spec.kind == tilo_actions::OperationKind::Effect && action == "restore"
            }
            Command::Observe { .. } | Command::Cancel { .. } => {
                matches!(
                    action,
                    "paste" | "restore" | "undo" | "redo" | "recover-clipboard-transfers"
                )
            }
            Command::Undo { .. } => self.clipboard_compensation() && action == "undo",
            Command::Redo { .. } => self.clipboard_compensation() && action == "redo",
            Command::Recover { .. } => {
                self.spec.kind == tilo_actions::OperationKind::Effect
                    && action == "recover-clipboard-transfers"
            }
            Command::Capture { .. } | Command::Release { .. } => {
                self.spec.kind == tilo_actions::OperationKind::Effect
                    && matches!(
                        action,
                        "copy" | "copy-selection" | "cut" | "cut-selection" | "paste"
                    )
            }
            Command::Snapshot => true,
        }
    }

    pub(super) fn clipboard_compensation(&self) -> bool {
        self.spec.kind == tilo_actions::OperationKind::Document
            && matches!(self.request.address.action.as_str(), "undo" | "redo")
    }

    pub(super) fn clipboard_action(&self) -> &str {
        &self.request.address.action
    }
    pub(super) fn clipboard_resource(&self) -> &str {
        &self.request.address.resource
    }

    pub(super) fn revalidate_clipboard_origin(
        &self,
        state: &CapnpBrokerState,
    ) -> Result<(), String> {
        state.checked_policy_time(Some(&self.time))?;
        match &self.caller {
            InvocationCaller::NativePerson {
                room, certificate, ..
            } => {
                if room != &self.route.cluster_id {
                    return Err("original native caller route changed".into());
                }
                state.current_native_person(room, certificate)?;
            }
            InvocationCaller::Certified { caller, .. } => {
                state.current_action_caller(caller)?;
            }
            InvocationCaller::Local { route, admitted } => {
                state
                    .revalidate_admission(route, admitted)
                    .map_err(|error| error.to_string())?;
            }
        }
        Ok(())
    }
    pub(super) fn permits_clipboard(&self, needed: tilo_actions::PermissionRequirement) -> bool {
        self.spec.contract.permission.covers(needed)
            && self
                .execution
                .as_ref()
                .is_none_or(tilo_actions::remote::ExecutionLease::is_live)
    }

    fn current_person_certificate(
        &self,
        state: &CapnpBrokerState,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        if self
            .execution
            .as_ref()
            .is_some_and(|lease| !lease.is_live())
        {
            return Err("original invocation execution session ended".into());
        }
        let (_, now) = state.checked_policy_time(Some(&self.time))?;
        let certificate = match &self.caller {
            InvocationCaller::NativePerson {
                room, certificate, ..
            } => {
                if room != &self.route.cluster_id {
                    return Err("original native caller route changed".into());
                }
                state.current_native_person(room, certificate)?;
                certificate.clone()
            }
            InvocationCaller::Certified { caller, .. } => {
                let admitted = state.current_action_caller(caller)?;
                if admitted.agent_id.is_some() {
                    return Err("this action requires an authenticated person".into());
                }
                admitted
                    .certificate
                    .ok_or("original person certificate unavailable")?
            }
            InvocationCaller::Local { route, admitted } => {
                let admitted = self.current_local_caller(state, route, admitted)?;
                if admitted.agent_id.is_some() {
                    return Err("this action requires an authenticated person".into());
                }
                admitted
                    .certificate
                    .ok_or("original person certificate unavailable")?
            }
        };
        tilo_user_identity::verify_device_cert_at(&certificate, now)
            .map_err(|_| "original invocation certificate expired")?;
        let mut person = state.certified_person(&self.route.cluster_id, &certificate);
        if let InvocationCaller::Certified {
            foreign_restriction,
            ..
        } = &self.caller
        {
            person.capped |= super::cap_applies_in(
                state.certificate_authority.get().map(AsRef::as_ref),
                &self.route.cluster_id,
                *foreign_restriction,
                Some(&certificate.device_public_key),
            );
        }
        let seconds = now / 1_000_000_000;
        let verb = super::permission_verb(self.spec.contract.permission);
        state
            .membership
            .as_ref()
            .ok_or("workspace authority unavailable")?
            .decide_in_room(&self.route.cluster_id, |workspace| {
                let workspace = workspace.ok_or("original invocation workspace unavailable")?;
                let actor = person.policy_in(workspace, seconds);
                if verb.is_some_and(|verb| {
                    workspace.decide_at(
                        &actor,
                        &ResourceRef::Action {
                            app: self.route.app_id.clone(),
                            resource: self.request.address.resource.clone(),
                            action: self.request.address.action.clone(),
                        },
                        verb,
                        seconds,
                    ) != CapDecision::Allow
                }) {
                    return Err("original invocation caller is no longer authorized");
                }
                Ok(())
            })??;
        Ok(certificate)
    }

    /// Recover a person's original certified custody for a native owner turn.
    /// This is not an allow decision: the owner rechecks current room rights.
    pub(super) fn owner_person_certificate(
        &self,
        state: &CapnpBrokerState,
        room: &str,
        envelope: &super::InvocationEnvelope,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        if self.route.cluster_id != room
            || self.request.wire_id != envelope.id
            || self.request.invocation != envelope.invocation
            || self.request.address.resource != envelope.resource
            || self.request.address.action != envelope.action
            || self.request.argument != envelope.argument
            || self
                .execution
                .as_ref()
                .is_some_and(|lease| !lease.is_live())
        {
            return Err("owner invocation original binding changed".into());
        }
        self.current_person_certificate(state)
    }

    /// Revalidate an immutable View request from either an admitted person or
    /// Agent immediately before a broker-owned inspection answers it.
    pub(super) fn revalidate_participant_inspection(
        &self,
        state: &CapnpBrokerState,
        room: &str,
        envelope: &super::InvocationEnvelope,
    ) -> Result<(), String> {
        if self.route.cluster_id != room
            || self.request.wire_id != envelope.id
            || self.request.invocation != envelope.invocation
            || self.request.address.resource != envelope.resource
            || self.request.address.action != envelope.action
            || self.request.argument != envelope.argument
            || self
                .execution
                .as_ref()
                .is_some_and(|lease| !lease.is_live())
        {
            return Err("owner invocation original binding changed".into());
        }
        let (_, now) = state.checked_policy_time(Some(&self.time))?;
        let principal = self.current_inspection_principal(state, room)?;
        let tree = state
            .membership
            .as_ref()
            .ok_or("workspace authority unavailable")?
            .snapshot()?;
        let workspace = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == room)
            .ok_or("original invocation workspace unavailable")?;
        let seconds = now / 1_000_000_000;
        let actor = match principal {
            InspectionPrincipal::Agent(agent) => {
                workspace
                    .role_of_agent_in_room_at(&agent, room, seconds)
                    .ok_or("original agent membership expired")?;
                Actor::agent(agent).in_room(room.to_owned())
            }
            InspectionPrincipal::Person {
                certificate,
                foreign_restriction,
            } => {
                let mut person = state.certified_person(room, &certificate);
                person.capped |= super::cap_applies_in(
                    state.certificate_authority.get().map(AsRef::as_ref),
                    room,
                    foreign_restriction,
                    Some(&certificate.device_public_key),
                );
                person.policy_in(workspace, seconds)
            }
        };
        let verb = match self.spec.contract.permission {
            tilo_actions::PermissionRequirement::None => None,
            tilo_actions::PermissionRequirement::Read => Some(Verb::Read),
            tilo_actions::PermissionRequirement::Comment => Some(Verb::Comment),
            tilo_actions::PermissionRequirement::Write => Some(Verb::Write),
            tilo_actions::PermissionRequirement::Invite => Some(Verb::Invite),
            tilo_actions::PermissionRequirement::Admin => Some(Verb::Admin),
        };
        if verb.is_some_and(|verb| {
            workspace.decide_at(
                &actor,
                &ResourceRef::Action {
                    app: self.route.app_id.clone(),
                    resource: self.request.address.resource.clone(),
                    action: self.request.address.action.clone(),
                },
                verb,
                seconds,
            ) != CapDecision::Allow
        }) {
            return Err("original invocation caller is no longer authorized".into());
        }
        Ok(())
    }

    fn current_inspection_principal(
        &self,
        state: &CapnpBrokerState,
        room: &str,
    ) -> Result<InspectionPrincipal, String> {
        Ok(match &self.caller {
            InvocationCaller::NativePerson {
                room: caller_room,
                certificate,
                ..
            } => {
                if caller_room != room {
                    return Err("original native caller route changed".into());
                }
                state.current_native_person(room, certificate)?;
                InspectionPrincipal::Person {
                    certificate: certificate.clone(),
                    foreign_restriction: false,
                }
            }
            InvocationCaller::Certified {
                caller,
                foreign_restriction,
            } => {
                let admitted = state.current_action_caller(caller)?;
                if let Some(agent) = admitted.agent_id {
                    InspectionPrincipal::Agent(agent)
                } else {
                    InspectionPrincipal::Person {
                        certificate: admitted
                            .certificate
                            .ok_or("original person certificate unavailable")?,
                        foreign_restriction: *foreign_restriction,
                    }
                }
            }
            InvocationCaller::Local { route, admitted } => {
                let admitted = self.current_local_caller(state, route, admitted)?;
                if let Some(agent) = admitted.agent_id {
                    InspectionPrincipal::Agent(agent)
                } else {
                    InspectionPrincipal::Person {
                        certificate: admitted
                            .certificate
                            .ok_or("original person certificate unavailable")?,
                        foreign_restriction: false,
                    }
                }
            }
        })
    }

    pub(super) fn permits_launch_in(&self, room: &str) -> bool {
        self.route.cluster_id == room
            && self
                .spec
                .contract
                .permission
                .covers(tilo_actions::PermissionRequirement::Write)
            && self
                .execution
                .as_ref()
                .is_none_or(tilo_actions::remote::ExecutionLease::is_live)
    }

    pub(super) fn launch_person_certificate(
        &self,
        state: &CapnpBrokerState,
        room: &str,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        if !self.permits_launch_in(room) {
            return Err("original invocation cannot authorize this launch".into());
        }
        self.current_person_certificate(state)
    }

    pub(super) fn launch_administrator_certificate(
        &self,
        state: &CapnpBrokerState,
        room: &str,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        let certificate = self.launch_person_certificate(state, room)?;
        let (_, now) = state.checked_policy_time(Some(&self.time))?;
        let mut actor = state.certified_person(room, &certificate);
        if let InvocationCaller::Certified {
            foreign_restriction,
            ..
        } = &self.caller
        {
            actor.capped |= super::cap_applies_in(
                state.certificate_authority.get().map(AsRef::as_ref),
                room,
                *foreign_restriction,
                Some(&certificate.device_public_key),
            );
        }
        let seconds = now / 1_000_000_000;
        state
            .membership
            .as_ref()
            .ok_or("workspace authority unavailable")?
            .decide_in_room(room, |workspace| {
                let workspace = workspace.ok_or("original launch workspace unavailable")?;
                if workspace.decide_at(
                    &actor.policy_in(workspace, seconds),
                    &ResourceRef::Workspace,
                    Verb::Admin,
                    seconds,
                ) != CapDecision::Allow
                {
                    return Err("original launch caller cannot delegate application authorship");
                }
                Ok(())
            })??;
        Ok(certificate)
    }

    pub(super) fn capture(
        request: PublishedInvocationRequest,
        route: CrdtRoute,
        offered: &PublishedAction,
        revision: Option<&str>,
        fresh_binding: Option<&tilo_actions::remote::FreshActionBinding>,
        caller: Option<InvocationCaller>,
        time: Option<crate::peer_actions::PolicyTime>,
        execution: Option<tilo_actions::remote::ExecutionLease>,
    ) -> Option<Arc<Self>> {
        let spec = PublishedInvocationSpec {
            kind: offered.operation_kind,
            contract: offered.contract.clone(),
            params: offered.params.clone(),
            input: match &offered.input {
                PublishedInput::None => tilo_actions::InputSpec::None,
                PublishedInput::Text {
                    prompt,
                    placeholder,
                    wants_content,
                } => tilo_actions::InputSpec::Text {
                    prompt: prompt.clone(),
                    placeholder: placeholder.clone(),
                    collects: if *wants_content {
                        tilo_actions::Collects::Content
                    } else {
                        tilo_actions::Collects::Words
                    },
                },
            },
            directory_revision: revision.map(str::to_owned),
            fresh_binding: fresh_binding.cloned(),
        };
        Some(Arc::new(Self {
            request,
            route,
            spec,
            resource_identity: offered.semantic_identity.clone(),
            caller: caller?,
            time: time?,
            execution,
            semantic: offered.semantic_identity.clone(),
        }))
    }
}

fn message_agent_from_semantic_key(key: &str) -> Option<&str> {
    key.strip_prefix("chat.person:")
        .filter(|agent| !agent.is_empty())
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovedCheckArguments {
    check_id: String,
}

enum TargetCustody {
    Registered(Box<PrivateWorkerRegistration>),
    Native {
        connection: Arc<super::native_media::LocalAppConnection>,
        admitted: Box<AdmittedJoinIdentity>,
    },
}

/// Immutable original facts; there is no public constructor or deserializer.
/// This token is not an allow decision. The final owner must recheck its current
/// certificate, membership and policy under its held guard.
pub struct OriginalInvocation {
    broker: Weak<CapnpBrokerState>,
    peer: u64,
    worker: TargetCustody,
    data: Arc<OriginalInvocationData>,
}
impl OriginalInvocation {
    pub(super) fn resource_identity(
        &self,
    ) -> Result<&tilo_actions::ResourceSemanticIdentity, String> {
        self.data
            .resource_identity
            .as_ref()
            .filter(|identity| identity.is_valid())
            .ok_or_else(|| "accepted resource semantic identity unavailable".into())
    }
    pub(super) fn check_resource_publication(
        &self,
        state: &CapnpBrokerState,
    ) -> Result<(), String> {
        let identity = self.resource_identity()?;
        let (surface, offered) = state
            .published_action(
                &self.data.route,
                &self.data.request.address.resource,
                &self.data.request.address.action,
            )
            .ok_or("accepted resource publication retired")?;
        if !surface.live
            || surface.peer_id != self.peer
            || offered.semantic_identity.as_ref() != Some(identity)
        {
            return Err(
                "accepted resource semantic identity changed or publication replaced".into(),
            );
        }
        Ok(())
    }
    pub fn request(&self) -> &PublishedInvocationRequest {
        &self.data.request
    }
    pub fn route(&self) -> &CrdtRoute {
        &self.data.route
    }
    pub fn contract(&self) -> &tilo_actions::ActionContract {
        &self.data.spec.contract
    }
    pub fn dispatch_spec(&self) -> &PublishedInvocationSpec {
        &self.data.spec
    }
    pub fn policy_time(&self) -> &crate::peer_actions::PolicyTime {
        &self.data.time
    }
    /// The original caller's device certificate.
    ///
    /// # Errors
    ///
    /// Returns a message when the original caller carried no certificate.
    pub fn certificate(&self) -> Result<&tilo_user_identity::DeviceCert, String> {
        match &self.data.caller {
            // A network caller's verified device certificate is its signed
            // evidence; room admission alone carries only the key identity.
            InvocationCaller::Certified { caller, .. } => {
                caller.admitted.certificate.as_ref().or_else(|| {
                    caller
                        .network
                        .as_ref()
                        .map(tilo_relay_session::client::CertifiedAction::certificate)
                })
            }
            InvocationCaller::Local { admitted, .. } => admitted.certificate.as_ref(),
            InvocationCaller::NativePerson { certificate, .. } => Some(certificate),
        }
        .ok_or_else(|| "original invocation certificate unavailable".into())
    }
    pub fn agent_id(&self) -> Option<&str> {
        match &self.data.caller {
            InvocationCaller::Certified { caller, .. } => caller.admitted.agent_id.as_deref(),
            InvocationCaller::Local { admitted, .. } => admitted.agent_id.as_deref(),
            InvocationCaller::NativePerson { .. } => None,
        }
    }
    /// Pure custody check: no owner callback, workspace snapshot or Store open.
    /// Its returned sample belongs to the original registered clock and origin.
    ///
    /// # Errors
    ///
    /// Returns a message when the broker, worker, certificate or clock lapsed.
    pub fn check_custody(&self) -> Result<u64, String> {
        if matches!(&self.worker, TargetCustody::Registered(worker) if !worker.current()) {
            return Err("original launcher publisher process or connection ended".into());
        }
        if self
            .data
            .execution
            .as_ref()
            .is_some_and(|lease| !lease.is_live())
        {
            return Err("original invocation execution session ended".into());
        }
        let now = self.data.time.now_ns().map_err(|error| error.to_string())?;
        let state = self
            .broker
            .upgrade()
            .ok_or("original invocation broker retired")?;
        if !state.is_accepting() {
            return Err("original invocation broker retired".into());
        }
        match &self.worker {
            TargetCustody::Registered(worker) => {
                let workers = state
                    .host_workers
                    .lock()
                    .map_err(|_| "worker registry unavailable")?;
                if workers
                    .get(&self.peer)
                    .and_then(|entry| entry.worker_registration.as_ref())
                    != Some(worker)
                {
                    return Err("original invocation worker connection retired or replaced".into());
                }
            }
            TargetCustody::Native { connection, .. } => {
                if !connection.current()
                    || connection.app_id != self.data.route.app_id
                    || connection.workspace_id != self.data.route.cluster_id
                {
                    return Err("original native target connection retired or replaced".into());
                }
                self.check_execution_frontier(&state)?;
            }
        }
        drop(state);
        tilo_user_identity::verify_device_cert_at(self.certificate()?, now)
            .map_err(|_| "original invocation certificate expired")?;
        self.data.time.now_ns().map_err(|error| error.to_string())
    }
    pub(super) fn check_execution_frontier(&self, state: &CapnpBrokerState) -> Result<(), String> {
        let pending = state
            .pending_invocations
            .lock()
            .map_err(|_| "invocation registry unavailable")?;
        let retained = pending
            .get(&self.data.request.wire_id)
            .ok_or("original invocation no longer retained")?;
        let invalid = retained.peer_id != self.peer
            || retained.update.is_terminal()
            || retained.cancellation_requested_at.is_some()
            || retained
                .original
                .as_ref()
                .is_none_or(|data| !Arc::ptr_eq(data, &self.data));
        drop(pending);
        if invalid {
            return Err("original invocation execution frontier retired or cancelled".into());
        }
        Ok(())
    }

    pub(super) fn for_approved_check(
        channel: &super::CapnpChannelHandle,
        request: &runtime_collab::approved_checks::Request,
    ) -> Result<Arc<Self>, String> {
        let route = channel.route();
        let admitted = channel
            .state
            .revalidate_admission(&route, &channel.verified_identity)
            .map_err(|error| error.to_string())?;
        let data = {
            let pending = channel
                .state
                .pending_invocations
                .lock()
                .map_err(|_| "invocation registry unavailable")?;
            let mut matches = pending.values().filter(|pending| {
                pending.peer_id == channel.peer_id
                    && pending.update.invocation().0.to_string() == request.invocation
            });
            let selected = matches
                .next()
                .ok_or("approved check has no retained target invocation")?;
            if matches.next().is_some() {
                return Err("ambiguous approved-check invocation identity".into());
            }
            let original = selected.original.clone();
            drop(pending);
            original.ok_or("original invocation authority unavailable")?
        };
        if data.route != route
            || data.request.address.action != runtime_collab::approved_checks::ACTION_ID
            || data.spec.kind != tilo_actions::OperationKind::Effect
            || data.spec.contract.execution
                != (tilo_actions::ExecutionMode::Background { cancellable: true })
            || data.spec.contract.permission != tilo_actions::PermissionRequirement::Write
        {
            return Err("approved check target action or contract mismatch".into());
        }
        let argument = data
            .request
            .argument
            .as_deref()
            .ok_or("approved check ID argument missing")?;
        if argument.len() > runtime_collab::approved_checks::MAX_REQUEST_BYTES {
            return Err("approved check arguments exceed bound".into());
        }
        let arguments: ApprovedCheckArguments = serde_json::from_str(
            argument
                .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                .unwrap_or(argument),
        )
        .map_err(|error| error.to_string())?;
        if arguments.check_id != request.check_id {
            return Err("approved check ID differs from accepted action".into());
        }
        let worker = if let Some(worker) = admitted.worker_registration.clone() {
            TargetCustody::Registered(Box::new(worker))
        } else {
            let connection = channel
                .lease
                .as_ref()
                .and_then(|lease| lease.local_media.as_ref())
                .filter(|connection| connection.current())
                .cloned()
                .ok_or("native target process custody unavailable")?;
            admitted
                .certificate
                .as_ref()
                .ok_or("native target certificate unavailable")?;
            TargetCustody::Native {
                connection,
                admitted: Box::new(admitted),
            }
        };
        let original = Arc::new(Self {
            broker: Arc::downgrade(&channel.state),
            peer: channel.peer_id,
            worker,
            data,
        });
        original.check_execution_frontier(&channel.state)?;
        original.check_lifecycle()?;
        Ok(original)
    }

    /// The original person caller, decided while `state` can still say who
    /// governs the room; `None` for an agent caller.
    fn original_person(
        &self,
        state: &CapnpBrokerState,
    ) -> Result<Option<super::BrokerActor>, String> {
        if self.agent_id().is_some() {
            return Ok(None);
        }
        let room = &self.data.route.cluster_id;
        let certificate = self.certificate()?;
        let mut person = state.certified_person(room, certificate);
        if let InvocationCaller::Certified {
            foreign_restriction,
            ..
        } = &self.data.caller
        {
            // Re-evaluate the governing exemption with the original exact device;
            // never let a weaker certificate erase the host's retained restriction.
            person.capped |= super::cap_applies_in(
                state.certificate_authority.get().map(AsRef::as_ref),
                room,
                *foreign_restriction,
                Some(&certificate.device_public_key),
            );
        }
        Ok(Some(person))
    }
    fn original_actor(
        &self,
        person: Option<super::BrokerActor>,
        workspace: &tilo_desktop_model::Workspace,
        seconds: u64,
    ) -> Result<Actor, String> {
        if let Some(person) = person {
            return Ok(person.policy_in(workspace, seconds));
        }
        let agent = self
            .agent_id()
            .ok_or("original invocation caller unavailable")?;
        workspace
            .role_of_agent_in_room_at(agent, &self.data.route.cluster_id, seconds)
            .ok_or("original agent membership expired")?;
        Ok(Actor::agent(agent).in_room(self.data.route.cluster_id.clone()))
    }
    pub(super) fn check_lifecycle(&self) -> Result<(), String> {
        self.check_custody()?;
        let state = self
            .broker
            .upgrade()
            .ok_or("original invocation broker retired")?;
        state.checked_policy_time(Some(&self.data.time))?;
        match &self.worker {
            TargetCustody::Registered(worker) => {
                let admitted = state
                    .host_workers
                    .lock()
                    .map_err(|_| "worker registry unavailable")?
                    .get(&self.peer)
                    .cloned()
                    .ok_or("original worker connection retired")?;
                if admitted.worker_registration.as_ref() != Some(worker) {
                    return Err("original worker connection replaced".into());
                }
                state
                    .revalidate_admission(&worker.route, &admitted)
                    .map_err(|error| error.to_string())?;
            }
            TargetCustody::Native { admitted, .. } => {
                state
                    .revalidate_admission(&self.data.route, admitted)
                    .map_err(|error| error.to_string())?;
            }
        }
        match &self.data.caller {
            InvocationCaller::NativePerson {
                room, certificate, ..
            } => {
                state.current_native_person(room, certificate)?;
            }
            InvocationCaller::Certified { caller, .. } => {
                state.current_action_caller(caller)?;
            }
            InvocationCaller::Local { route, admitted } => {
                state
                    .revalidate_admission(route, admitted)
                    .map_err(|error| error.to_string())?;
            }
        }
        let membership = state
            .membership
            .clone()
            .ok_or("workspace authority unavailable")?;
        let person = self.original_person(&state)?;
        drop(state);
        let seconds = self.data.time.now_ns().map_err(|error| error.to_string())? / 1_000_000_000;
        let verb = super::permission_verb(self.data.spec.contract.permission);
        membership.decide_in_room(&self.data.route.cluster_id, |workspace| {
            let workspace = workspace.ok_or("original invocation workspace unavailable")?;
            let actor = self.original_actor(person, workspace, seconds)?;
            if verb.is_some_and(|verb| {
                workspace.decide_at(
                    &actor,
                    &ResourceRef::Action {
                        app: self.data.route.app_id.clone(),
                        resource: self.data.request.address.resource.clone(),
                        action: self.data.request.address.action.clone(),
                    },
                    verb,
                    seconds,
                ) != CapDecision::Allow
            }) {
                return Err("original invocation caller is no longer authorized".to_string());
            }
            Ok(())
        })??;
        self.check_custody()?;

        Ok(())
    }
}

#[cfg(test)]
mod source_route_tests {
    use super::*;
    use crate::collab_broker_capnp::{BrokerChannels, FanoutState, PeerPublisher};
    use crate::peer_actions::{CertificateAuthority, OwnerPolicyClock, PolicyOrigin, PolicyTime};
    use crate::services::HostServices;
    use crate::test_support::{Workspace, WorkspaceId, WorkspaceTree};
    use collab_types::{CapOverride, Role, Scope, Subject, Verb};
    use ed25519_dalek::SigningKey;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    struct CurrentCertificateAuthority {
        origin: PolicyOrigin,
        accepts: AtomicBool,
    }

    impl CertificateAuthority for CurrentCertificateAuthority {
        fn policy_time(&self) -> Result<Option<PolicyTime>, String> {
            self.origin
                .capture()
                .map(Some)
                .map_err(|error| error.to_string())
        }

        fn authorize(
            &self,
            _room: uuid::Uuid,
            _certificate: &tilo_user_identity::DeviceCert,
        ) -> Result<(), String> {
            self.accepts
                .load(Ordering::Acquire)
                .then_some(())
                .ok_or_else(|| "fixture certificate revoked".into())
        }
    }

    struct Fixture {
        state: Arc<CapnpBrokerState>,
        membership: crate::test_support::WorkspaceTreeHandle,
        authority: Arc<CurrentCertificateAuthority>,
        source: CrdtRoute,
        target: CrdtRoute,
        admitted: AdmittedJoinIdentity,
        time: PolicyTime,
    }

    impl Fixture {
        fn new() -> Self {
            let user_signer = SigningKey::from_bytes(&[91; 32]);
            let device = SigningKey::from_bytes(&[92; 32]);
            let principal =
                super::super::principal_id_for_root(user_signer.verifying_key().to_bytes());
            let mut workspace = Workspace::new(WorkspaceId(91), "Cross-app original");
            workspace.set_owner(principal);
            let room = workspace.room.to_string();
            let source = CrdtRoute::record("so.tilo.chat", &room, "chat");
            let target = CrdtRoute::record("so.tilo.desktop", &room, "");
            let mut tree = WorkspaceTree::default();
            tree.workspaces.push(workspace);
            let mut admitted = super::super::admit_verified_join(
                &tree,
                &source,
                super::super::VerifiedJoinIdentity {
                    user_public_key: user_signer.verifying_key().to_bytes(),
                    device_public_key: device.verifying_key().to_bytes(),
                    capped: false,
                },
            )
            .expect("workspace owner is admitted");
            let mut certificate = tilo_user_identity::DeviceCert {
                user_public_key: user_signer.verifying_key().to_bytes(),
                device_public_key: device.verifying_key().to_bytes(),
                device_label: "cross-app fixture".into(),
                capabilities: tilo_user_identity::DeviceCapabilities::primary(),
                issued_at_ns: 1,
                expires_at_ns: None,
                signature: [0; 64],
            };
            tilo_user_identity::sign_device_cert(&mut certificate, &user_signer).unwrap();
            admitted.certificate = Some(certificate);
            let membership = tree.shared();
            let (clock, _control) = OwnerPolicyClock::for_test(2_000_000_000);
            let authority = Arc::new(CurrentCertificateAuthority {
                origin: clock.new_origin(),
                accepts: AtomicBool::new(true),
            });
            let fanout = Arc::new(FanoutState::new());
            let (state, _bridge) = CapnpBrokerState::new_configured(
                &fanout,
                Arc::new(Mutex::new(BrokerChannels::default())),
                PeerPublisher::disabled(),
                Some(membership.authority()),
                None,
                None,
                HostServices {
                    native_certificates: Some(authority.clone()),
                    ..Default::default()
                },
            );
            let time = authority.origin.capture().unwrap();
            Self {
                state,
                membership,
                authority,
                source,
                target,
                admitted,
                time,
            }
        }

        fn original(
            &self,
            source: CrdtRoute,
            admitted: AdmittedJoinIdentity,
        ) -> Arc<OriginalInvocationData> {
            let invocation = tilo_actions::InvocationId(uuid::Uuid::from_u128(91));
            OriginalInvocationData::capture(
                PublishedInvocationRequest {
                    wire_id: 91,
                    invocation,
                    address: tilo_actions::ActionAddress::new("desktop:tasks", "queue"),
                    argument: Some(format!("{}{{}}", tilo_actions::ARGUMENTS_JSON_PREFIX)),
                    actor: "app:so.tilo.chat".into(),
                    participant: None,
                },
                self.target.clone(),
                &PublishedAction {
                    semantic_identity: None,
                    resource: "desktop:tasks".into(),
                    action: "queue".into(),
                    resource_kind: "workspace-task".into(),
                    resource_label: "Tasks".into(),
                    action_label: "Queue".into(),
                    description: None,
                    operation_kind: tilo_actions::OperationKind::Document,
                    params: Vec::new(),
                    contract: tilo_actions::ActionContract {
                        permission: tilo_actions::PermissionRequirement::Write,
                        ..Default::default()
                    },
                    view: None,
                    input: PublishedInput::None,
                },
                None,
                None,
                Some(InvocationCaller::Local {
                    route: source,
                    admitted: Box::new(admitted),
                }),
                Some(self.time.clone()),
                None,
            )
            .unwrap()
        }

        fn envelope(original: &OriginalInvocationData) -> super::super::InvocationEnvelope {
            super::super::InvocationEnvelope {
                id: original.request.wire_id,
                invocation: original.request.invocation,
                resource: original.request.address.resource.clone(),
                action: original.request.address.action.clone(),
                argument: original.request.argument.clone(),
                actor: original.request.actor.clone(),
                context: None,
            }
        }

        fn admitted_agent(&self) -> (AdmittedJoinIdentity, String) {
            let capability =
                self.membership.write().workspaces[0].grant_capability(CapOverride::membership(
                    Subject::agent("tide"),
                    self.target.cluster_id.clone(),
                    Verb::Write,
                ));
            let mut agent = self.admitted.clone();
            super::super::admit_agent_delegation(
                &self.membership.read(),
                &self.source,
                &mut agent,
                "tide",
            )
            .unwrap();
            (agent, capability)
        }
    }

    #[test]
    fn current_person_custody_allows_a_revalidated_same_room_cross_app_target() {
        let fixture = Fixture::new();
        let original = fixture.original(fixture.source.clone(), fixture.admitted.clone());
        let envelope = Fixture::envelope(&original);
        assert_eq!(
            original
                .owner_person_certificate(&fixture.state, &fixture.target.cluster_id, &envelope)
                .unwrap()
                .device_public_key,
            fixture
                .admitted
                .certificate
                .as_ref()
                .unwrap()
                .device_public_key
        );
        fixture.authority.accepts.store(false, Ordering::Release);
        assert!(
            original
                .owner_person_certificate(&fixture.state, &fixture.target.cluster_id, &envelope)
                .is_err(),
            "revoked source admission cannot retain cross-app owner custody"
        );
        drop(fixture);
    }

    #[test]
    fn current_person_custody_refuses_wrong_room_agent_and_target_policy() {
        let fixture = Fixture::new();
        let wrong_room =
            CrdtRoute::record("so.tilo.chat", uuid::Uuid::new_v4().to_string(), "chat");
        let original = fixture.original(wrong_room, fixture.admitted.clone());
        assert!(
            original
                .owner_person_certificate(
                    &fixture.state,
                    &fixture.target.cluster_id,
                    &Fixture::envelope(&original),
                )
                .is_err()
        );
        let (agent, _capability) = fixture.admitted_agent();
        let original = fixture.original(fixture.source.clone(), agent);
        assert!(
            original
                .owner_person_certificate(
                    &fixture.state,
                    &fixture.target.cluster_id,
                    &Fixture::envelope(&original),
                )
                .is_err()
        );

        fixture.membership.write().workspaces[0].grant_capability(
            CapOverride::restrict_role_in_app(Role::Owner, "so.tilo.desktop", Verb::Read),
        );
        let original = fixture.original(fixture.source.clone(), fixture.admitted.clone());
        assert!(
            original
                .owner_person_certificate(
                    &fixture.state,
                    &fixture.target.cluster_id,
                    &Fixture::envelope(&original),
                )
                .is_err()
        );
        drop(fixture);
    }

    #[test]
    fn participant_inspection_revalidates_cross_app_people_and_agents() {
        let fixture = Fixture::new();
        let person = fixture.original(fixture.source.clone(), fixture.admitted.clone());
        let person_envelope = Fixture::envelope(&person);
        person
            .revalidate_participant_inspection(
                &fixture.state,
                &fixture.target.cluster_id,
                &person_envelope,
            )
            .unwrap();

        let (agent_identity, capability) = fixture.admitted_agent();
        let agent = fixture.original(fixture.source.clone(), agent_identity);
        let agent_envelope = Fixture::envelope(&agent);
        assert!(
            agent
                .revalidate_participant_inspection(
                    &fixture.state,
                    &fixture.target.cluster_id,
                    &agent_envelope,
                )
                .is_err(),
            "Agent membership alone does not grant the target action"
        );
        fixture.membership.write().workspaces[0].grant_capability(CapOverride::grant(
            Subject::agent("tide"),
            Scope::Action {
                app: "so.tilo.desktop".into(),
                resource: "desktop:tasks".into(),
                action: "queue".into(),
            },
            Verb::Write,
        ));
        agent
            .revalidate_participant_inspection(
                &fixture.state,
                &fixture.target.cluster_id,
                &agent_envelope,
            )
            .unwrap();
        fixture.membership.write().workspaces[0].revoke_capability(&capability);
        assert!(
            agent
                .revalidate_participant_inspection(
                    &fixture.state,
                    &fixture.target.cluster_id,
                    &agent_envelope,
                )
                .is_err(),
            "expired Agent membership cannot retain inspection custody"
        );

        let mut changed = person_envelope;
        changed.action = "another-action".into();
        assert!(
            person
                .revalidate_participant_inspection(
                    &fixture.state,
                    &fixture.target.cluster_id,
                    &changed,
                )
                .is_err(),
            "a changed envelope cannot borrow cross-app inspection custody"
        );
        fixture.authority.accepts.store(false, Ordering::Release);
        assert!(
            person
                .revalidate_participant_inspection(
                    &fixture.state,
                    &fixture.target.cluster_id,
                    &Fixture::envelope(&person),
                )
                .is_err(),
            "revoked source admission cannot retain inspection custody"
        );
        drop(fixture);
    }
}
impl CapnpBrokerState {
    /// Allocate the actual socket and its opaque host-local callback together.
    /// A second socket with an equal route/certificate gets a different token.
    ///
    /// # Errors
    ///
    /// Returns a message when the broker refuses the worker connection.
    pub fn connect_local_worker_with_authorizer(
        self: &Arc<Self>,
        route: CrdtRoute,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<
        (
            std::os::unix::net::UnixStream,
            PublishedInvocationAuthorizer,
        ),
        String,
    > {
        let registration = uuid::Uuid::new_v4();
        let stream =
            self.connect_local_worker_registered(route.clone(), certificate, Some(registration))?;
        let broker = Arc::downgrade(self);
        let worker = PrivateWorkerRegistration {
            route,
            certificate: certificate.clone(),
            identity: registration,
            launcher: None,
        };
        let authorize = Arc::new(move |request: &PublishedInvocationRequest| -> Result<PublishedInvocationLease, String> {
            let state = broker.upgrade().ok_or("original invocation broker retired")?;
            let (peer, data) = {
                let registry = state.pending_invocations.lock().map_err(|_| "invocation registry unavailable")?;
                let pending = registry.get(&request.wire_id).ok_or("original invocation is not retained")?;
                let data = pending.original.clone().ok_or("original invocation authority was not captured")?;
                if data.request != *request || data.route != worker.route || pending.update.invocation() != request.invocation {
                    return Err("delivered invocation differs from original request or worker route".into());
                }
                let peer = pending.peer_id;
                drop(registry);
                (peer, data)
            };
            let original = Arc::new(OriginalInvocation { broker: broker.clone(), peer, worker: TargetCustody::Registered(Box::new(worker.clone())), data });
            original.check_lifecycle()?;
            let route = ui::collaboration::RouteSpec {
                app_id: worker.route.app_id.clone(),
                cluster_id: worker.route.cluster_id.parse().map_err(|_| "invalid original worker room")?,
                record: (!worker.route.record_id.is_empty()).then(|| worker.route.record_id.clone()),
            };
            let check = original.clone();
            PublishedInvocationLease::new(request.clone(), route, Arc::new(move || check.check_lifecycle()))
                .with_dispatch_spec(original.dispatch_spec().clone())?
                .with_owner_token(original)
        });
        Ok((stream, authorize))
    }
}

#[cfg(test)]
mod tests {
    use super::message_agent_from_semantic_key;

    #[test]
    fn message_agent_requires_canonical_nonempty_person_key() {
        assert_eq!(
            message_agent_from_semantic_key("chat.person:scribe"),
            Some("scribe")
        );
        assert_eq!(message_agent_from_semantic_key("scribe"), None);
        assert_eq!(message_agent_from_semantic_key("chat.person:"), None);
    }
}
