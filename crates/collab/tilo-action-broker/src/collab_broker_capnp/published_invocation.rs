//! Original invocation custody, minted only by dispatch and an actual private worker.
use super::{
    AdmittedJoinIdentity, Arc, CapnpBrokerState, CrdtRoute, PrivateWorkerRegistration,
    PublishedAction, VerifiedActionCaller, Weak,
};
use tilo_action_policy::roles;
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
    /// The kind of the resource the action is published on: where it sits
    /// in the tree the grants decide over.
    resource_kind: String,
    caller: InvocationCaller,
    time: crate::peer_actions::PolicyTime,
    execution: Option<tilo_actions::remote::ExecutionLease>,
}
impl OriginalInvocationData {
    pub(super) fn permits_selected_open_v2(
        &self,
        request: &runtime_collab::open_selected::v2::Request,
    ) -> bool {
        let needed = if request.request.access.editable() {
            tilo_actions::ActionKind::Undoable
        } else {
            tilo_actions::ActionKind::Read
        };
        if !request.valid()
            || self.spec.contract.kind < needed
            || self.spec.kind != tilo_actions::OperationKind::Effect
            || self
                .execution
                .as_ref()
                .is_some_and(|lease| !lease.is_live())
            || self.request.address.resource != request.resource
        {
            return false;
        }
        selected_open_v2_action_matches(
            &self.request.address,
            self.request.argument.as_deref(),
            &self.spec.params,
            &request.resource,
            &request.request.app_id,
            request.request.access,
        )
    }

    pub(super) fn permits_selected_discovery(
        &self,
        scope: &runtime_collab::open_selected::discovery::InvocationScope,
    ) -> bool {
        let needed = if scope.access.editable() {
            tilo_actions::ActionKind::Undoable
        } else {
            tilo_actions::ActionKind::Read
        };
        scope.valid()
            && self.spec.kind == tilo_actions::OperationKind::Effect
            && self.spec.contract.kind >= needed
            && self
                .execution
                .as_ref()
                .is_none_or(tilo_actions::remote::ExecutionLease::is_live)
            && selected_open_v2_action_matches(
                &self.request.address,
                self.request.argument.as_deref(),
                &self.spec.params,
                &scope.resource,
                &scope.app_id,
                scope.access,
            )
    }

    pub(super) fn permits_selected_open(
        &self,
        request: &runtime_collab::open_selected::Request,
    ) -> bool {
        if self.spec.kind != tilo_actions::OperationKind::Effect
            || self.request.address.resource != request.resource
            || self.spec.contract.kind < tilo_actions::ActionKind::Read
            || self
                .execution
                .as_ref()
                .is_some_and(|lease| !lease.is_live())
        {
            return false;
        }
        selected_open_action_matches(
            &self.request.address,
            self.request.argument.as_deref(),
            request,
        )
    }

    pub(super) fn private_draft_caller(
        self: &Arc<Self>,
        state: &Arc<CapnpBrokerState>,
        peer: u64,
        worker: &PrivateWorkerRegistration,
        token: uuid::Uuid,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Argument {
            token: uuid::Uuid,
        }
        let argument = self
            .request
            .argument
            .as_deref()
            .ok_or("private draft token missing")?;
        let argument: Argument = serde_json::from_str(
            argument
                .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                .unwrap_or(argument),
        )
        .map_err(|_| "invalid private draft action arguments")?;
        if self.route != worker.route
            || self.request.address.action != "adopt-private-draft"
            || argument.token != token
        {
            return Err("original private draft action or token changed".into());
        }
        let restricted = match &self.caller {
            InvocationCaller::Certified {
                caller,
                foreign_restriction,
            } => {
                *foreign_restriction
                    || caller.admitted.verified.capped
                    || caller.admitted.host_scope.is_some()
            }
            InvocationCaller::Local { admitted, .. } => {
                admitted.verified.capped || admitted.host_scope.is_some()
            }
            InvocationCaller::NativePerson { .. } => false,
        };
        let original = OriginalInvocation {
            broker: Arc::downgrade(state),
            peer,
            worker: TargetCustody::Registered(Box::new(worker.clone())),
            data: self.clone(),
        };
        original.check_execution_frontier(state)?;
        original.check_lifecycle()?;
        let certificate = original.certificate()?;
        if restricted || original.agent_id().is_some() || certificate.capabilities.foreign {
            return Err("private draft adoption requires the original certified person".into());
        }
        let certificate = certificate.clone();
        drop(original);
        Ok(certificate)
    }

    pub(super) fn private_draft_token(&self) -> Option<uuid::Uuid> {
        if self.request.address.action != "adopt-private-draft" {
            return None;
        }
        let argument = self.request.argument.as_deref()?;
        let value: serde_json::Value = serde_json::from_str(
            argument
                .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                .unwrap_or(argument),
        )
        .ok()?;
        serde_json::from_value(value.get("token")?.clone()).ok()
    }

    pub(super) fn person_chat_caller(
        self: &Arc<Self>,
        state: &Arc<CapnpBrokerState>,
        peer: u64,
        worker: &PrivateWorkerRegistration,
        command: &crate::services::person_chat::PersonChatCommand,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        use crate::services::person_chat::PersonChatCommand;
        // Reading an open view is a narrower continuation of that exact
        // person's open invocation, still bound below to its original peer.
        let open_read = matches!(command, PersonChatCommand::Read { .. })
            && self.request.address.action == "open-direct-conversation";
        if self.route != worker.route
            || (self.request.address.action != command.action() && !open_read)
        {
            return Err(
                "original private app action does not match person conversation operation".into(),
            );
        }
        let bound_person = self.resource_kind == "chat.person"
            && self
                .request
                .address
                .resource
                .starts_with("tilo-ui-bound:v1:");
        let argument = if bound_person {
            // The single prompted text field is carried as plain text by the
            // action door, not as the legacy multi-parameter JSON object.
            serde_json::json!({"text": self.request.argument})
        } else {
            self.request
                .argument
                .as_deref()
                .map(serde_json::from_str)
                .transpose()
                .map_err(|_| "invalid original person conversation arguments")?
                .unwrap_or(serde_json::Value::Null)
        };
        let original_peer = self.person_conversation_peer(&argument);
        match command {
            PersonChatCommand::Contacts => {
                return Err("contacts are a private native snapshot, not an action".into());
            }
            PersonChatCommand::Open { public_key } | PersonChatCommand::Read { public_key } => {
                if original_peer.as_deref() != Some(public_key.as_str()) {
                    return Err("original person conversation peer changed".into());
                }
            }
            PersonChatCommand::Send {
                public_key,
                text,
                invocation,
            } => {
                if self.request.invocation.0 != *invocation {
                    return Err("original person conversation send invocation changed".into());
                }
                if original_peer.as_deref() != Some(public_key.as_str()) {
                    return Err("original person conversation send peer changed".into());
                }
                if argument.get("text").and_then(serde_json::Value::as_str) != Some(text.as_str()) {
                    return Err("original person conversation send text changed".into());
                }
            }
        }
        let original = OriginalInvocation {
            broker: Arc::downgrade(state),
            peer,
            worker: TargetCustody::Registered(Box::new(worker.clone())),
            data: self.clone(),
        };
        original.check_execution_frontier(state)?;
        original.check_lifecycle()?;
        let restricted = match &self.caller {
            InvocationCaller::Certified {
                caller,
                foreign_restriction,
            } => {
                *foreign_restriction
                    || caller.admitted.verified.capped
                    || caller.admitted.host_scope.is_some()
            }
            InvocationCaller::Local { admitted, .. } => {
                admitted.verified.capped || admitted.host_scope.is_some()
            }
            InvocationCaller::NativePerson { .. } => false,
        };
        let certificate = original.certificate()?;
        if restricted || original.agent_id().is_some() || certificate.capabilities.foreign {
            return Err("private conversations require the original certified person".into());
        }
        let certificate = certificate.clone();
        drop(original);
        Ok(certificate)
    }

    fn person_conversation_peer(&self, argument: &serde_json::Value) -> Option<String> {
        if let Some(key) = argument.get("public_key") {
            return key.as_str().map(str::to_owned);
        }
        // A framework live row binds the peer in its resource address, so the
        // caller supplies only the unbound fields (text for Send, none for
        // Open/Read). This is the retained, broker-resolved address, never a
        // new address asserted by the worker. Lifecycle checks pin its owner.
        if self.resource_kind != "chat.person" {
            return None;
        }
        let resource = &self.request.address.resource;
        let key = if let Some(bound) = resource.strip_prefix("tilo-ui-bound:v1:") {
            let mut parts = bound.splitn(3, ':');
            uuid::Uuid::parse_str(parts.next()?).ok()?;
            uuid::Uuid::parse_str(parts.next()?).ok()?;
            String::from_utf8(hex::decode(parts.next()?).ok()?).ok()?
        } else {
            resource.clone()
        };
        let peer = key.strip_prefix("chat.person:")?;
        (peer.len() == 64 && peer.bytes().all(|byte| byte.is_ascii_hexdigit()))
            .then(|| peer.to_owned())
    }

    pub(super) fn display_caller(
        self: &Arc<Self>,
        state: &Arc<CapnpBrokerState>,
        peer: u64,
        worker: &PrivateWorkerRegistration,
        mutates: bool,
    ) -> Result<tilo_user_identity::DeviceCert, String> {
        if self.route != worker.route
            || self.spec.contract.kind
                < if mutates {
                    tilo_actions::ActionKind::Outside
                } else {
                    tilo_actions::ActionKind::Read
                }
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
    pub(super) fn clipboard_resource_kind(&self) -> &str {
        &self.resource_kind
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
    pub(super) fn permits_clipboard(&self, needed: tilo_actions::ActionKind) -> bool {
        self.spec.contract.kind >= needed
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
                if route != &self.route {
                    return Err("original local caller route changed".into());
                }
                if admitted.agent_id.is_some() {
                    return Err("this action requires an authenticated person".into());
                }
                admitted
                    .certificate
                    .clone()
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
        state
            .membership
            .as_ref()
            .ok_or("workspace authority unavailable")?
            .decide_in_room(&self.route.cluster_id, |workspace| {
                let workspace = workspace.ok_or("original invocation workspace unavailable")?;
                if !person.may_in(
                    workspace,
                    &self.node(workspace),
                    self.spec.contract.kind,
                    seconds,
                ) {
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

    /// Where the original action sits in `workspace`'s tree.
    fn node(&self, workspace: &tilo_desktop_model::Workspace) -> tilo_action_policy::TreePath {
        tilo_actions::action_node(
            &workspace.node(),
            &self.route.app_id,
            &self.resource_kind,
            &self.request.address.resource,
            self.spec.contract.field.as_deref(),
            &self.request.address.action,
        )
    }

    pub(super) fn permits_launch_in(&self, room: &str) -> bool {
        self.route.cluster_id == room
            && self.spec.contract.kind >= tilo_actions::ActionKind::Undoable
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
                if !actor.may_in(
                    workspace,
                    &roles::governance_action(&workspace.policy.room, roles::governance::GRANT),
                    roles::GOVERNANCE_KIND,
                    seconds,
                ) {
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
        caller: Option<InvocationCaller>,
        time: Option<crate::peer_actions::PolicyTime>,
        execution: Option<tilo_actions::remote::ExecutionLease>,
    ) -> Option<Arc<Self>> {
        let spec = PublishedInvocationSpec {
            kind: offered.operation_kind,
            contract: offered.contract.clone(),
            params: offered.params.clone(),
            input: (&offered.input).into(),
            directory_revision: revision.map(str::to_owned),
        };
        Some(Arc::new(Self {
            request,
            route,
            spec,
            resource_identity: offered.semantic_identity.clone(),
            resource_kind: offered.resource_kind.clone(),
            caller: caller?,
            time: time?,
            execution,
        }))
    }
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
            || data.spec.contract.kind != tilo_actions::ActionKind::Undoable
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
    ) -> Result<super::BrokerActor, String> {
        if let Some(person) = person {
            return Ok(person);
        }
        let agent = self
            .agent_id()
            .ok_or("original invocation caller unavailable")?;
        let actor = super::BrokerActor::agent(format!("agent:{agent}"));
        // An agent a person delegated acts for them, never beyond them.
        let actor = match self.certificate() {
            Ok(certificate) => {
                actor.for_person(super::principal_id_for_root(certificate.user_public_key))
            }
            Err(_) => actor,
        };
        if !actor.reads_in(workspace, seconds) {
            return Err("original agent membership expired".into());
        }
        Ok(actor)
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
        membership.decide_in_room(&self.data.route.cluster_id, |workspace| {
            let workspace = workspace.ok_or("original invocation workspace unavailable")?;
            let actor = self.original_actor(person, workspace, seconds)?;
            if !actor.may_in(
                workspace,
                &self.data.node(workspace),
                self.data.spec.contract.kind,
                seconds,
            ) {
                return Err("original invocation caller is no longer authorized".to_string());
            }
            Ok(())
        })??;
        self.check_custody()?;

        Ok(())
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

fn selected_open_v2_action_matches(
    address: &tilo_actions::ActionAddress,
    argument: Option<&str>,
    params: &[tilo_actions::ActionParam],
    resource: &str,
    app_id: &str,
    access: runtime_collab::open_selected::v2::SelectedAccess,
) -> bool {
    if address.resource != resource {
        return false;
    }
    let mut access_params = params.iter().filter(|param| param.name == "access");
    let Some(param) = access_params.next() else {
        return false;
    };
    if access_params.next().is_some() || param.required || param.ty != tilo_actions::ParamType::Text
    {
        return false;
    }
    let Some(schema) = param.schema.as_ref() else {
        return false;
    };
    let Some(default) = schema.get("default").and_then(serde_json::Value::as_str) else {
        return false;
    };
    let arguments = match argument {
        Some(raw) => match serde_json::from_str::<serde_json::Value>(
            raw.strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
                .unwrap_or(raw),
        ) {
            Ok(value) if value.is_object() => Some(value),
            _ => return false,
        },
        None => None,
    };
    let actual = arguments
        .as_ref()
        .and_then(|value| value.get("access"))
        .map_or(Some(default), serde_json::Value::as_str);
    if actual != Some(access.as_str())
        || !param.choices.iter().any(|choice| choice == access.as_str())
    {
        return false;
    }
    if let Some(app) = address.action.strip_prefix("open-with-app:") {
        return app == app_id
            && arguments.as_ref().is_none_or(|value| {
                value
                    .as_object()
                    .is_some_and(|values| values.keys().all(|key| key == "access"))
            })
            && schema.get("const").and_then(serde_json::Value::as_str) == Some(default)
            && default == access.as_str()
            && param.choices.len() == 1;
    }
    if address.action != "open-with" {
        return false;
    }
    arguments.is_some_and(|value| {
        value.get("app").and_then(serde_json::Value::as_str) == Some(app_id)
            && value
                .get("always")
                .is_none_or(|always| always == &serde_json::Value::Bool(false))
    })
}
fn selected_open_action_matches(
    address: &tilo_actions::ActionAddress,
    argument: Option<&str>,
    request: &runtime_collab::open_selected::Request,
) -> bool {
    if address.resource != request.resource {
        return false;
    }
    let action = &address.action;
    if let Some(app) = action.strip_prefix("open-with-app:") {
        return app == request.request.app_id;
    }
    // Changing a default is another effect, deliberately absent here.
    if action != "open-with" {
        return false;
    }
    argument
        .and_then(|argument| serde_json::from_str::<serde_json::Value>(argument).ok())
        .is_some_and(|value| {
            value.get("app").and_then(serde_json::Value::as_str)
                == Some(request.request.app_id.as_str())
                && value
                    .get("always")
                    .is_none_or(|always| always == &serde_json::Value::Bool(false))
        })
}
#[cfg(test)]
mod selected_open_tests {
    use super::selected_open_action_matches;
    use runtime_collab::open_selected::{PROTOCOL, Request, RootRequest};
    use tilo_actions::ActionAddress;
    #[test]
    fn selected_open_requires_the_exact_canonical_target_and_resource() {
        let request = Request {
            protocol: PROTOCOL.into(),
            resource: "entry:chosen".into(),
            request: RootRequest {
                schema: 1,
                request_id: "11111111-1111-4111-8111-111111111111".into(),
                components: vec![b"selected.txt".to_vec()],
                app_id: "so.tilo.text-editor".into(),
                read_only: true,
            },
            selected_descriptor: 3,
            parent_descriptor: 4,
            proof_descriptor: 5,
        };
        let address = ActionAddress::new("entry:chosen", "open-with");
        assert!(selected_open_action_matches(
            &address,
            Some(r#"{"app":"so.tilo.text-editor","always":false}"#),
            &request
        ));
        assert!(!selected_open_action_matches(
            &address,
            Some(r#"{"app":"so.tilo.terminal"}"#),
            &request
        ));
        assert!(!selected_open_action_matches(
            &address,
            Some(r#"{"app":"so.tilo.text-editor","always":true}"#),
            &request
        ));
        assert!(!selected_open_action_matches(&address, None, &request));
        assert!(!selected_open_action_matches(
            &ActionAddress::new("entry:other", "open-with-app:so.tilo.text-editor"),
            None,
            &request
        ));
        assert!(!selected_open_action_matches(
            &ActionAddress::new("entry:chosen", "always-open-with:so.tilo.text-editor"),
            None,
            &request
        ));
        assert!(!selected_open_action_matches(
            &ActionAddress::new("entry:chosen", "copy-selection"),
            None,
            &request
        ));
        assert!(selected_open_action_matches(
            &ActionAddress::new("entry:chosen", "open-with-app:so.tilo.text-editor"),
            None,
            &request
        ));
    }
}

#[cfg(test)]
mod selected_open_v2_tests {
    use super::selected_open_v2_action_matches;
    use runtime_collab::open_selected::v2::{PROTOCOL, Request, RootRequest, SelectedAccess};
    use tilo_actions::{ActionAddress, ActionParam, ParamType};
    #[test]
    fn original_bound_access_cannot_be_replaced_by_request_or_legacy_shorthand() {
        let request = Request {
            protocol: PROTOCOL.into(),
            resource: "entry:report".into(),
            request: RootRequest {
                schema: 2,
                request_id: "11111111-1111-4111-8111-111111111111".into(),
                components: vec![b"report.txt".to_vec()],
                app_id: "so.tilo.text-editor".into(),
                access: SelectedAccess::EditInParent,
            },
            selected_descriptor: 3,
            parent_descriptor: 4,
            proof_descriptor: 5,
        };
        let param=ActionParam::optional("access",ParamType::Text,"Selected source access").with_choices(["edit-in-parent"]).with_schema(serde_json::json!({"type":"string","const":"edit-in-parent","default":"edit-in-parent"}));
        let address = ActionAddress::new("entry:report", "open-with-app:so.tilo.text-editor");
        assert!(selected_open_v2_action_matches(
            &address,
            None,
            std::slice::from_ref(&param),
            &request.resource,
            &request.request.app_id,
            request.request.access
        ));
        assert!(!selected_open_v2_action_matches(
            &address,
            None,
            &[],
            &request.resource,
            &request.request.app_id,
            request.request.access
        ));
        let readonly = ActionParam::optional("access", ParamType::Text, "Selected source access")
            .with_choices(["read-only"])
            .with_schema(
                serde_json::json!({"type":"string","const":"read-only","default":"read-only"}),
            );
        assert!(!selected_open_v2_action_matches(
            &address,
            None,
            &[readonly],
            &request.resource,
            &request.request.app_id,
            request.request.access
        ));
        assert!(!selected_open_v2_action_matches(
            &address,
            Some(r#"{"access":"read-only"}"#),
            std::slice::from_ref(&param),
            &request.resource,
            &request.request.app_id,
            request.request.access
        ));
        let different = ActionAddress::new("entry:report", "open-with-app:so.tilo.terminal");
        assert!(!selected_open_v2_action_matches(
            &different,
            None,
            std::slice::from_ref(&param),
            &request.resource,
            &request.request.app_id,
            request.request.access
        ));
        let generic = ActionAddress::new("entry:report", "open-with");
        assert!(selected_open_v2_action_matches(
            &generic,
            Some(r#"{"app":"so.tilo.text-editor","always":false}"#),
            std::slice::from_ref(&param),
            &request.resource,
            &request.request.app_id,
            request.request.access
        ));
        assert!(!selected_open_v2_action_matches(
            &generic,
            Some(r#"{"app":"so.tilo.text-editor","always":true}"#),
            &[param],
            &request.resource,
            &request.request.app_id,
            request.request.access
        ));
    }
}
