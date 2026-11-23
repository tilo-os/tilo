#[allow(
    clippy::wildcard_imports,
    reason = "one part of the broker module: the shared state, its imports and the other parts reach this through the parent glob"
)]
use super::*;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CollabLaunchHandoff {
    pub(super) protocol: u16,
    pub(super) app_id: String,
    pub(super) workspace_id: String,
    #[serde(default)]
    pub(super) executing_peer: bool,
    /// The window a connection the session makes for one of its own windows
    /// is for (`launcher::services::write_collab_window_handoff`).
    #[serde(default)]
    pub(super) window_instance: Option<u64>,
}

impl CollabLaunchHandoff {
    /// The launch `service` holds for the process `pid` this handoff came
    /// from, as the app, workspace and window it names.
    pub(super) fn authenticated_launch(
        &self,
        service: &dyn crate::apps::services::LauncherServices,
        pid: u32,
    ) -> Result<Option<crate::apps::services::AuthenticatedApplicationLaunch>, String> {
        service.authenticated_launch(pid, &self.app_id, &self.workspace_id, self.window_instance)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct VerifiedJoinIdentity {
    pub(super) user_public_key: [u8; 32],
    pub(super) device_public_key: [u8; 32],
    /// A foreign device in a workspace it does not govern: its role is capped.
    pub(super) capped: bool,
}

impl VerifiedJoinIdentity {
    pub(super) fn of(cert: &tilo_user_identity::DeviceCert, capped: bool) -> Self {
        Self {
            user_public_key: cert.user_public_key,
            device_public_key: cert.device_public_key,
            capped,
        }
    }

    /// Whether `cluster_id` is this identity's own workspace: the workspace
    /// every device its root certified owns, and no other device enters.
    pub(super) fn owns_person_workspace(&self, cluster_id: &str) -> bool {
        uuid::Uuid::parse_str(cluster_id).is_ok_and(|workspace| {
            collab_types::is_person_workspace(&self.user_public_key, &workspace)
        })
    }
}

/// Whether a device is held to the foreign-device cap in `workspace`, as far as
/// `authority` knows who governs it; `device` `None` is this host's own.
pub(super) fn cap_applies_in(
    authority: Option<&dyn crate::apps::certificate_authority::CertificateAuthority>,
    workspace: &str,
    foreign: bool,
    device: Option<&[u8; 32]>,
) -> bool {
    let governs = authority
        .zip(tilo_failure::ignored!(
            "a workspace named by anything but a UUID is governed by no authority",
            uuid::Uuid::parse_str(workspace).ok()
        ))
        .is_some_and(|(authority, workspace)| authority.governs(workspace, device));
    // A foreign device is held to the bound only in a workspace it does not govern.
    foreign && !governs
}

/// [`cap_applies_in`] for `cert`'s device.
pub(super) fn cert_capped_in(
    authority: Option<&dyn crate::apps::certificate_authority::CertificateAuthority>,
    workspace: &str,
    cert: &tilo_user_identity::DeviceCert,
) -> bool {
    cap_applies_in(
        authority,
        workspace,
        cert.capabilities.foreign,
        Some(&cert.device_public_key),
    )
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AdmittedJoinIdentity {
    pub(super) worker_registration: Option<PrivateWorkerRegistration>,
    pub(super) certificate: Option<tilo_user_identity::DeviceCert>,
    pub(super) verified: VerifiedJoinIdentity,
    pub(super) actor_key: String,
    /// Informational display label announced by this authenticated session.
    /// Authorization always uses `actor_key`, never this handle.
    pub(super) display_handle: Option<String>,
    /// Workspace admission role from the live owner/capability roster.
    /// Certified document operations use this directly instead of requiring
    /// a second, channel-local role attestation for the same person. For a
    /// delegated agent it is the agent's role
    /// ([`roles::agent_policy::role`]): its preset's, capped by its person's.
    pub(super) role: CapRole,
    /// Delegated agent actor for this channel. The certified device still
    /// proves which person launched it, while policy uses this revocable id.
    pub(super) agent_id: Option<String>,
    /// The agent is one this person's own device handed the session to (an
    /// MCP client): it carries the delegation a device gives its agents,
    /// not grants the workspace holds for it.
    pub(super) session_delegated: bool,
    /// The agent certificate an agent joined under, verified: a device's
    /// chain to its person, for one session in this workspace. Every
    /// revalidation verifies it again and asks whether its session still goes.
    pub(super) agent_cert: Option<Box<AgentCert>>,
}

impl AdmittedJoinIdentity {
    /// This admission handed to `agent` by the person's own device, if any.
    pub(super) fn delegated_to(mut self, agent: Option<&str>) -> Self {
        if let Some(agent) = agent {
            // The delegation names an agent, never its actor spelling.
            self.agent_id = Some(delegated_agent_name(agent).to_owned());
            self.session_delegated = true;
        } else {
            // A fresh admission to the person themselves: no earlier caller's
            // delegation may survive into it.
            self.agent_id = None;
            self.session_delegated = false;
        }
        self
    }

    /// The durable owner of an invocation this admitted identity invokes:
    /// [`InvocationOwner::AuthenticatedLocal`] naming the agent this session
    /// was handed to, or no agent for the person themselves. Both apis that
    /// admit a certified or local identity derive their owner here, so an
    /// invocation's durable owner, its receipt actor and every later
    /// validation name the same effective actor.
    pub(super) fn invocation_owner(&self) -> Result<InvocationOwner, String> {
        let cert = self
            .certificate
            .as_ref()
            .ok_or("admitted caller certificate missing")?;
        let owner = InvocationOwner::AuthenticatedLocal {
            root: cert.user_public_key,
            device: cert.device_public_key,
            agent: self.agent_id.clone(),
        };
        owner.validate()?;
        Ok(owner)
    }

    /// The durable owner of an invocation from a certified caller
    /// ([`VerifiedActionCaller`]).
    ///
    /// A session the person's own device handed to an agent is owned exactly
    /// as the local api owns that same admitted identity
    /// ([`Self::invocation_owner`]): [`InvocationOwner::AuthenticatedLocal`]
    /// naming the agent, so the durable owner, the receipt actor and every
    /// later validation agree on who acted. A plain certified person keeps
    /// [`InvocationOwner::Certified`], which is how their invocations have
    /// always been owned.
    ///
    /// A delegated session whose admission cannot name its owner (an
    /// admission made without a retained certificate, as a network API's
    /// can be) is refused here: falling back to [`InvocationOwner::Certified`]
    /// would silently drop the delegation the person's device granted.
    pub(super) fn delegated_invocation_owner(
        &self,
        delegated: Option<&str>,
    ) -> Result<InvocationOwner, String> {
        if let Some(agent) = delegated {
            // The person and device this admission proves, and the delegation
            // this caller says the person handed it.
            self.invocation_owner()?;
            if self.agent_id.as_deref() != Some(delegated_agent_name(agent)) {
                return Err("admitted caller does not carry its delegation".into());
            }
            return self.invocation_owner();
        }
        if self.agent_id.is_some() || self.session_delegated {
            return Err("admitted caller carries another agent's delegation".into());
        }
        Ok(self.person_owner())
    }

    /// The owner of an invocation by the certified person themselves, never
    /// an agent acting for them.
    pub(super) fn person_owner(&self) -> InvocationOwner {
        InvocationOwner::Certified {
            root: self.verified.user_public_key,
            device: self.verified.device_public_key,
        }
    }

    pub(super) fn broker_actor(&self) -> BrokerActor {
        let actor = match &self.agent_id {
            Some(agent) => {
                let actor = BrokerActor::agent(agent.clone()).for_person(self.actor_key.clone());
                let actor = if self.session_delegated {
                    actor.session_delegated()
                } else {
                    actor
                };
                // A certified agent works in its session alone.
                match &self.agent_cert {
                    Some(cert) => actor.in_session(cert.session.clone()),
                    None => actor,
                }
            }
            None => BrokerActor::person(self.actor_key.clone()),
        };
        actor.capped(self.verified.capped)
    }

    /// The caller this admitted connection is, confined to its workspace.
    pub(super) fn caller(&self, workspace: &str) -> tilo_actions::remote::Caller {
        let foreign = self
            .certificate
            .as_ref()
            .is_some_and(|cert| cert.capabilities.foreign);
        match &self.agent_id {
            // The agent acts for the person whose device this session is
            // ([`tilo_actions::remote::Caller::acting_for_person`]): every op it
            // makes and everything presence says about it names that person.
            Some(agent) => tilo_actions::remote::Caller::local_agent(agent.clone())
                .acting_for(self.actor_key.clone()),
            None => tilo_actions::remote::Caller::local_person(self.actor_key.clone()),
        }
        .in_workspace(workspace)
        .from_foreign_device(foreign)
    }

    /// The caller this certified identity is in `workspace`, from its own
    /// device: foreign when it was held to the cap there or the host keeps
    /// a stricter restriction for it.
    pub(super) fn certified_caller(
        &self,
        workspace: &str,
        restricted: bool,
    ) -> tilo_actions::remote::Caller {
        let verified = &self.verified;
        match &self.agent_id {
            Some(agent) => tilo_actions::remote::Caller::delegated_agent(
                agent.clone(),
                verified.user_public_key,
                verified.device_public_key,
            ),
            None => tilo_actions::remote::Caller::authenticated(
                verified.user_public_key,
                verified.device_public_key,
            ),
        }
        .in_workspace(workspace)
        .from_foreign_device(verified.capped || restricted)
    }

    /// Run `decide` with what this identity may do in `workspace` (its person
    /// workspace, or a workspace in `tree`).
    /// [`Self::with_authority`] over the committed authority of
    /// `membership`, borrowed for the one decision.
    pub(super) fn with_current_authority<T>(
        &self,
        membership: Option<&WorkspaceTreeHandle>,
        workspace: &str,
        decide: impl FnOnce(&tilo_action_policy::MemberAccess) -> T,
    ) -> Result<T, String> {
        match membership {
            Some(membership) => membership
                .decide_in_tree(|tree| self.with_authority(Some(tree), workspace, decide))
                .and_then(std::convert::identity),
            None => self.with_authority(None, workspace, decide),
        }
    }

    pub(super) fn with_authority<T>(
        &self,
        tree: Option<&Layout>,
        workspace_id: &str,
        decide: impl FnOnce(&tilo_action_policy::MemberAccess) -> T,
    ) -> Result<T, String> {
        if (self.agent_id.is_none() || self.session_delegated)
            && self.verified.owns_person_workspace(workspace_id)
        {
            // The own workspace's devices are its owner's; a foreign one is
            // held to the foreign-device bound there as anywhere, and an
            // agent one of them was handed to, to that delegation too.
            let policy = WorkspacePolicy::new(workspace_id, self.actor_key.clone());
            let access =
                tilo_action_policy::MemberAccess::new(policy, self.broker_actor().acting());
            return Ok(decide(&access));
        }
        let workspace = tree
            .ok_or("the identity has no workspace authority")?
            .workspaces
            .iter()
            .find(|workspace| workspace.id().to_string() == workspace_id)
            .ok_or("the identity's workspace is no longer hosted")?;
        Ok(decide(&self.broker_actor().access_in(workspace)))
    }
}

/// Answer a remote-action request with `error`, refusing it.
/// Whether `evidence` is what the confirmation `policy` asks for. Minting
/// the grant that binds it to one delivery is
/// [`AppBrokerState::confirmation_token`]'s job.
pub(super) fn confirmation_satisfied(
    policy: &tilo_actions::ConfirmationPolicy,
    evidence: Option<&tilo_actions::remote::Confirmation>,
) -> bool {
    use tilo_actions::remote::Confirmation;
    match (policy, evidence) {
        (tilo_actions::ConfirmationPolicy::None, _)
        | (tilo_actions::ConfirmationPolicy::Confirm, Some(Confirmation::Confirmed)) => true,
        (
            tilo_actions::ConfirmationPolicy::Phrase { phrase: expected },
            Some(Confirmation::Phrase(phrase)),
        ) => phrase.len() <= DIRECTORY_MAX_CONFIRMATION_BYTES && phrase == expected,
        _ => false,
    }
}

#[cfg(test)]
pub(super) fn admit_verified_join(
    tree: &Layout,
    route: &CrdtRoute,
    verified: VerifiedJoinIdentity,
) -> Result<AdmittedJoinIdentity, String> {
    admit_verified_join_at(
        crate::apps::services::workspace_in(tree, &route.cluster_id),
        route,
        verified,
        tilo_clock::try_unix_now().ok_or("legacy clock unavailable")?,
    )
}

/// Admit a verified identity into `route`'s workspace. `workspace` is the
/// committed workspace of exactly `route.cluster_id` from one authority
/// sample (`None` when this host has none).
pub(super) fn admit_verified_join_at(
    workspace: Option<&tilo_desktop_model::Workspace>,
    route: &CrdtRoute,
    verified: VerifiedJoinIdentity,
    now: u64,
) -> Result<AdmittedJoinIdentity, String> {
    let actor_key = person_id_for_root(verified.user_public_key);
    // The person's own workspace's devices are its owner's, by the
    // workspace id's derivation, exactly the devices this root certified, each
    // acting as the workspace's owner — a foreign device with the foreign-device
    // cap's role, there as in any workspace. Any other workspace id is a workspace.
    if verified.owns_person_workspace(&route.cluster_id) {
        let role = CapRole::Owner;
        return Ok(AdmittedJoinIdentity {
            worker_registration: None,
            certificate: None,
            verified,
            actor_key,
            role,
            display_handle: None,
            agent_id: None,
            session_delegated: false,
            agent_cert: None,
        });
    }
    let workspace = workspace
        .ok_or_else(|| "certified join names a workspace this session does not host".to_string())?;
    // Taking part is being allowed to read the workspace now: as a member, or
    // by a grant for a while.
    if !BrokerActor::person(actor_key.clone()).reads_in(workspace, now) {
        return Err("certified collaboration identity is not a workspace member".into());
    }
    let role = workspace.role_of(&actor_key).unwrap_or(CapRole::Viewer);
    Ok(AdmittedJoinIdentity {
        worker_registration: None,
        certificate: None,
        verified,
        actor_key,
        role,
        display_handle: None,
        agent_id: None,
        session_delegated: false,
        agent_cert: None,
    })
}

/// Whether the launched app `app` may join `route` in its person's own
/// workspace: only a record of the person's whose kind its surface declares
/// (its own, or another app's it reads). The person's workspace holds nothing
/// else for an app.
pub(super) fn person_document_join(app: &str, route: &CrdtRoute) -> Result<(), String> {
    let kind = collab_types::person_resource_type(&route.document_id)
        .ok_or("an app joins the person's workspace only for one of their records")?;
    if !collab_types::app_surface::keeps_person_resource(app, kind) {
        return Err(format!(
            "{app} declares no record of the person's of kind {kind}"
        ));
    }
    Ok(())
}

pub(super) fn verify_session_author(
    admitted: &AdmittedJoinIdentity,
    author: collab_types::Ed25519PublicKey,
) -> Result<(), String> {
    if author.as_bytes() != &admitted.verified.device_public_key {
        return Err("signed op author does not match the certified device for this session".into());
    }
    Ok(())
}

pub(super) fn verify_join_presentation(
    route: &CrdtRoute,
    presentation: &runtime_collab::CertifiedJoinIdentity,
    expected_user_public_key: &[u8; 32],
    now_ns: u64,
    revoked: bool,
    enrolled: bool,
) -> Result<VerifiedJoinIdentity, String> {
    if presentation.workspace_id != route.cluster_id {
        return Err("certified join is scoped to a different workspace".into());
    }
    let cert: tilo_user_identity::DeviceCert =
        serde_json::from_slice(&presentation.device_certificate)
            .map_err(|error| format!("device certificate is not valid JSON: {error}"))?;
    tilo_user_identity::verify_device_cert_at(&cert, now_ns)
        .map_err(|error| format!("device certificate was rejected: {error}"))?;
    if &cert.user_public_key != expected_user_public_key {
        return Err("device certificate belongs to another workspace identity".into());
    }
    if revoked {
        return Err("certified collaboration device has been revoked".into());
    }
    if !enrolled {
        return Err("certified collaboration device is not enrolled".into());
    }
    let proof = <[u8; 64]>::try_from(presentation.proof.as_slice())
        .map_err(|_| "collaboration join proof must be 64 bytes".to_string())?;
    let workspace = presentation.workspace_id.as_str();
    let digest = tilo_identity_client::collaboration_join_digest(workspace)
        .map_err(|error| format!("collaboration join cannot be encoded: {error}"))?;
    admission_memo::join_proof_verified(&cert.device_public_key, workspace, &proof, || {
        VerifyingKey::from_bytes(&cert.device_public_key)
            .map_err(|_| "device certificate contains an invalid device key".to_string())?
            .verify(&digest, &Signature::from_bytes(&proof))
            .map_err(|_| "collaboration join proof was signed by an unknown device".to_string())
    })?;
    // Without an authority to ask who governs the workspace, every foreign device
    // is held to the cap; the caller refines this where it has one.
    Ok(VerifiedJoinIdentity::of(&cert, cert.capabilities.foreign))
}

pub(super) fn verify_join_identity(
    route: &CrdtRoute,
    identity_bytes: &[u8],
    membership: Option<&WorkspaceTreeHandle>,
    trusted_devices: Option<&[[u8; 32]]>,
    authority: Option<&dyn crate::apps::certificate_authority::CertificateAuthority>,
) -> Result<AdmittedJoinIdentity, String> {
    if let Some(presentation) = runtime_collab::AgentJoinIdentity::decode(identity_bytes)
        .map_err(|error| format!("agent join presentation is malformed: {error}"))?
    {
        return verify_agent_join(route, &presentation, membership, trusted_devices, authority);
    }
    admit_join_presentation(
        route,
        &join_presentation(identity_bytes)?,
        membership,
        trusted_devices,
        authority,
    )
}

/// The certified presentation in a join's identity bytes.
pub(super) fn join_presentation(
    identity_bytes: &[u8],
) -> Result<runtime_collab::CertifiedJoinIdentity, String> {
    runtime_collab::CertifiedJoinIdentity::decode(identity_bytes)
        .map_err(|error| format!("certified join presentation is malformed: {error}"))?
        .ok_or_else(|| "collaboration join requires a certified device presentation".into())
}

pub(super) fn admit_join_presentation(
    route: &CrdtRoute,
    presentation: &runtime_collab::CertifiedJoinIdentity,
    membership: Option<&WorkspaceTreeHandle>,
    trusted_devices: Option<&[[u8; 32]]>,
    authority: Option<&dyn crate::apps::certificate_authority::CertificateAuthority>,
) -> Result<AdmittedJoinIdentity, String> {
    let cert: tilo_user_identity::DeviceCert =
        serde_json::from_slice(&presentation.device_certificate)
            .map_err(|error| format!("device certificate is not valid JSON: {error}"))?;
    let (expected_root, revoked, enrolled) =
        authorize_join_certificate(route, &cert, trusted_devices, authority)?;
    let registered = authority.map_or(
        Ok(None),
        crate::apps::certificate_authority::CertificateAuthority::policy_time,
    )?;
    let now_ns = policy_context::sample(registered.clone(), registered.as_ref())?.1;
    let verified = VerifiedJoinIdentity {
        capped: cert_capped_in(authority, &route.cluster_id, &cert),
        ..verify_join_presentation(
            route,
            presentation,
            &expected_root,
            now_ns,
            revoked,
            enrolled,
        )?
    };
    let membership = membership
        .ok_or_else(|| "certified join has no workspace membership authority".to_string())?;
    let mut admitted = membership.decide_in_workspace(&route.cluster_id, |workspace| {
        admit_verified_join_at(workspace, route, verified, now_ns / 1_000_000_000)
    })??;
    admitted.certificate = Some(cert);
    Ok(admitted)
}

/// Whether `cert` may join `route`: the workspace's certificate authority
/// (device list, revocations), else the trusted keys.
/// Answers the root expected to certify it, and whether it is revoked or enrolled.
pub(super) fn authorize_join_certificate(
    route: &CrdtRoute,
    cert: &tilo_user_identity::DeviceCert,
    trusted_devices: Option<&[[u8; 32]]>,
    authority: Option<&dyn crate::apps::certificate_authority::CertificateAuthority>,
) -> Result<([u8; 32], bool, bool), String> {
    Ok(if let Some(authority) = authority {
        let workspace =
            uuid::Uuid::parse_str(&route.cluster_id).map_err(|_| "invalid exact workspace")?;
        authority.authorize_join(workspace, cert)?;
        (cert.user_public_key, false, true)
    } else if let Some(devices) = trusted_devices {
        (
            cert.user_public_key,
            false,
            devices.contains(&cert.device_public_key),
        )
    } else {
        return Err("no current certificate authority is attached".into());
    })
}
