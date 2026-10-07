//! A host-local door retaining an existing signed room admission, never a new grant.
use super::{Arc, CapnpBrokerState, VerifiedActionCaller, published_invocation::InvocationCaller};
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CallerPrincipal, CataloguePage, CatalogueQuery,
    InvocationReceipt, InvocationState, InvokeRequest, Pending,
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
        host.check(caller).map_err(|error| error.to_string())?;
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
    fn check(&self, caller: &Caller) -> Result<(), ActionHostError> {
        if caller != &self.caller
            || caller.execution_id().is_none()
            || !caller.execution_is_live()
            || !self.state.is_accepting()
            || self.admission.policy_time.is_none()
        {
            return Err(ActionHostError::NotPermitted);
        }
        self.state
            .current_action_caller(&self.admission)
            .map_err(|_| ActionHostError::NotPermitted)?;
        Ok(())
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
        let answer = self.state.action_query(&self.effective, &query);
        let (complete, pending) = tilo_actions::remote::pending();
        let host = self.clone();
        answer.then(move |page| {
            let result = host
                .check(&host.caller)
                .and_then(|()| {
                    page.unwrap_or_else(|| {
                        Err(ActionHostError::Unavailable {
                            reason: "catalogue closed".into(),
                        })
                    })
                })
                .map(|mut page| {
                    page.rows.retain(|row| certified_provider(&row.provider));
                    page.providers
                        .retain(|provider| certified_provider(&provider.provider));
                    page
                });
            complete.complete(result);
        });
        pending
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        if let Err(error) = self.check(caller) {
            return Pending::ready(Err(error));
        }
        if !certified_provider(&request.target.provider) {
            return Pending::ready(Err(ActionHostError::UnknownAction));
        }
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
    }

    fn observe(&self, caller: &Caller, id: &tilo_actions::InvocationId) -> Option<InvocationState> {
        self.check(caller).ok()?;
        self.state.action_observe(&self.effective, id)
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
