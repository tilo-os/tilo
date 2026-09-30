//! Original invocation custody, minted only by dispatch and an actual private worker.
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
    Certified(Box<VerifiedActionCaller>),
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
    caller: InvocationCaller,
    time: crate::peer_actions::PolicyTime,
}
impl OriginalInvocationData {
    pub(super) fn capture(
        request: PublishedInvocationRequest,
        route: CrdtRoute,
        offered: &PublishedAction,
        revision: Option<&str>,
        caller: Option<InvocationCaller>,
        time: Option<crate::peer_actions::PolicyTime>,
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
        };
        Some(Arc::new(Self {
            request,
            route,
            spec,
            caller: caller?,
            time: time?,
        }))
    }
}

/// Immutable original facts; there is no public constructor or deserializer.
/// This token is not an allow decision. The final owner must recheck its current
/// certificate, membership and policy under its held guard.
pub struct OriginalInvocation {
    broker: Weak<CapnpBrokerState>,
    peer: u64,
    worker: PrivateWorkerRegistration,
    data: Arc<OriginalInvocationData>,
}
impl OriginalInvocation {
    pub fn request(&self) -> &PublishedInvocationRequest {
        &self.data.request
    }
    pub fn worker_certificate(&self) -> &tilo_user_identity::DeviceCert {
        &self.worker.certificate
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
            InvocationCaller::Certified(caller) => {
                caller.admitted.certificate.as_ref().or_else(|| {
                    caller
                        .network
                        .as_ref()
                        .map(tilo_relay::client::CertifiedAction::certificate)
                })
            }
            InvocationCaller::Local { admitted, .. } => admitted.certificate.as_ref(),
        }
        .ok_or_else(|| "original invocation certificate unavailable".into())
    }
    pub fn agent_id(&self) -> Option<&str> {
        match &self.data.caller {
            InvocationCaller::Certified(caller) => caller.admitted.agent_id.as_deref(),
            InvocationCaller::Local { admitted, .. } => admitted.agent_id.as_deref(),
        }
    }
    /// Pure custody check: no owner callback, workspace snapshot or Store open.
    /// Its returned sample belongs to the original registered clock and origin.
    ///
    /// # Errors
    ///
    /// Returns a message when the broker, worker, certificate or clock lapsed.
    pub fn check_custody(&self) -> Result<u64, String> {
        let now = self.data.time.now_ns().map_err(|error| error.to_string())?;
        let state = self
            .broker
            .upgrade()
            .ok_or("original invocation broker retired")?;
        if !state.is_accepting() {
            return Err("original invocation broker retired".into());
        }
        let workers = state
            .host_workers
            .lock()
            .map_err(|_| "worker registry unavailable")?;
        let same_worker = workers
            .get(&self.peer)
            .and_then(|entry| entry.worker_registration.as_ref())
            == Some(&self.worker);
        drop(workers);
        drop(state);
        if !same_worker {
            return Err("original invocation worker connection retired or replaced".into());
        }
        tilo_user_identity::verify_device_cert_at(self.certificate()?, now)
            .map_err(|_| "original invocation certificate expired")?;
        self.data.time.now_ns().map_err(|error| error.to_string())
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
        Ok(Some(state.certified_person(room, self.certificate()?)))
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
            .role_of_agent_at(agent, seconds)
            .ok_or("original agent membership expired")?;
        Ok(Actor::agent(agent).in_room(self.data.route.cluster_id.clone()))
    }
    fn check_lifecycle(&self) -> Result<(), String> {
        self.check_custody()?;
        let state = self
            .broker
            .upgrade()
            .ok_or("original invocation broker retired")?;
        state.checked_policy_time(Some(&self.data.time))?;
        let admitted = state
            .host_workers
            .lock()
            .map_err(|_| "worker registry unavailable")?
            .get(&self.peer)
            .cloned()
            .ok_or("original worker connection retired")?;
        if admitted.worker_registration.as_ref() != Some(&self.worker) {
            return Err("original worker connection replaced".into());
        }
        // Current certificate policy may open Store; the registry lock is gone.
        state
            .revalidate_admission(&self.worker.route, &admitted)
            .map_err(|error| error.to_string())?;
        match &self.data.caller {
            InvocationCaller::Certified(caller) => {
                state.current_action_caller(caller)?;
            }
            InvocationCaller::Local { route, admitted } => {
                state
                    .revalidate_admission(route, admitted)
                    .map_err(|error| error.to_string())?;
            }
        }
        let tree = state
            .membership
            .as_ref()
            .ok_or("workspace authority unavailable")?
            .snapshot()?;
        let person = self.original_person(&state)?;
        drop(state);
        let workspace = tree
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == self.data.route.cluster_id)
            .ok_or("original invocation workspace unavailable")?;
        let seconds = self.data.time.now_ns().map_err(|error| error.to_string())? / 1_000_000_000;
        let actor = self.original_actor(person, workspace, seconds)?;
        let verb = match self.data.spec.contract.permission {
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
                    app: self.data.route.app_id.clone(),
                    action: self.data.request.address.action.clone(),
                },
                verb,
                seconds,
            ) != CapDecision::Allow
        }) {
            return Err("original invocation caller is no longer authorized".into());
        }
        drop(tree);
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
            let original = Arc::new(OriginalInvocation { broker: broker.clone(), peer, worker: worker.clone(), data });
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
