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
    provider.parse::<u64>().is_ok()
        || tilo_actions::remote::application_provider(provider).is_some()
        || super::launchable::is_launchable_provider(provider)
}
