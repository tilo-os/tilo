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
    pub(super) policy_time: Option<crate::apps::policy_clock::PolicyTime>,
    pub(super) admitted: AdmittedJoinIdentity,
    /// The agent this person's own device handed the session to, kept
    /// across every re-verification of the person's admission.
    pub(super) delegated: Option<String>,
    /// A keyless native projection verified against protected device identity.
    /// Only `certified_native_action_provider` sets this; signed/network evidence
    /// alone never establishes physical intent.
    pub(super) native_projection_verified: bool,
}

impl VerifiedActionCaller {
    /// The same admission, handed by the person's own device to agent
    /// `agent`: it acts for the person with the delegation a device gives
    /// its agents.
    pub(super) fn delegated_to(mut self, agent: &str) -> Self {
        self.delegated = Some(delegated_agent_name(agent).to_owned());
        self.native_projection_verified = false;
        self.admitted = self.admitted.delegated_to(self.delegated.as_deref());
        self
    }

    /// Who durably owns what this caller invokes.
    ///
    /// A plain certified person is owned as [`InvocationOwner::Certified`].
    /// A session the person's own device handed to an agent (an MCP client,
    /// [`Self::delegated_to`]) is owned exactly as the local api owns the
    /// same admitted identity ([`durable_local_invocation::LocalCaller::owner_of`]):
    /// [`InvocationOwner::AuthenticatedLocal`] naming that agent, so the
    /// durable owner, the receipt actor and every later validation agree on
    /// who acted. Keeping only `Certified` here dropped the delegation, and
    /// the agent's own terminal update then looked like a stranger's.
    pub(super) fn owner(&self) -> Result<InvocationOwner, String> {
        self.admitted
            .delegated_invocation_owner(self.delegated.as_deref())
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

pub(super) fn revalidate_workspace_control(
    state: &AppBrokerState,
    route: &CrdtRoute,
    identity: &AdmittedJoinIdentity,
) -> Result<AdmittedJoinIdentity, BrokerError> {
    let current = state.revalidate_admission(route, identity)?;
    if !workspace_control_role_allowed(current.role) {
        return Err(BrokerError::failed(
            "workspace control requires current editor authority".into(),
        ));
    }
    Ok(current)
}

pub(super) fn workspace_control_role_allowed(role: collab_types::Role) -> bool {
    matches!(role, collab_types::Role::Owner | collab_types::Role::Editor)
}

impl AppBrokerState {
    /// The hosts of the workspaces this device takes part in but does not
    /// host, as the session gave them.
    pub(crate) fn hosted_workspaces(
        &self,
    ) -> Option<Arc<dyn crate::apps::services::HostedWorkspaces>> {
        self.services.hosted_workspaces.clone()
    }
    /// Attach the authority a harness admits its devices under, when it
    /// has one; a broker takes one authority, from wherever it comes.
    pub(super) fn attach_certificate_authority(
        &self,
        authority: Option<Arc<dyn crate::apps::certificate_authority::CertificateAuthority>>,
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
    ) -> Result<Option<crate::apps::policy_clock::PolicyTime>, String> {
        self.certificate_authority
            .get()
            .map_or(Ok(None), |authority| authority.policy_time())
    }
    pub(super) fn checked_policy_time(
        &self,
        submitted: Option<&crate::apps::policy_clock::PolicyTime>,
    ) -> Result<(Option<crate::apps::policy_clock::PolicyTime>, u64), String> {
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
            delegated: None,
        })
    }
    /// Whether `cert`'s device is held to the foreign-device cap in `workspace`.
    pub(super) fn capped_in(&self, workspace: &str, cert: &tilo_user_identity::DeviceCert) -> bool {
        cert_capped_in(
            self.certificate_authority.get().map(AsRef::as_ref),
            workspace,
            cert,
        )
    }
    /// The broker actor `caller` is in `workspace`.
    pub(super) fn caller_actor(
        &self,
        caller: &tilo_actions::remote::Caller,
        workspace: &str,
    ) -> BrokerActor {
        let actor = BrokerActor::for_caller(caller).capped(cap_applies_in(
            self.certificate_authority.get().map(AsRef::as_ref),
            workspace,
            caller.is_foreign_device(),
            caller.device_public_key(),
        ));
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
    /// The person `cert` certifies, acting in `workspace` from that certificate's device.
    pub(super) fn certified_person(
        &self,
        workspace: &str,
        cert: &tilo_user_identity::DeviceCert,
    ) -> BrokerActor {
        BrokerActor::person(person_id_for_root(cert.user_public_key))
            .capped(self.capped_in(workspace, cert))
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
        Ok(current.admitted.delegated_to(caller.delegated.as_deref()))
    }
    #[expect(
        private_interfaces,
        reason = "the journal key is the broker's own handle, reachable crate-wide only for tests"
    )]
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
            |agent| collab_types::MemberId::agent(agent.clone()),
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
            Some(cert) => VerifiedJoinIdentity::of(cert, self.capped_in(&route.cluster_id, cert)),
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
                } else {
                    current.display_handle.clone_from(&admitted.display_handle);
                }
                Ok::<_, String>(current)
            })
            .and_then(std::convert::identity)
            .map_err(BrokerError::failed)
    }
    /// Bind a certified operation to the route, typed document protocol and
    /// current workspace capability state. The identity service performs the
    /// same checks before signing; repeating them at receipt prevents a stale
    /// role or capability from remaining effective after launch.
    pub(super) fn authorize_certified_document(
        &self,
        route: &CrdtRoute,
        admitted: &AdmittedJoinIdentity,
        envelope: &SignedOp,
    ) -> Result<AuthorizedDocumentRoute, BrokerError> {
        let now = self.policy_now_ns().map_err(BrokerError::failed)? / 1_000_000_000;
        if route.app_id.is_empty() {
            return Err(BrokerError::failed(
                "certified document operation does not match its joined route".into(),
            ));
        }
        let payload = collab_types::validate_document_payload_route(
            &route.app_id,
            &route.cluster_id,
            &route.document_id,
            envelope,
        )
        .map_err(|error| {
            BrokerError::failed(format!(
                "certified document operation has an invalid routed payload: {error}"
            ))
        })?;
        // A workspace that seals its records takes every write encrypted under
        // the record channel's keys. An unencrypted one is refused here, on
        // the device that wrote it, and never reaches the workspace.
        if envelope.encryption.is_none()
            && self
                .services
                .committed_documents
                .as_ref()
                .is_some_and(|service| service.seals_documents(&route.cluster_id))
        {
            tracing::warn!(
                app = %route.app_id,
                workspace = %route.cluster_id,
                "an unencrypted write to a sealed workspace was refused"
            );
            return Err(BrokerError::failed(
                "this workspace seals every write: an unencrypted write is refused".into(),
            ));
        }
        let broker_channels = self.broker_channels.lock().map_err(|_| {
            BrokerError::failed("collaboration governance state is unavailable".into())
        })?;
        if !broker_channels
            .document_encryption_allowed(&route.cluster_id, envelope.encryption.as_ref())
        {
            return Err(BrokerError::failed(
                "plaintext document operation rejected after MLS migration".into(),
            ));
        }
        drop(broker_channels);
        // The change the op makes must be allowed outright: a signed op
        // carries no answer to a grant that would ask first.
        let record = collab_types::WrittenDocument {
            workspace: &route.cluster_id,
            app: &route.app_id,
            document: &route.document_id,
        };
        let allowed = admitted
            .with_current_authority(self.membership.as_ref(), &route.cluster_id, |access| {
                let cx = tilo_desktop_model::context_at(now);
                let allows = |invoker: Option<&tilo_action_policy::Acting>,
                              node: &tilo_action_policy::TreePath,
                              kind| {
                    collab_types::ChangeAuthor::answer(access, invoker, node, kind, &cx)
                        == Answer::Allow
                };
                Ok::<_, BrokerError>(
                    collab_types::decide_op(
                        envelope,
                        &record,
                        None,
                        None,
                        &collab_types::Surfaces::process(),
                        &allows,
                    )
                    .is_ok_and(|receipt| receipt == collab_types::Receipt::Admit),
                )
            })
            .map_err(BrokerError::failed)??;
        if !allowed {
            return Err(BrokerError::failed(
                "current workspace grants deny this document action".into(),
            ));
        }
        Ok(AuthorizedDocumentRoute {
            artifact: envelope.artifact,
            payload,
        })
    }
}
