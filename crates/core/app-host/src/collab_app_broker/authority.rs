#[allow(
    clippy::wildcard_imports,
    reason = "one part of the broker module: the shared state, its imports and the other parts reach this through the parent glob"
)]
use super::*;

/// An opaque certified person admission. Its root/device are never accepted
/// from action payload strings; the existing certificate/proof verifier owns
/// construction and every operation rechecks current certificate/membership.
#[derive(Clone)]
pub(crate) struct VerifiedActionCaller {
    pub(super) workspace: String,
    pub(super) presentation: Vec<u8>,
    pub(super) policy_time: Option<crate::policy_clock::PolicyTime>,
    pub(super) admitted: AdmittedJoinIdentity,
    /// A keyless native projection verified against protected device identity.
    /// Only `certified_native_action_provider` sets this; signed/network evidence
    /// alone never establishes physical intent.
    pub(super) native_projection_verified: bool,
}

impl VerifiedActionCaller {
    /// Who durably owns what this caller invokes: the certified person, as
    /// [`InvocationOwner::Certified`].
    pub(super) fn owner(&self) -> Result<InvocationOwner, String> {
        self.admitted.certified_owner()
    }

    /// What a fresh verification of this caller proves: `current` (the
    /// presentation or signed evidence verified again, without a delegation)
    /// certifies the same person from the same device as the admission this
    /// caller carries.
    ///
    /// This cannot detect a changed session delegation: the delegation is
    /// never part of the presentation or the signed evidence, it is this
    /// caller's own, so it is not compared here. It proves the person and
    /// their device, and nothing else; the caller's delegation still decides
    /// what the invocation is owned by ([`Self::owner`]) and what it is
    /// permitted as.
    pub(super) fn reverified(&self, current: &Self) -> Result<(), String> {
        let (owner, current) = (self.owner()?, current.owner()?);
        match (same_root_and_device(&owner), same_root_and_device(&current)) {
            (Some(owner), Some(current)) if owner == current => Ok(()),
            _ => Err("certified caller changed".into()),
        }
    }
}

/// The agent a delegation names: an actor string (`agent:mcp-7`) names the
/// same agent as its bare name (`mcp-7`). Delegations are normalized here, at
/// the one point every one of them passes, so the durable owner and every
/// later comparison see one spelling.
pub(super) fn delegated_agent_name(agent: &str) -> &str {
    agent.strip_prefix("agent:").unwrap_or(agent)
}

/// The person and device an owner names, for an owner that retains an
/// authenticated one. An owner confined to an actor string names no device,
/// so it never matches.
pub(super) fn same_root_and_device(owner: &InvocationOwner) -> Option<([u8; 32], [u8; 32])> {
    match owner {
        InvocationOwner::Certified { root, device }
        | InvocationOwner::AuthenticatedLocal { root, device, .. } => Some((*root, *device)),
        InvocationOwner::Local { .. } => None,
    }
}

impl AppBrokerState {
    /// The hosts of the workspaces this device takes part in but does not
    /// host, as the session gave them.
    pub(crate) fn hosted_workspaces(&self) -> Option<Arc<dyn crate::services::HostedWorkspaces>> {
        self.services.hosted_workspaces.clone()
    }
    /// Attach the authority a harness admits its devices under, when it
    /// has one; a broker takes one authority, from wherever it comes.
    pub(super) fn attach_certificate_authority(
        &self,
        authority: Option<Arc<dyn crate::certificate_authority::CertificateAuthority>>,
    ) -> std::io::Result<()> {
        let Some(authority) = authority else {
            return Ok(());
        };
        self.certificate_authority
            .set(authority)
            .map_err(|_| std::io::Error::other("a broker takes one certificate authority"))
    }
    pub(super) fn registered_policy_time(
        &self,
    ) -> Result<Option<crate::policy_clock::PolicyTime>, String> {
        self.certificate_authority
            .get()
            .map_or(Ok(None), |authority| authority.policy_time())
    }
    pub(super) fn checked_policy_time(
        &self,
        submitted: Option<&crate::policy_clock::PolicyTime>,
    ) -> Result<(Option<crate::policy_clock::PolicyTime>, u64), String> {
        policy_context::sample(self.registered_policy_time()?, submitted)
    }
    pub(super) fn policy_now_ns(&self) -> Result<u64, String> {
        let registered = self.registered_policy_time()?;
        Ok(policy_context::sample(registered.clone(), registered.as_ref())?.1)
    }
    pub(crate) fn invocation_host(&self) -> Result<uuid::Uuid, String> {
        if self.durable_owner_required {
            self.owner_health()?;
        }
        match &mut *self
            .journal
            .lock()
            .map_err(|_| "invocation journal unavailable")?
        {
            // An invocation starts on a journal that recovered from any
            // failed barrier, never on one that waits for a restart.
            Ok(Some(journal)) => journal.ready().map(|()| journal.host()),
            Ok(None) => Err("durable admission unavailable".to_string()),
            Err(error) => Err(error.clone()),
        }
    }
    pub(crate) fn verify_action_caller(
        &self,
        workspace: &str,
        presentation: &[u8],
    ) -> Result<VerifiedActionCaller, String> {
        // The local certified presentation format also binds the exact workspace.
        // No caller-controlled `is_agent` or actor string enters this api.
        if uuid::Uuid::parse_str(workspace).is_err() || presentation.len() > 16 * 1024 {
            return Err("invalid certified invocation caller".into());
        }
        #[cfg(test)]
        admission_memo::count_verification();
        let admitted = verify_join_identity(
            &CrdtRoute::cluster(workspace),
            presentation,
            self.membership.as_ref(),
            self.trusted_devices.as_deref().map(Vec::as_slice),
            self.certificate_authority.get().map(Arc::as_ref),
        )?;
        Ok(VerifiedActionCaller {
            workspace: workspace.into(),
            presentation: presentation.into(),
            native_projection_verified: false,
            policy_time: self.registered_policy_time()?,
            admitted,
        })
    }
    /// The broker actor `caller` is, in any workspace.
    pub(crate) fn caller_actor(&self, caller: &tilo_actions::remote::Caller) -> BrokerActor {
        let actor = BrokerActor::for_caller(caller).capped(caller.is_foreign_device());
        match caller.acting().app() {
            Some(app) => match self.app_resource_kinds(app.as_str()) {
                Some(kinds) => actor.in_app(app.clone(), kinds),
                None => actor.through_app(app.clone()),
            },
            None => actor,
        }
    }
    /// The resource kinds `app` declared in its signed manifest, when this
    /// broker reads installed manifests.
    pub(super) fn app_resource_kinds(&self, app: &str) -> Option<Vec<String>> {
        self.app_resource_kinds
            .read()
            .ok()?
            .as_ref()
            .map(|resolve| resolve(app))
    }
    /// The person `cert` certifies, acting from that certificate's device.
    pub(super) fn certified_person(cert: &tilo_user_identity::DeviceCert) -> BrokerActor {
        BrokerActor::person(person_id_for_root(cert.user_public_key))
            .capped(cert.capabilities.foreign)
    }
    pub(crate) fn current_action_caller(
        &self,
        caller: &VerifiedActionCaller,
    ) -> Result<AdmittedJoinIdentity, String> {
        self.checked_policy_time(caller.policy_time.as_ref())?;
        if let Some(admitted) = admission_memo::admitted_now(caller) {
            return Ok(admitted);
        }
        self.live_action_caller(caller)
    }
    /// Effect barriers must not reuse the synchronous dispatch admission memo.
    pub(super) fn live_action_caller(
        &self,
        caller: &VerifiedActionCaller,
    ) -> Result<AdmittedJoinIdentity, String> {
        self.checked_policy_time(caller.policy_time.as_ref())?;
        let current = self.verify_action_caller(&caller.workspace, &caller.presentation)?;
        // The fresh verification re-establishes the person and device; the
        // session delegation this caller carries is the one the person's own
        // device handed over, here re-applied exactly as the dispatch does.
        // A caller whose proven person or device changed is refused, and a
        // delegated caller whose admission can no longer name its owner is
        // refused rather than downgraded, while its own recheck passes.
        caller.reverified(&current)?;
        caller.owner()?;
        Ok(current.admitted)
    }
    #[cfg(test)]
    pub(crate) fn caller_key(
        &self,
        caller: &VerifiedActionCaller,
        invocation: tilo_actions::InvocationId,
    ) -> Result<InvocationKey, String> {
        Ok(InvocationKey {
            workspace: caller.workspace.clone(),
            host: self.invocation_host()?,
            owner: caller.owner()?,
            invocation,
        })
    }
    /// Stable authenticated actor id for native presence and interactions.
    /// Display handles remain presentation only.
    pub(super) fn member_id(admitted: &AdmittedJoinIdentity) -> collab_types::MemberId {
        admitted.agent_id.as_ref().map_or_else(
            || collab_types::MemberId::person(admitted.actor_key.clone()),
            |agent| collab_types::MemberId::agent(agent.clone(), admitted.actor_key.clone()),
        )
    }
    pub(super) fn revalidate_admission(
        &self,
        route: &CrdtRoute,
        admitted: &AdmittedJoinIdentity,
    ) -> Result<AdmittedJoinIdentity, BrokerError> {
        let now = self.policy_now_ns().map_err(BrokerError::failed)?;
        if let Some(cert) = &admitted.certificate {
            tilo_user_identity::verify_device_cert_at(cert, now)
                .map_err(|_| BrokerError::failed("certificate no longer valid".into()))?;
            if let Some(authority) = self.certificate_authority.get() {
                let workspace = uuid::Uuid::parse_str(&route.cluster_id)
                    .map_err(|_| BrokerError::failed("invalid exact workspace".into()))?;
                authority
                    .authorize_join(workspace, cert)
                    .map_err(BrokerError::failed)?;
            }
        }
        let membership = self.membership.as_ref().ok_or_else(|| {
            BrokerError::failed("certified session lost its membership authority".into())
        })?;
        // The certificate was checked above; its connection ceiling must also
        // reflect current governance, not just governance at the original join.
        let verified = match &admitted.certificate {
            Some(cert) => VerifiedJoinIdentity::of(cert, cert.capabilities.foreign),
            None => admitted.verified.clone(),
        };
        let agents = self.services.agents.as_deref();
        // The person and any delegated agent are revalidated on one sample.
        membership
            .decide_in_workspace(&route.cluster_id, |workspace| {
                let mut current =
                    admit_verified_join_at(workspace, route, verified, now / 1_000_000_000)
                        .map_err(|error| {
                            format!(
                                "certified collaboration session is no longer admitted: {error}"
                            )
                        })?;
                current.certificate.clone_from(&admitted.certificate);
                current
                    .worker_registration
                    .clone_from(&admitted.worker_registration);
                if let Some(agent_id) = &admitted.agent_id {
                    let roster = agents.ok_or("this host reads no workspace's agents")?;
                    admit_listed_agent(roster, &route.cluster_id, &mut current, agent_id).map_err(
                        |error| format!("certified agent session is no longer admitted: {error}"),
                    )?;
                }
                Ok::<_, String>(current)
            })
            .and_then(std::convert::identity)
            .map_err(BrokerError::failed)
    }
}
