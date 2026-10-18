//! A host-local door retaining an existing signed room admission, never a new grant.
use super::{Arc, CapnpBrokerState, VerifiedActionCaller, published_invocation::InvocationCaller};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CallerPrincipal, CataloguePage, CatalogueQuery,
    InvocationReceipt, InvocationState, InvokeRequest, Pending, PendingPoll,
};

#[derive(Clone)]
struct CertifiedLocalActionHost {
    state: Arc<CapnpBrokerState>,
    // Exact transport-owned session, including its shared execution lifetime.
    caller: Caller,
    effective: Caller,
    admission: VerifiedActionCaller,
    /// Only the protected native-seat entrypoint supplies physical intent.
    native_projection: Option<Caller>,
}

impl CapnpBrokerState {
    /// Retain a certified native-seat origin for an ephemeral private editor transfer.
    /// The payload never enters the action host, catalogue or history.
    /// # Errors
    /// Refuses stale/restricted Person admission, absent or ambiguous native Chat
    /// custody, invalid editor state or exhausted ephemeral capacity.
    pub fn stage_private_draft(
        self: &Arc<Self>,
        caller: &Caller,
        presentation: &[u8],
        draft: runtime_collab::private_draft::Draft,
    ) -> Result<uuid::Uuid, String> {
        self.stage_private_draft_bound(caller, presentation, draft, None)
    }

    /// Task editor transfers must retain the actual published paint admission.
    /// # Errors
    /// Refuses absent, forged, mismatched or revoked original paint provenance.
    pub fn stage_private_painted_draft(
        self: &Arc<Self>,
        caller: &Caller,
        presentation: &[u8],
        draft: runtime_collab::private_draft::Draft,
        app: String,
        interaction: tilo_actions::SemanticInteraction,
    ) -> Result<uuid::Uuid, String> {
        self.stage_private_draft_bound(caller, presentation, draft, Some((app, interaction)))
    }

    fn stage_private_draft_bound(
        self: &Arc<Self>,
        caller: &Caller,
        presentation: &[u8],
        draft: runtime_collab::private_draft::Draft,
        source: Option<(String, tilo_actions::SemanticInteraction)>,
    ) -> Result<uuid::Uuid, String> {
        let checked = self.checked_local_host(caller, presentation)?;
        let admitted = checked.check(caller).map_err(|error| error.to_string())?;
        let certificate = admitted
            .certificate
            .ok_or("private draft certificate unavailable")?;
        if checked.effective.is_foreign_device()
            || admitted.agent_id.is_some()
            || admitted.verified.capped
            || admitted.host_scope.is_some()
            || draft.workspace != checked.admission.room
            || certificate.capabilities.foreign
        {
            return Err("private drafts require the original certified person".into());
        }
        let bound_task = match (&draft.target, source) {
            (runtime_collab::private_draft::Target::Person { .. }, None) => None,
            (
                runtime_collab::private_draft::Target::Task {
                    agent,
                    task,
                    run_epoch,
                },
                Some((app, interaction)),
            ) => {
                if self.certified_painted_task(&draft.workspace, &app, agent, &interaction)
                    != Some((task.clone(), *run_epoch))
                {
                    return Err("private task draft requires its original painted admission".into());
                }
                Some((
                    draft.workspace.clone(),
                    app,
                    agent.clone(),
                    task.clone(),
                    *run_epoch,
                    interaction,
                ))
            }
            _ => return Err("private draft source does not match its target".into()),
        };
        // Weak state avoids a broker -> draft -> checked host -> broker cycle.
        let state = Arc::downgrade(self);
        let caller = checked.caller.clone();
        let admission = checked.admission.clone();
        drop(checked);
        let origin = Arc::new(move || {
            let state = state.upgrade().ok_or("private draft host closed")?;
            if !state.is_accepting() || !caller.execution_is_live() {
                return Err("private draft original person session ended".into());
            }
            let admitted = state.current_action_caller(&admission)?;
            if let Some((room, app, agent, task, epoch, interaction)) = &bound_task
                && state.certified_painted_task(room, app, agent, interaction)
                    != Some((task.clone(), *epoch))
            {
                return Err("private task draft source was revoked".into());
            }
            drop(state);
            if admitted.agent_id.is_some()
                || admitted.verified.capped
                || admitted.host_scope.is_some()
            {
                return Err("private draft person admission changed".into());
            }
            admitted
                .certificate
                .ok_or_else(|| "private draft certificate unavailable".into())
        });
        self.mint_private_draft(certificate, origin, draft)
    }

    /// Resolve a native seat's projected Person through protected host custody.
    /// `None` means this broker has no native seat (and grants no certificate
    /// custody); a configured service's refusal is never a compatibility fallback.
    ///
    /// # Errors
    /// Refuses retired executions, non-native callers and stale native custody.
    pub fn native_local_person_certificate(
        &self,
        caller: &Caller,
    ) -> Result<Option<tilo_user_identity::DeviceCert>, String> {
        let Some(service) = self.services.local_person.as_ref() else {
            return Ok(None);
        };
        if !self.is_accepting()
            || (caller.execution_id().is_some() && !caller.execution_is_live())
            || caller.is_foreign_device()
            || caller.acting_for_person().is_some()
            || caller.shortcut().is_some()
            || caller.user_public_key().is_some()
            || caller.device_public_key().is_some()
        {
            return Err("native person execution unavailable".into());
        }
        let CallerPrincipal::Person(person) = caller.principal() else {
            return Err("native person required".into());
        };
        let room = caller.room().ok_or("native person room required")?;
        let certificate = service.certificate(room, person)?;
        if super::principal_id_for_root(certificate.user_public_key) != *person {
            return Err("native person certificate mismatch".into());
        }
        self.current_native_person(room, &certificate)?;
        Ok(Some(certificate))
    }

    /// Invoke through the native seat's existing palette/control provider.
    /// This process-owned door is never used by network, MCP or app transports.
    pub fn action_invoke_local_person(
        self: &Arc<Self>,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if let Err(refused) = tilo_actions::appearance::refuse_here(request) {
            return Pending::ready(Err(refused));
        }
        if request.on_behalf_of.is_some()
            || caller.is_foreign_device()
            || !matches!(caller.principal(), CallerPrincipal::Person(_))
        {
            return Pending::ready(Err(ActionHostError::NotPermitted));
        }
        // Hosts without a native-seat service retain the pre-existing ordinary
        // action door. It supplies no original certificate custody, so no
        // privileged continuation can mistake it for native-person evidence.
        // An installed service that refuses NEVER takes this compatibility path.
        if self.services.local_person.is_none() {
            return self.action_invoke(caller, None, None, request);
        }
        let origin = (|| {
            if !self.is_accepting() || request.on_behalf_of.is_some() || caller.is_foreign_device()
            {
                return Err(ActionHostError::NotPermitted);
            }
            let CallerPrincipal::Person(person) = caller.principal() else {
                return Err(ActionHostError::NotPermitted);
            };
            let room = caller.room().ok_or(ActionHostError::NotPermitted)?;
            let certificate = self
                .services
                .local_person
                .as_ref()
                .ok_or(ActionHostError::NotPermitted)?
                .certificate(room, person)
                .map_err(|_| ActionHostError::NotPermitted)?;
            if caller
                .user_public_key()
                .is_some_and(|key| key != &certificate.user_public_key)
                || caller
                    .device_public_key()
                    .is_some_and(|key| key != &certificate.device_public_key)
            {
                return Err(ActionHostError::NotPermitted);
            }
            self.current_native_person(room, &certificate)
                .map_err(|_| ActionHostError::NotPermitted)?;
            if super::principal_id_for_root(certificate.user_public_key) != *person {
                return Err(ActionHostError::NotPermitted);
            }
            let time = self
                .registered_policy_time()
                .map_err(|_| ActionHostError::NotPermitted)?
                .ok_or(ActionHostError::NotPermitted)?;
            Ok(InvocationCaller::NativePerson {
                invocation: request.invocation,
                room: room.into(),
                certificate,
                time,
            })
        })();
        match origin {
            Ok(origin) => {
                match self.host_action_replay(caller, &origin, request) {
                    Ok(Some(receipt)) => return Pending::ready(Ok(receipt)),
                    Err(error) => return Pending::ready(Err(error)),
                    Ok(None) => {}
                }
                if let InvocationCaller::NativePerson {
                    room, certificate, ..
                } = &origin
                {
                    match self.checkpoint_replay(room, certificate, caller, request) {
                        Ok(Some(receipt)) => return Pending::ready(Ok(receipt)),
                        Err(reason) => {
                            return Pending::ready(Err(ActionHostError::Unavailable { reason }));
                        }
                        Ok(None) => {}
                    }
                }
                self.action_invoke_with_origin(caller, None, None, Some(&origin), request)
            }
            Err(error) => Pending::ready(Err(error)),
        }
    }

    pub(super) fn current_native_person(
        &self,
        room: &str,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        if !self.is_accepting() {
            return Err("native person host retired".into());
        }
        let time = self
            .registered_policy_time()?
            .ok_or("native person policy origin unavailable")?;
        tilo_user_identity::verify_device_cert_at(
            certificate,
            time.now_ns().map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let current = self
            .services
            .local_person
            .as_ref()
            .ok_or("native person authority unavailable")?
            .certificate(
                room,
                &super::principal_id_for_root(certificate.user_public_key),
            )?;
        if current != *certificate {
            return Err("native person identity replaced".into());
        }
        Ok(())
    }

    /// Bind one host-issued execution to an existing signed, room-bound presentation.
    /// The verifier, not the caller's labels or UUID, establishes authority.
    ///
    /// # Errors
    ///
    /// Refuses missing clocks, ended sessions, invalid/currently unadmitted evidence,
    /// and any caller principal, room or device mismatch. Does not enroll a device.
    pub fn certified_local_action_host(
        self: &Arc<Self>,
        caller: &Caller,
        presentation: &[u8],
    ) -> Result<Arc<dyn ActionHost + Send + Sync>, String> {
        Ok(self.checked_local_host(caller, presentation)?)
    }

    /// Preserve native physical intent without dropping the signed session.
    /// Network, app and delegated-agent doors never select this entrypoint.
    ///
    /// # Errors
    /// Refuses missing or replaced protected seat custody and invalid admission.
    pub fn certified_native_action_host(
        self: &Arc<Self>,
        caller: &Caller,
        presentation: &[u8],
    ) -> Result<Arc<dyn ActionHost + Send + Sync>, String> {
        let mut checked = (*self.checked_local_host(caller, presentation)?).clone();
        if checked.effective.is_foreign_device() {
            return Err("native action requires this device's person".into());
        }
        let CallerPrincipal::Person(person) = caller.principal() else {
            return Err("native action requires a person".into());
        };
        let mut projection = Caller::local_person(person).in_room(&checked.admission.room);
        if let Some(lease) = caller.execution_lease() {
            projection = projection.in_execution(lease);
        }
        let certificate = self
            .native_local_person_certificate(&projection)?
            .ok_or("native seat custody unavailable")?;
        if checked.admission.admitted.certificate.as_ref() != Some(&certificate) {
            return Err("native seat differs from original certified admission".into());
        }
        checked.admission.native_projection_verified = true;
        checked.effective = projection.clone();
        checked.native_projection = Some(projection);
        Ok(Arc::new(checked))
    }

    /// Retain the exact certificate owner for the native Jobs projection.
    /// Every read and control revalidates the original session admission.
    ///
    /// # Errors
    /// Refuses the same invalid or expired custody as `certified_local_action_host`.
    pub fn certified_action_lifecycle(
        self: &Arc<Self>,
        caller: &Caller,
        presentation: &[u8],
    ) -> Result<CertifiedActionLifecycle, String> {
        Ok(CertifiedActionLifecycle(
            self.checked_local_host(caller, presentation)?,
        ))
    }

    /// [`Self::certified_local_action_host`] handed to agent `agent` by the
    /// person's own device (an MCP client): the session stays the person's
    /// admission, every action through it is the agent's, acting for the
    /// person with the delegation a device gives its agents.
    ///
    /// # Errors
    ///
    /// As [`Self::certified_local_action_host`].
    pub fn certified_local_agent_host(
        self: &Arc<Self>,
        caller: &Caller,
        presentation: &[u8],
        agent: &str,
    ) -> Result<Arc<dyn ActionHost + Send + Sync>, String> {
        Ok(Arc::new(
            self.checked_local_host(caller, presentation)?
                .delegated_to(agent),
        ))
    }

    fn checked_local_host(
        self: &Arc<Self>,
        caller: &Caller,
        presentation: &[u8],
    ) -> Result<Arc<CertifiedLocalActionHost>, String> {
        // This door never falls back to the legacy wall clock during verification.
        self.registered_policy_time()?
            .ok_or("original policy clock unavailable")?;
        let room = caller.room().ok_or("an exact admitted room is required")?;
        let admission = self.verify_action_caller(room, presentation)?;
        if admission.policy_time.is_none() {
            return Err("original policy clock unavailable".into());
        }
        let certificate = admission
            .admitted
            .certificate
            .as_ref()
            .ok_or("original certificate unavailable")?;
        if caller.principal() != &CallerPrincipal::Person(admission.admitted.principal_id.clone())
            || caller.user_public_key() != Some(&certificate.user_public_key)
            || caller.device_public_key() != Some(&certificate.device_public_key)
        {
            return Err("caller does not match the certified room admission".into());
        }
        // Never remove a caller's existing foreign-device restriction, and never
        // let an omitted caller flag erase the signed certificate's restriction.
        let effective = caller
            .clone()
            .from_foreign_device(caller.is_foreign_device() || certificate.capabilities.foreign);
        let host = Arc::new(CertifiedLocalActionHost {
            state: self.clone(),
            caller: caller.clone(),
            effective,
            admission,
            native_projection: None,
        });
        // The presentation was verified against current membership and
        // policy above, with this clock sample; only the lease is new.
        host.check_lease(caller)
            .map_err(|error| error.to_string())?;
        Ok(host)
    }
}

/// Owner-aware lifecycle access under an existing certified session.
#[derive(Clone)]
pub struct CertifiedActionLifecycle(Arc<CertifiedLocalActionHost>);

impl CertifiedActionLifecycle {
    pub fn statuses(&self) -> Vec<(u64, tilo_actions::InvocationUpdate)> {
        let Ok(admitted) = self.0.check(&self.0.caller) else {
            return Vec::new();
        };
        self.0
            .state
            .invocation_statuses_for_owner(&self.0.admission.owner())
            .into_iter()
            .filter(|(id, update)| self.permitted_job(*id, update.invocation(), &admitted))
            .collect()
    }

    pub fn status(&self, id: u64) -> Option<tilo_actions::InvocationUpdate> {
        let admitted = self.0.check(&self.0.caller).ok()?;
        let update = self
            .0
            .state
            .invocation_status_for_owner(id, &self.0.admission.owner())?;
        self.permitted_job(id, update.invocation(), &admitted)
            .then_some(update)
    }

    pub fn can_cancel(&self, id: u64) -> bool {
        self.status(id).is_some()
            && self
                .0
                .state
                .can_cancel_for_owner(id, &self.0.admission.owner())
    }

    /// # Errors
    /// Refuses revoked custody, foreign ownership and non-cancellable jobs.
    pub fn cancel(&self, id: u64) -> Result<(), String> {
        self.status(id)
            .ok_or("the invocation is unavailable in this session")?;
        self.0
            .state
            .request_cancellation_for_owner(id, &self.0.admission.owner())
    }

    fn permitted_job(
        &self,
        id: u64,
        invocation: tilo_actions::InvocationId,
        admitted: &super::AdmittedJoinIdentity,
    ) -> bool {
        let Ok(guard) = self.0.state.journal.lock() else {
            return false;
        };
        let Ok(Some(journal)) = guard.as_ref() else {
            return false;
        };
        let key = super::InvocationKey {
            host: journal.host(),
            room: self.0.admission.room.clone(),
            owner: self.0.admission.owner(),
            invocation,
        };
        let in_room = journal
            .lookup(&key)
            .ok()
            .flatten()
            .is_some_and(|row| row.wire_id == id);
        drop(guard);
        in_room
            && self
                .0
                .state
                .authorize_checkpoint_job(
                    &self.0.admission.room,
                    admitted
                        .certificate
                        .as_ref()
                        .filter(|_| admitted.agent_id.is_none()),
                    id,
                )
                .is_ok()
    }
}

/// Existing signed personal-device admission, retained with its real execution
/// and original policy clock. No public constructor, serialized token, key,
/// device enrolment or account grant is exposed by this projection.
#[derive(Clone)]
pub struct CertifiedPersonContext {
    principal: String,
    device: String,
    checked: Arc<CertifiedLocalActionHost>,
}
impl CertifiedPersonContext {
    pub fn principal(&self) -> &str {
        &self.principal
    }
    pub fn device(&self) -> &str {
        &self.device
    }
    /// # Errors
    /// Refuses an ended/revoked session, unavailable original clock or changed
    /// certificate/admission. Must be called at the actual native I/O boundary.
    pub fn revalidate(&self) -> Result<(), String> {
        self.checked
            .check(&self.checked.caller)
            .map(|_| ())
            .map_err(|_| "personal device admission unavailable".into())
    }
}
impl CapnpBrokerState {
    /// Project only an already admitted person's own room on a non-foreign
    /// device. This does not delegate account credentials or mint permissions.
    /// # Errors
    /// Refuses forged labels, agents/apps, other rooms/devices, expired or
    /// revoked certificate/admission, ended execution and missing policy clocks.
    pub fn certified_person_context(
        self: &Arc<Self>,
        caller: &Caller,
        presentation: &[u8],
    ) -> Result<CertifiedPersonContext, String> {
        let checked = self.checked_local_host(caller, presentation)?;
        let certificate = checked
            .admission
            .admitted
            .certificate
            .as_ref()
            .ok_or("personal device certificate unavailable")?;
        let room = uuid::Uuid::parse_str(&checked.admission.room)
            .map_err(|_| "personal room unavailable")?;
        if !collab_types::is_person_room(&certificate.user_public_key, &room)
            || checked.effective.is_foreign_device()
            || checked.admission.admitted.agent_id.is_some()
            || checked.admission.admitted.host_scope.is_some()
        {
            return Err("native credentials require this person's own admitted device room".into());
        }
        let principal = checked.admission.admitted.principal_id.clone();
        let device = hex::encode(certificate.device_public_key);
        Ok(CertifiedPersonContext {
            principal,
            device,
            checked,
        })
    }
}

impl CertifiedLocalActionHost {
    fn invocation_origin(&self) -> InvocationCaller {
        InvocationCaller::Certified {
            caller: Box::new(self.admission.clone()),
            foreign_restriction: self.effective.is_foreign_device(),
        }
    }

    /// This door, handed to agent `agent` by the person's own device.
    fn delegated_to(&self, agent: &str) -> Self {
        Self {
            state: Arc::clone(&self.state),
            caller: self.caller.clone(),
            effective: self.effective.clone().delegated_to(agent),
            admission: self.admission.clone().delegated_to(agent),
            native_projection: None,
        }
    }

    /// The exact caller, its live execution, an accepting host and the
    /// original policy clock. No verification of the presentation.
    fn check_lease(&self, caller: &Caller) -> Result<(), ActionHostError> {
        if caller != &self.caller
            || caller.execution_id().is_none()
            || !caller.execution_is_live()
            || !self.state.is_accepting()
            || self.admission.policy_time.is_none()
        {
            return Err(ActionHostError::NotPermitted);
        }
        // Native custody was acquired at admission. Lifecycle reads must not
        // resolve the seat's identity files again; `check` below validates the
        // retained admission against current revocation and membership state.
        if self.native_projection.as_ref().is_some_and(|projection| {
            projection.execution_id().is_some() && !projection.execution_is_live()
        }) {
            return Err(ActionHostError::NotPermitted);
        }
        Ok(())
    }

    /// [`Self::check_lease`], then the presentation verified again against
    /// current certificate, fleet, membership and policy.
    fn check(&self, caller: &Caller) -> Result<super::AdmittedJoinIdentity, ActionHostError> {
        self.check_lease(caller)?;
        self.state
            .current_action_caller(&self.admission)
            .map_err(|_| ActionHostError::NotPermitted)
    }
}

impl ActionHost for CertifiedLocalActionHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        if let Err(error) = self.check(caller) {
            return Pending::ready(Err(error));
        }
        // Allow only doors that retain original custody through application
        // dispatch or the host-side admission barrier.
        if query.provider.as_ref().is_some_and(|provider| {
            !certified_provider(provider)
                || query
                    .target
                    .as_ref()
                    .is_some_and(|address| !certified_target(provider, address))
        }) {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        }
        let mut answer = self.state.action_query_with_origin(
            &self.effective,
            Some(&self.invocation_origin()),
            &query,
        );
        // An answer this call produced is covered by the check above; one
        // that arrives later is checked again when it does.
        if let PendingPoll::Ready(page) = answer.poll_ready() {
            return Pending::ready(page.map(certified_page));
        }
        let (complete, pending) = tilo_actions::remote::pending();
        let host = self.clone();
        answer.then(move |page| {
            let result = host
                .check(&host.caller)
                .and_then(|_| {
                    page.unwrap_or_else(|| {
                        Err(ActionHostError::Unavailable {
                            reason: "catalogue closed".into(),
                        })
                    })
                })
                .map(certified_page);
            complete.complete(result);
        });
        pending
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if let Err(refused) = tilo_actions::appearance::refuse_here(&request) {
            return Pending::ready(Err(refused));
        }
        // A new application admission is queued onto the broker. Validate its
        // lease here, but verify live certificate/policy there, not twice across
        // an asynchronous boundary. Replay/checkpoint replies still verify here
        // because they may return retained private data without another dispatch.
        let deferred = request.target.address.resource.as_str()
            != super::workspace_checkpoints::RESOURCE
            && self
                .state
                .remote_job(&self.effective, &request.invocation)
                .is_none();
        let admitted = if deferred {
            if let Err(error) = self.check_lease(caller) {
                return Pending::ready(Err(error));
            }
            self.admission.admitted.clone()
        } else {
            match self.check(caller) {
                Ok(admitted) => admitted,
                Err(error) => return Pending::ready(Err(error)),
            }
        };
        if !certified_target(&request.target.provider, &request.target.address) {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        }
        if request.target.address.resource.as_str() == super::workspace_checkpoints::RESOURCE
            && admitted.agent_id.is_some()
        {
            return Pending::ready(Err(ActionHostError::NotPermitted));
        }
        match self
            .state
            .host_action_replay(&self.effective, &self.invocation_origin(), &request)
        {
            Ok(Some(receipt)) => return Pending::ready(Ok(receipt)),
            Err(error) => return Pending::ready(Err(error)),
            Ok(None) => {}
        }
        let person_certificate = admitted
            .certificate
            .as_ref()
            .filter(|_| admitted.agent_id.is_none());
        if let Some(certificate) = person_certificate {
            match self.state.checkpoint_replay(
                &self.admission.room,
                certificate,
                &self.effective,
                &request,
            ) {
                Ok(Some(receipt)) => return Pending::ready(Ok(receipt)),
                Err(reason) => return Pending::ready(Err(ActionHostError::Unavailable { reason })),
                Ok(None) => {}
            }
        }
        if let Some(job) = self.state.remote_job(&self.effective, &request.invocation)
            && let Err(reason) =
                self.state
                    .authorize_checkpoint_job(&self.admission.room, person_certificate, job)
        {
            return Pending::ready(Err(ActionHostError::Unavailable { reason }));
        }
        // Only lend a live admission to synchronous work. A newly queued
        // dispatch verifies its original custody on the broker turn instead.
        let invoke = || {
            self.state.action_invoke_with_origin(
                &self.effective,
                None,
                None,
                Some(&self.invocation_origin()),
                &request,
            )
        };
        if deferred {
            invoke()
        } else {
            super::admission_memo::with_admitted_now(&self.admission, admitted, invoke)
        }
    }

    fn confirm(
        &self,
        caller: &Caller,
        request: &InvokeRequest,
    ) -> Pending<Result<tilo_actions::remote::Confirmation, ActionHostError>> {
        if let Err(error) = self.check(caller) {
            return Pending::ready(Err(error));
        }
        if !certified_target(&request.target.provider, &request.target.address) {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        }
        self.state.request_witness(&self.effective, None, request)
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        let admitted = self.check(caller).ok()?;
        let job = self.state.remote_job(&self.effective, id)?;
        self.state
            .authorize_checkpoint_job(
                &self.admission.room,
                admitted
                    .certificate
                    .as_ref()
                    .filter(|_| admitted.agent_id.is_none()),
                job,
            )
            .ok()?;
        self.state.action_observe(&self.effective, id)
    }

    fn next_change(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<Pending<()>> {
        self.check(caller).ok()?;
        self.state
            .action_next_change(&self.effective, id, self.state.invocation_changes())
    }

    fn cancel(
        &self,
        caller: &Caller,
        id: &tilo_actions::InvocationId,
    ) -> Result<(), ActionHostError> {
        self.check(caller)?;
        let job = self
            .state
            .remote_job(&self.effective, id)
            .ok_or(ActionHostError::UnknownInvocation)?;
        self.state
            .request_cancellation_for_owner(job, &self.admission.owner())
            .map_err(|reason| ActionHostError::NotCancellable { reason })
    }
}

/// `page` without rows or providers outside this door.
fn certified_page(mut page: CataloguePage) -> CataloguePage {
    page.rows.retain(|row| certified_provider(&row.provider));
    page.providers
        .retain(|provider| certified_provider(&provider.provider));
    page
}

// Registered workspace services now all retain original host admission. The
// live dispatcher, not this syntactic provider gate, resolves the descriptor.
fn certified_target(provider: &str, _address: &tilo_actions::ActionAddress) -> bool {
    certified_provider(provider)
}

// Only provider forms with preserved original dispatch/admission custody.
// Workspace tokens are browsable, but their rows and requests need target checks.
fn certified_provider(provider: &str) -> bool {
    // This is only door classification. The action host independently looks
    // up the opaque token and revalidates its live registration/custody.
    if provider.starts_with(super::canonical_route::PROVIDER_PREFIX) {
        return true;
    }
    matches!(
        provider,
        tilo_actions::remote::device_pairing::PROVIDER
            | tilo_actions::remote::identity_admin::PROVIDER
            | tilo_actions::remote::own_device::PROVIDER
    ) || provider.parse::<u64>().is_ok()
        || tilo_actions::remote::application_provider(provider).is_some()
        || super::launchable::is_launchable_provider(provider)
        || tilo_actions::remote::workspace_provider_room(provider).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::remote::{CatalogueProvider, CatalogueRow, workspace_provider};
    use tilo_actions::{ActionAddress, ActionDescriptor};

    fn workspace_targets() -> (Vec<ActionAddress>, Vec<ActionAddress>) {
        (
            vec![
                ActionAddress::new("app.so.tilo.notes", "open"),
                ActionAddress::new("app.so.tilo.notes", "close"),
                ActionAddress::new(
                    tilo_automation::shortcut_resource_id("test-shortcut"),
                    tilo_automation::SHORTCUT_RUN_ACTION,
                ),
                ActionAddress::new("workspace", "grant"),
                ActionAddress::new("workspace", "preset.assign"),
                ActionAddress::new(crate::automation::LIBRARY_RESOURCE, "create"),
                ActionAddress::new(crate::automation::LIBRARY_RESOURCE, "list"),
                ActionAddress::new("keyboard-shortcuts", "list"),
                ActionAddress::new("keyboard-shortcut:show-overview", "rebind"),
            ],
            vec![],
        )
    }

    #[test]
    fn workspace_requests_leave_descriptor_resolution_to_the_live_dispatcher() {
        let provider = workspace_provider("room");
        assert!(certified_provider(&provider));
        let (allowed, blocked) = workspace_targets();
        for address in allowed {
            // Both invocation and confirmation use this exact target gate.
            assert!(certified_target(&provider, &address), "{address:?}");
        }
        for address in blocked {
            assert!(!certified_target(&provider, &address), "{address:?}");
        }
        assert!(!certified_provider("untrusted"));
        assert!(!certified_target(
            "untrusted",
            &ActionAddress::new("app.so.tilo.notes", "open")
        ));
        assert!(certified_target("42", &ActionAddress::new("note", "edit")));
    }

    fn row(provider: &str, address: ActionAddress) -> CatalogueRow {
        CatalogueRow::for_action(
            provider,
            "test-app",
            ActionDescriptor {
                semantic_identity: None,
                address,
                resource_kind: "test-resource".into(),
                resource_label: "Test resource".into(),
                action_label: "Test action".into(),
                description: None,
                locus: tilo_actions::ExecutionLocus::default(),
                kind: tilo_actions::OperationKind::Effect,
                contract: tilo_actions::ActionContract::view(),
                input: tilo_actions::InputSpec::None,
                params: Vec::new(),
                view: None,
            },
        )
    }

    #[test]
    fn workspace_page_keeps_registered_service_actions_and_resource_rows() {
        let provider = workspace_provider("room");
        let (allowed, blocked) = workspace_targets();
        let mut rows: Vec<_> = allowed
            .iter()
            .chain(&blocked)
            .cloned()
            .map(|address| row(&provider, address))
            .collect();
        let mut resource_only = row(&provider, ActionAddress::new("shortcuts", "list"));
        resource_only.action = None;
        rows.push(resource_only);
        rows.push(row("42", ActionAddress::new("note", "edit")));
        rows.push(row("untrusted", ActionAddress::new("note", "edit")));
        let providers = [provider.as_str(), "42", "untrusted"]
            .into_iter()
            .map(|provider| CatalogueProvider {
                provider: provider.into(),
                app_id: "test-app".into(),
                room: "room".into(),
                record_id: String::new(),
                browsable: true,
            })
            .collect();
        let page = certified_page(CataloguePage {
            revision: "revision".into(),
            rows,
            providers,
            next_cursor: vec![1],
            ..CataloguePage::default()
        });
        assert_eq!(page.rows.len(), allowed.len() + 2);
        assert!(page.rows[allowed.len()].action.is_none());
        assert_eq!(
            page.rows[..allowed.len()]
                .iter()
                .map(|row| row.action.as_ref().unwrap().address.clone())
                .collect::<Vec<_>>(),
            allowed
        );
        assert_eq!(page.rows.last().unwrap().provider, "42");
        assert_eq!(
            page.providers
                .iter()
                .map(|p| p.provider.as_str())
                .collect::<Vec<_>>(),
            [provider.as_str(), "42"]
        );
        assert_eq!(page.revision, "revision");
        assert_eq!(page.next_cursor, [1]);
    }
}
