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
        });
        // The presentation was verified against current membership and
        // policy above, with this clock sample; only the lease is new.
        host.check_lease(caller)
            .map_err(|error| error.to_string())?;
        Ok(host)
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
    /// This door, handed to agent `agent` by the person's own device.
    fn delegated_to(&self, agent: &str) -> Self {
        Self {
            state: Arc::clone(&self.state),
            caller: self.caller.clone(),
            effective: self.effective.clone().delegated_to(agent),
            admission: self.admission.clone().delegated_to(agent),
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
        // Numeric peer and application/retained-launch aliases preserve original
        // custody to the actual numeric dispatch. Other doors remain separate.
        if query
            .provider
            .as_ref()
            .is_some_and(|provider| !certified_provider(provider))
        {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        }
        let mut answer = self.state.action_query(&self.effective, &query);
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
        let admitted = match self.check(caller) {
            Ok(admitted) => admitted,
            Err(error) => return Pending::ready(Err(error)),
        };
        if !certified_provider(&request.target.provider) {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        }
        if request.target.address.resource.as_str() == super::workspace_checkpoints::RESOURCE
            && admitted.agent_id.is_some()
        {
            return Pending::ready(Err(ActionHostError::NotPermitted));
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
        // This admission stands for the dispatch this call reaches; whatever
        // continues after it returns verifies again.
        super::admission_memo::with_admitted_now(&self.admission, admitted, || {
            self.state.action_invoke_with_origin(
                &self.effective,
                None,
                None,
                Some(&InvocationCaller::Certified {
                    caller: Box::new(self.admission.clone()),
                    foreign_restriction: self.effective.is_foreign_device(),
                }),
                &request,
            )
        })
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
        self.state.action_cancel(&self.effective, id)
    }
}

/// `page` without rows or providers outside this door.
fn certified_page(mut page: CataloguePage) -> CataloguePage {
    page.rows.retain(|row| certified_provider(&row.provider));
    page.providers
        .retain(|provider| certified_provider(&provider.provider));
    page
}

// Only existing provider forms with preserved original SDK-delivery custody.
fn certified_provider(provider: &str) -> bool {
    // This is only door classification. The action host independently looks
    // up the opaque token and revalidates its live registration/custody.
    if provider.starts_with(super::canonical_route::PROVIDER_PREFIX) {
        return true;
    }
    provider.parse::<u64>().is_ok()
        || tilo_actions::remote::application_provider(provider).is_some()
        || super::launchable::is_launchable_provider(provider)
}
