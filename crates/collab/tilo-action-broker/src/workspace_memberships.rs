//! Governed agent membership through the ordinary durable invocation surface.
//! Public arguments select only a closed command; caller custody and signing
//! authority remain host-owned.
use super::published_service::{ServiceOutcome, refused};
use super::{
    AppBrokerState, Arc, CrdtRoute, InvocationEnvelope, PublishedAction, PublishedInput,
    PublishedSurface,
};
use tilo_peer_state::owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_membership::{SettingsChange, WorkspaceMembershipCommand};

const APP: &str = "so.tilo.desktop";
const RECORD: &str = "membership";
const WORKSPACE: &str = "desktop:workspace";

/// Which of a workspace's two published surfaces: this device's own membership
/// changes, and the workspace's governance under the provider every client
/// names without a catalogue page.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Surface {
    Membership,
    Governance,
}

impl Surface {
    /// The application a surface's actions are published as.
    pub(super) const fn app_id(self) -> &'static str {
        match self {
            Self::Membership => APP,
            Self::Governance => tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER,
        }
    }
}
const GRANT_APP_AUTHORSHIP: &str = "grant-app-authorship";

impl AppBrokerState {
    /// [`Self::stage_workspace_membership`], started once its admission is
    /// durable, on a thread that may block: never the broker's loop.
    pub(crate) fn invoke_workspace_membership(
        &self,
        workspace: &str,
        command: &WorkspaceMembershipCommand,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<super::InvocationJob, String> {
        self.stage_workspace_membership(workspace, command, certificate)?
            .start_blocking(self)
    }

    /// Admit one membership command as `certificate`'s person. Nobody hears
    /// of it before its admission is durable: the caller starts it, waiting
    /// for that on its own thread or, on the broker's loop, without holding
    /// the loop.
    fn stage_workspace_membership(
        &self,
        workspace: &str,
        command: &WorkspaceMembershipCommand,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<super::StagedJob, String> {
        command.validate()?;
        let (resource, action, argument) = match command {
            WorkspaceMembershipCommand::RetiredAddAgent { .. }
            | WorkspaceMembershipCommand::RetiredRemoveAgent { .. } => {
                return Err(tilo_peer_state::workspace_membership::RETIRED_AGENT_MEMBERSHIP.into());
            }
            WorkspaceMembershipCommand::GrantAppAuthorship { app } => {
                let argument = format!(
                    "{}{}",
                    tilo_actions::ARGUMENTS_JSON_PREFIX,
                    serde_json::json!({"app": app})
                );
                (WORKSPACE.into(), GRANT_APP_AUTHORSHIP, Some(argument))
            }
            WorkspaceMembershipCommand::Grant { grant } => {
                let argument = format!(
                    "{}{}",
                    tilo_actions::ARGUMENTS_JSON_PREFIX,
                    serde_json::json!({ "grant": grant })
                );
                (WORKSPACE.into(), "grant", Some(argument))
            }
            WorkspaceMembershipCommand::Revoke { grant } => {
                let argument = format!(
                    "{}{}",
                    tilo_actions::ARGUMENTS_JSON_PREFIX,
                    serde_json::json!({ "grant": grant })
                );
                (WORKSPACE.into(), "revoke", Some(argument))
            }
            WorkspaceMembershipCommand::RetiredGrantForTask { .. }
            | WorkspaceMembershipCommand::RetiredRevokeForTask { .. } => {
                return Err(tilo_peer_state::workspace_membership::RETIRED_TASK_GRANTS.into());
            }
            WorkspaceMembershipCommand::InstallBot(_)
            | WorkspaceMembershipCommand::RenewBot(_)
            | WorkspaceMembershipCommand::AdoptBot(_)
            | WorkspaceMembershipCommand::UninstallBot { .. }
            | WorkspaceMembershipCommand::PauseBot { .. }
            | WorkspaceMembershipCommand::ResumeBot { .. }
            | WorkspaceMembershipCommand::SetBotAppearance { .. }
            | WorkspaceMembershipCommand::RequestBot(_)
            | WorkspaceMembershipCommand::WithdrawBotRequest { .. } => {
                let (action, argument) = bot_invocation(command)?;
                (WORKSPACE.into(), action, Some(argument))
            }
            WorkspaceMembershipCommand::Member { change } => (
                governance_resource(workspace),
                change.action(),
                Some(encoded(change)?),
            ),
            WorkspaceMembershipCommand::Settings(SettingsChange::Rename { name }) => (
                governance_resource(workspace),
                "rename",
                Some(encoded(&serde_json::json!({ "name": name }))?),
            ),
            WorkspaceMembershipCommand::Settings(_) => {
                return Err(
                    "a workspace's saved grant sets and app permissions change where its person changes them"
                        .into(),
                );
            }
            WorkspaceMembershipCommand::Share { .. }
            | WorkspaceMembershipCommand::Unshare { .. } => {
                return Err("a share is its sharer's own, made at their device".into());
            }
            WorkspaceMembershipCommand::InstallApp(_)
            | WorkspaceMembershipCommand::UninstallApp { .. } => {
                return Err(
                    "a workspace's app version is chosen on its host, through the workspace's `install-app`"
                        .into(),
                );
            }
        };
        self.invoke_workspace_person_action(
            workspace,
            &resource,
            action,
            argument.as_deref(),
            certificate,
        )
    }

    /// Let the app `route` names author its workspace before it opens there as
    /// `certificate`'s person, when the workspace does not let it yet and that
    /// person may let it there
    /// ([`super::services::WorkspaceMemberships::app_opens_by_granting`]):
    /// the same membership invocation a launch asks, started once its
    /// admission is durable. Nothing to do otherwise.
    ///
    /// A workspace this device's own host serves decides through this device's
    /// owner: what an app opening there needs is for the owner to record.
    pub(crate) async fn admit_opening_app(
        &self,
        route: &CrdtRoute,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<(), String> {
        if route.app_id.is_empty() {
            return Ok(());
        }
        let Some(memberships) = self.services.workspace_memberships.as_ref() else {
            return Ok(());
        };
        // This device hosts the workspace, or its own owner decides the same
        // question a member's owner does: either way the app opens here.
        let decided_here = certificate.capabilities.can_act_as_host
            || memberships.hosted_here(&route.cluster_id)
            || memberships.app_opens_by_granting(&route.cluster_id, &route.app_id, certificate);
        if !decided_here {
            return Ok(());
        }
        // This runs on the broker's loop, which answers every other app
        // meanwhile: the admission's write is waited for without holding it.
        let job = self
            .stage_workspace_membership(
                &route.cluster_id,
                &WorkspaceMembershipCommand::GrantAppAuthorship {
                    app: route.app_id.clone(),
                },
                certificate,
            )?
            .start(self)
            .await?;
        // Its answer comes when the workspace's owner has recorded the grant,
        // or when the invocation fails or expires. Nobody acts on it: the
        // app asks for its keys again when the grant is in. It is heard on
        // a thread of its own, which neither the broker's loop nor its
        // runtime's shutdown waits for.
        let workspace = route.cluster_id.clone();
        let app = route.app_id.clone();
        std::thread::Builder::new()
            .name("app-authorship".into())
            .spawn(move || match job.completion.recv() {
                Ok(completed) if completed.code.is_empty() => {}
                Ok(completed) => tracing::warn!(
                    %workspace,
                    %app,
                    error = %completed.message,
                    "an opening app was not let author its workspace"
                ),
                // ignored-failure: the caller is told the authorship answer was not heard
                Err(_) => tracing::debug!(
                    %workspace,
                    %app,
                    "an opening app's authorship ended without an answer"
                ),
            })
            .map(drop)
            .map_err(|error| format!("app authorship answer unheard: {error}"))
    }

    /// The workspace owner's commit that lets `app` author in `workspace`, as
    /// `certificate`'s person, this device, asks it on opening the app there:
    /// checked as that person's membership invocation is (an ordinary person
    /// device, admitted to the workspace, its certificate current), and handed to
    /// the workspace's owner, which decides it against the workspace's governance.
    ///
    /// Not journaled, unlike an invocation another actor asks: the grant is
    /// idempotent and the workspace's commit is its one durable record, so
    /// nothing is lost when a crash comes between the commit and an answer.
    /// The opening waits for the owner's commit alone, never for the
    /// journal's writes of other invocations queued before it, nor for the
    /// workspace's membership actions to be published.
    ///
    /// # Errors
    /// The workspace's membership is not held here, the device is not admitted
    /// to it, or the owner refused or could not commit the grant.
    /// What an opening in `workspace` waits for before its grant can be
    /// committed: nothing (`None`) when the workspace's membership is held here
    /// already, else to be told whether it comes
    /// ([`Self::await_membership_surface`]).
    pub(crate) fn membership_projection(
        &self,
        workspace: &str,
    ) -> Option<std::sync::mpsc::Receiver<bool>> {
        let service = self.services.workspace_memberships.as_ref()?;
        if service.requires_workspace(workspace) || service.grants_authorship_at_host(workspace) {
            return None;
        }
        Some(self.await_membership_surface(workspace))
    }

    pub(crate) fn app_authorship_commit(
        &self,
        workspace: &str,
        app: &str,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<
        (
            Arc<dyn crate::services::WorkspaceMemberships>,
            OwnerCommitRequest,
        ),
        String,
    > {
        let command = WorkspaceMembershipCommand::GrantAppAuthorship {
            app: app.to_owned(),
        };
        command.validate()?;
        let service = self
            .services
            .workspace_memberships
            .clone()
            .ok_or("workspace membership owner unavailable")?;
        if !(service.requires_workspace(workspace) || service.grants_authorship_at_host(workspace))
        {
            return Err("the workspace's membership is not held on this device".into());
        }
        if certificate.capabilities.can_act_as_host {
            return Err("membership changes require an ordinary person device".into());
        }
        let authority = self
            .certificate_authority
            .get()
            .ok_or("current certificate authority unavailable")?;
        let workspace_id =
            uuid::Uuid::parse_str(workspace).map_err(|_| "invalid exact workspace")?;
        authority.authorize_join(workspace_id, certificate)?;
        let time = self.registered_policy_time()?;
        let now = self.checked_policy_time(time.as_ref())?.1;
        tilo_user_identity::verify_device_cert_at(certificate, now)
            .map_err(|_| "membership device certificate rejected")?;
        let membership = self
            .membership
            .as_ref()
            .ok_or("workspace authority unavailable")?;
        let verified = super::VerifiedJoinIdentity::of(
            certificate,
            super::cert_capped_in(Some(authority.as_ref()), workspace, certificate),
        );
        let route = CrdtRoute::record(APP, workspace, RECORD);
        membership.decide_in_workspace(workspace, |workspace| {
            super::admit_verified_join_at(workspace, &route, verified, now / 1_000_000_000)
        })??;
        // The owner's commit waits for an owner still opening (a session's
        // first moments) inside its own future, as every commit does.
        Ok((
            service,
            OwnerCommitRequest {
                workspace: workspace.into(),
                operation: CommitOperation::WorkspaceMembershipCommand {
                    invocation: uuid::Uuid::new_v4(),
                    caller: certificate.clone(),
                    command,
                },
            },
        ))
    }

    fn invoke_workspace_person_action(
        &self,
        workspace: &str,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<super::StagedJob, String> {
        let route = CrdtRoute::record(APP, workspace, RECORD);
        // The workspace publishes its own changes and its governance on one
        // route: which surface a command goes to is the one that offers the
        // resource and action it names, not the surface it was filed under.
        let peers = self
            .surfaces
            .lock()
            .map_err(|_| "membership surfaces unavailable")?
            .values()
            .filter(|surface| {
                surface.live
                    && surface.route == route
                    && surface.actions.iter().any(|candidate| {
                        candidate.resource == resource && candidate.action == action
                    })
            })
            .map(|surface| surface.peer_id)
            .collect::<Vec<_>>();
        let [peer] = peers.as_slice() else {
            return Err(if peers.is_empty() {
                "workspace membership action is not published"
            } else {
                "workspace membership action publication is ambiguous"
            }
            .into());
        };
        if certificate.capabilities.can_act_as_host {
            return Err("membership changes require an ordinary person device".into());
        }
        let authority = self
            .certificate_authority
            .get()
            .ok_or("current certificate authority unavailable")?;
        let workspace_id =
            uuid::Uuid::parse_str(workspace).map_err(|_| "invalid exact workspace")?;
        authority.authorize_join(workspace_id, certificate)?;
        let time = self.registered_policy_time()?;
        let now = self.checked_policy_time(time.as_ref())?.1;
        tilo_user_identity::verify_device_cert_at(certificate, now)
            .map_err(|_| "membership device certificate rejected")?;
        let membership = self
            .membership
            .as_ref()
            .ok_or("workspace authority unavailable")?;
        let verified = super::VerifiedJoinIdentity::of(
            certificate,
            super::cert_capped_in(Some(authority.as_ref()), workspace, certificate),
        );
        let mut admitted = membership.decide_in_workspace(&route.cluster_id, |workspace| {
            super::admit_verified_join_at(workspace, &route, verified, now / 1_000_000_000)
        })??;

        admitted.certificate = Some(certificate.clone());
        let actor = admitted.broker_actor();
        self.start_invocation_confirmed_for_origin(
            *peer,
            resource,
            action,
            super::InvocationInputs {
                original_request: None,
                requested: None,
                caller: None,
                execution: None,
                argument,
                actor: &actor,
                confirmation_token: None,
                context: None,
            },
            Some(super::InvocationCaller::Local {
                route,
                admitted: Box::new(admitted),
                time,
            }),
        )
    }

    /// Start one change to a workspace's people, addressed under the provider
    /// every client names without a catalogue page: the change is made here,
    /// where its person is, and admitted by every device
    /// ([`people_command`]).
    pub(super) fn start_governance_action(
        self: &Arc<Self>,
        caller: &tilo_actions::remote::Caller,
        origin: Option<&super::InvocationCaller>,
        original_request: &tilo_actions::remote::InvokeRequest,
        request: &tilo_actions::remote::InvokeRequest,
        argument: Option<&str>,
    ) -> Result<super::action_host::Started, tilo_actions::remote::ActionHostError> {
        use tilo_actions::remote::ActionHostError;
        let address = &request.target.address;
        let Some(workspace) = address
            .resource
            .strip_prefix(&format!(
                "{}:",
                tilo_actions::remote::WORKSPACE_GOVERNANCE_KIND
            ))
            .filter(|workspace| !workspace.is_empty())
        else {
            return Err(ActionHostError::UnknownAction);
        };
        let Some((surface, action)) =
            self.governance_surface(workspace, &address.resource, &address.action)
        else {
            return Err(ActionHostError::UnknownAction);
        };
        let Some((committed, actor, now)) =
            self.decide_for(caller, workspace, |committed, actor, now| {
                (committed.clone(), actor.clone(), now)
            })
        else {
            return Err(ActionHostError::UnknownAction);
        };
        // A workspace's people change at its own host person's device, never
        // by an agent acting for them.
        if actor.direct_person().is_none() {
            return Err(ActionHostError::NotPermitted);
        }
        let need = action.contract.need();
        let node = tilo_action_policy::roles::governance_action(workspace, &address.action);
        if !actor
            .answer_in(&committed, &node, need.kind, now)
            .admits(actor.direct())
        {
            return Err(ActionHostError::NotPermitted);
        }
        let token = self.confirmation_token(
            surface.peer_id,
            &surface.app_id,
            &action,
            argument,
            &actor,
            request.confirmation.as_ref(),
            &self.witness_binding(actor.id(), workspace, original_request),
        )?;
        self.start_invocation_confirmed_for_origin(
            surface.peer_id,
            &address.resource,
            &address.action,
            super::InvocationInputs {
                original_request: Some(original_request),
                caller: Some(caller),
                requested: Some(request.invocation),
                execution: super::ExecutionScope::for_call(caller, request.invocation),
                argument,
                actor: &actor,
                confirmation_token: token.as_deref(),
                context: request.context.clone(),
            },
            origin.cloned(),
        )
        .map(super::action_host::Started::Staged)
        .map_err(super::action_host::refusal)
    }

    /// The published surface of `workspace`'s governance offering `action` on
    /// `resource`, with the action it publishes: one surface, so a change goes
    /// to the one place that makes it.
    fn governance_surface(
        &self,
        workspace: &str,
        resource: &str,
        action: &str,
    ) -> Option<(PublishedSurface, PublishedAction)> {
        let offered = {
            let surfaces = self.surfaces.lock().ok()?;
            surfaces
                .values()
                .filter(|surface| {
                    surface.live
                        && surface.app_id == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER
                        && surface.route.cluster_id == workspace
                })
                .filter_map(|surface| {
                    let action = surface
                        .actions
                        .iter()
                        .find(|candidate| {
                            candidate.resource == resource && candidate.action == action
                        })?
                        .clone();
                    Some((surface.clone(), action))
                })
                .collect::<Vec<_>>()
        };
        let mut offered = offered.into_iter();
        let one = offered.next()?;
        offered.next().is_none().then_some(one)
    }

    /// Whether `workspace`'s membership actions are published on this broker.
    pub(crate) fn membership_published(&self, workspace: &str) -> bool {
        self.workspace_membership_peers
            .lock()
            .is_ok_and(|peers| peers.contains_key(workspace))
    }

    /// Told whether `workspace`'s membership actions are published: at once when
    /// they are, else after the first refresh of the broker's membership
    /// surfaces that publishes them, which this asks for. A workspace this device
    /// just made or joined is published once its replica is projected here,
    /// which may come refreshes later; an app opening there right away asks
    /// its membership of the workspace only then. Told that they are not once a
    /// refresh finds the workspace unpublished and not on its way
    /// ([`crate::services::WorkspaceMemberships::membership_pending`]). The
    /// receiver's sender drops with the broker.
    pub(crate) fn await_membership_surface(
        &self,
        workspace: &str,
    ) -> std::sync::mpsc::Receiver<bool> {
        let (told, published) = std::sync::mpsc::channel();
        if self.membership_published(workspace) {
            let _ = told.send(true);
            return published;
        }
        if let Ok(mut waiting) = self.membership_surface_waiters.lock() {
            waiting.push((workspace.to_owned(), told));
        }
        tilo_identity_host::policy_change::announce();
        published
    }

    /// The opens waiting for a membership refresh, taken before it reads
    /// the workspaces ([`Self::await_membership_surface`]).
    pub(super) fn take_membership_surface_waiters(
        &self,
    ) -> Vec<(String, std::sync::mpsc::Sender<bool>)> {
        self.membership_surface_waiters
            .lock()
            .map(|mut waiting| std::mem::take(&mut *waiting))
            .unwrap_or_default()
    }

    /// Now that the refresh `waiting` waited for ran, tell those whose
    /// workspaces' membership actions it published that they are, and those
    /// whose workspaces are not on their way that they will not be; the rest
    /// wait for the refresh that follows the workspace's projection.
    pub(super) fn answer_membership_surface_waiters(
        &self,
        waiting: Vec<(String, std::sync::mpsc::Sender<bool>)>,
    ) {
        let service = self.services.workspace_memberships.as_ref();
        let mut still = Vec::new();
        for (workspace, told) in waiting {
            if self.membership_published(&workspace) {
                let _ = told.send(true);
            } else if service.is_some_and(|service| service.membership_pending(&workspace)) {
                still.push((workspace, told));
            } else {
                let _ = told.send(false);
            }
        }
        if still.is_empty() {
            return;
        }
        if let Ok(mut waiting) = self.membership_surface_waiters.lock() {
            waiting.extend(still);
        }
    }

    pub(super) fn refresh_workspace_membership_services(self: &Arc<Self>) -> Result<(), String> {
        let Some(service) = self.services.workspace_memberships.clone() else {
            return Ok(());
        };
        let Some(authority) = &self.membership else {
            return Ok(());
        };
        // Describe every workspace on one borrowed sample; the membership
        // service is asked about each workspace only after the borrow ends.
        let workspaces = authority.decide_in_tree(|tree| {
            tree.workspaces
                .iter()
                .map(|workspace| (workspace.id().to_string(), membership_actions()))
                .collect::<Vec<_>>()
        })?;
        for (workspace, actions) in workspaces {
            // A workspace its host stewards offers its membership changes here
            // too: this device writes them itself, as in any workspace it holds.
            if !(service.requires_workspace(&workspace)
                || service.grants_authorship_at_host(&workspace))
            {
                continue;
            }
            let governance = people_actions(&governance_resource(&workspace));
            let membership = self.publish_workspace_surface(
                Surface::Membership,
                &workspace,
                actions,
                service.clone(),
            )?;
            let people = self.publish_workspace_surface(
                Surface::Governance,
                &workspace,
                governance,
                service.clone(),
            )?;
            if membership || people {
                self.publish_changed_action_catalogue();
            }
        }
        Ok(())
    }

    /// Publish `actions` as the workspace's `surface` and say the action
    /// catalogue changed, or say the same actions are published already: a
    /// workspace keeps one surface of each kind, replaced in place as what it
    /// offers changes.
    fn publish_workspace_surface(
        self: &Arc<Self>,
        surface: Surface,
        workspace: &str,
        actions: Vec<PublishedAction>,
        service: Arc<dyn crate::services::WorkspaceMemberships>,
    ) -> Result<bool, String> {
        let peers = self.workspace_surface_peers(surface);
        let existing = peers
            .lock()
            .map_err(|_| "workspace surface registry unavailable")?
            .get(workspace)
            .copied();
        if let Some(peer) = existing {
            let changed = {
                let mut surfaces = self
                    .surfaces
                    .lock()
                    .map_err(|_| "membership surfaces unavailable")?;
                surfaces.get_mut(&peer).is_some_and(|published| {
                    if published.actions == actions {
                        return false;
                    }
                    published.actions = actions;
                    true
                })
            };
            return Ok(changed);
        }
        self.start_workspace_surface(surface, workspace.to_owned(), actions, service)
    }

    fn start_workspace_surface(
        self: &Arc<Self>,
        which: Surface,
        workspace: String,
        actions: Vec<PublishedAction>,
        service: Arc<dyn crate::services::WorkspaceMemberships>,
    ) -> Result<bool, String> {
        let peer = self.next_service_peer_id();
        let surface = PublishedSurface {
            peer_id: peer,
            app_id: which.app_id().into(),
            route: CrdtRoute::record(APP, &workspace, RECORD),
            live: true,
            actions,
        };
        self.workspace_surface_peers(which)
            .lock()
            .map_err(|_| "workspace surface registry unavailable")?
            .insert(workspace.clone(), peer);
        let served_workspace = workspace.clone();
        self.serve_published(
            surface,
            move |state, envelope| {
                let (service, workspace) = (service.clone(), served_workspace.clone());
                async move {
                    match state
                        .execute_workspace_membership(peer, &workspace, &envelope, service.as_ref())
                        .await
                    {
                        Ok(value) => ServiceOutcome::Succeeded(value),
                        Err(error) => refused("workspace_membership_refused", error),
                    }
                }
            },
            move |state, removed| {
                if removed {
                    state.publish_changed_action_catalogue();
                }
                if let Ok(mut peers) = state.workspace_surface_peers(which).lock() {
                    peers.remove(&workspace);
                }
            },
        )?;
        Ok(true)
    }

    async fn execute_workspace_membership(
        &self,
        peer: u64,
        workspace: &str,
        envelope: &InvocationEnvelope,
        service: &dyn crate::services::WorkspaceMemberships,
    ) -> Result<serde_json::Value, String> {
        let pending = self
            .pending_invocations
            .lock()
            .map_err(|_| "invocation registry unavailable")?
            .get(&envelope.id)
            .filter(|pending| {
                pending.peer_id == peer && pending.update.invocation() == envelope.invocation
            })
            .cloned()
            .ok_or("membership invocation is no longer admitted")?;
        let key = pending
            .journal_key
            .as_ref()
            .ok_or("membership invocation has no durable admission")?;
        if key.workspace != workspace
            || key.invocation != envelope.invocation
            || key.owner != pending.owner
            || !(service.requires_workspace(workspace)
                || service.grants_authorship_at_host(workspace))
        {
            return Err("membership invocation binding changed".into());
        }
        let original = pending
            .original
            .as_ref()
            .ok_or("membership original custody unavailable")?;
        let caller = original.owner_person_certificate(self, workspace, envelope)?;
        let command = membership_command(envelope)?;
        let request = OwnerCommitRequest {
            workspace: workspace.into(),
            operation: CommitOperation::WorkspaceMembershipCommand {
                invocation: envelope.invocation.0,
                caller,
                command,
            },
        };
        let receipt = service.execute(request.clone()).await?;
        receipt.validate_for(&request)?;
        Ok(serde_json::json!({"generation": receipt.generation}))
    }
}

fn membership_command(envelope: &InvocationEnvelope) -> Result<WorkspaceMembershipCommand, String> {
    if let Some(command) = people_command(envelope)? {
        return Ok(command);
    }
    let command = if let Some(command) = bot_command(envelope)? {
        command
    } else if envelope.resource == WORKSPACE && envelope.action == "grant-app-authorship" {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct AppAuthorship {
            app: String,
        }
        let encoded = bounded_argument(envelope, "app authorship")?;
        let input: AppAuthorship =
            serde_json::from_str(encoded).map_err(|error| error.to_string())?;
        WorkspaceMembershipCommand::GrantAppAuthorship { app: input.app }
    } else if envelope.resource == WORKSPACE && envelope.action == "grant" {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Give {
            grant: tilo_action_policy::Grant,
        }
        let encoded = bounded_argument(envelope, "grant")?;
        let input: Give = serde_json::from_str(encoded).map_err(|error| error.to_string())?;
        WorkspaceMembershipCommand::Grant { grant: input.grant }
    } else if envelope.resource == WORKSPACE && envelope.action == "revoke" {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct TakeBack {
            grant: tilo_action_policy::GrantId,
        }
        let encoded = bounded_argument(envelope, "grant")?;
        let input: TakeBack = serde_json::from_str(encoded).map_err(|error| error.to_string())?;
        WorkspaceMembershipCommand::Revoke { grant: input.grant }
    } else {
        return Err("unknown workspace membership command".into());
    };
    command.validate()?;
    Ok(command)
}

/// The address a change to a workspace's people is made at: the governance
/// resource every client names, `workspace.permissions:<workspace>`.
fn governance_resource(workspace: &str) -> String {
    tilo_actions::remote::workspace_governance_resource(workspace)
}

/// `value` as an invocation's argument, under the prefix the broker's own
/// actions carry.
fn encoded(value: &impl serde::Serialize) -> Result<String, String> {
    let json = serde_json::to_string(value).map_err(|error| error.to_string())?;
    Ok(format!("{}{json}", tilo_actions::ARGUMENTS_JSON_PREFIX))
}

/// The change to a workspace's people `envelope` asks for, when its address is
/// the workspace's governance: a role, a removal, a ban or a restriction, or
/// the workspace's own name. `None` for every other address.
fn people_command(
    envelope: &InvocationEnvelope,
) -> Result<Option<WorkspaceMembershipCommand>, String> {
    let prefix = format!("{}:", tilo_actions::remote::WORKSPACE_GOVERNANCE_KIND);
    let Some(workspace) = envelope.resource.strip_prefix(&prefix) else {
        return Ok(None);
    };
    if workspace.is_empty() {
        return Err("a workspace's governance names its workspace".into());
    }
    if envelope.action == "rename" {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Rename {
            name: String,
        }
        let rename: Rename =
            serde_json::from_str(bounded_argument(envelope, "the workspace's name")?)
                .map_err(|error| error.to_string())?;
        return Ok(Some(WorkspaceMembershipCommand::Settings(
            SettingsChange::Rename { name: rename.name },
        )));
    }
    let change: tilo_action_policy::Change =
        serde_json::from_str(bounded_argument(envelope, "the change")?)
            .map_err(|error| error.to_string())?;
    if change.action() != envelope.action {
        return Err(format!(
            "`{}` is not a change to {workspace}'s people",
            envelope.action
        ));
    }
    Ok(Some(WorkspaceMembershipCommand::Member { change }))
}

/// The bot commands an envelope names: their arguments are the whole
/// payload, as the governor checks it.
fn bot_command(
    envelope: &InvocationEnvelope,
) -> Result<Option<WorkspaceMembershipCommand>, String> {
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Install {
        install: tilo_peer_state::workspace_membership::BotInstall,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Renew {
        renewal: tilo_peer_state::workspace_membership::BotRenewal,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Adopt {
        adoption: tilo_peer_state::workspace_membership::BotAdoption,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Named {
        installation: tilo_peer_state::InstallationId,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Appearance {
        installation: tilo_peer_state::InstallationId,
        appearance: tilo_peer_state::AppearancePolicy,
    }
    #[derive(serde::Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Entry {
        entry: String,
    }
    fn read<T: serde::de::DeserializeOwned>(
        envelope: &InvocationEnvelope,
        name: &str,
    ) -> Result<T, String> {
        serde_json::from_str(bounded_argument(envelope, name)?).map_err(|error| error.to_string())
    }
    if envelope.resource != WORKSPACE {
        return Ok(None);
    }
    Ok(Some(match envelope.action.as_str() {
        "install-bot" => {
            let encoded = bounded_argument(envelope, "bot install")?;
            let input: Install =
                serde_json::from_str(encoded).map_err(|error| error.to_string())?;
            WorkspaceMembershipCommand::InstallBot(Box::new(input.install))
        }
        "renew-bot" => {
            let encoded = bounded_argument(envelope, "bot renewal")?;
            let input: Renew = serde_json::from_str(encoded).map_err(|error| error.to_string())?;
            WorkspaceMembershipCommand::RenewBot(Box::new(input.renewal))
        }
        "adopt-bot" => WorkspaceMembershipCommand::AdoptBot(Box::new(
            read::<Adopt>(envelope, "bot adoption")?.adoption,
        )),
        "uninstall-bot" => WorkspaceMembershipCommand::UninstallBot {
            installation: read::<Named>(envelope, "bot")?.installation,
        },
        "pause-bot" => WorkspaceMembershipCommand::PauseBot {
            installation: read::<Named>(envelope, "bot")?.installation,
        },
        "resume-bot" => WorkspaceMembershipCommand::ResumeBot {
            installation: read::<Named>(envelope, "bot")?.installation,
        },
        "set-bot-appearance" => {
            let input = read::<Appearance>(envelope, "bot appearance")?;
            WorkspaceMembershipCommand::SetBotAppearance {
                installation: input.installation,
                appearance: input.appearance,
            }
        }
        "request-bot" => WorkspaceMembershipCommand::RequestBot(read::<
            tilo_peer_state::workspace_membership::BotRequest,
        >(envelope, "bot request")?),
        "withdraw-bot-request" => WorkspaceMembershipCommand::WithdrawBotRequest {
            entry: read::<Entry>(envelope, "bot request")?.entry,
        },
        _ => return Ok(None),
    }))
}

fn bounded_argument<'a>(envelope: &'a InvocationEnvelope, name: &str) -> Result<&'a str, String> {
    let encoded = envelope
        .argument
        .as_deref()
        .ok_or_else(|| format!("{name} arguments missing"))?;
    if encoded.len() > argument_limit(&envelope.action) {
        return Err(format!("{name} arguments exceed bounded size"));
    }
    Ok(encoded
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .unwrap_or(encoded))
}

/// Most bytes an action's arguments may take. Installing a bot carries its
/// whole program, and a bot's device carries a certificate and delegation;
/// every other membership command is a few ids.
fn argument_limit(action: &str) -> usize {
    match action {
        "install-bot" | "renew-bot" | "adopt-bot" => {
            tilo_automation::program::MAX_PROGRAM_BYTES + 64 * 1024
        }
        _ => 1024,
    }
}

fn membership_actions() -> Vec<PublishedAction> {
    let mut actions = vec![membership_action(
        WORKSPACE,
        GRANT_APP_AUTHORSHIP,
        "Workspace",
        "Allow app authorship",
        vec![tilo_actions::ActionParam::required(
            "app",
            tilo_actions::ParamType::Text,
            "Application id",
        )],
    )];
    actions.push(membership_action(
        WORKSPACE,
        "grant",
        "Workspace",
        "Give a grant",
        vec![tilo_actions::ActionParam::required(
            "grant",
            tilo_actions::ParamType::Object,
            "The grant: id, subject, node, kinds, answer, lifetime",
        )],
    ));
    actions.push(membership_action(
        WORKSPACE,
        "revoke",
        "Workspace",
        "Take back a grant",
        vec![tilo_actions::ActionParam::required(
            "grant",
            tilo_actions::ParamType::Text,
            "The grant's id",
        )],
    ));
    actions.extend(bot_actions());
    actions
}

/// The changes to a workspace's people, as the workspace's governance offers
/// them: the verbs its clients already name (`tilo_action_policy::roles::
/// governance`), made here at this device and admitted by every device.
fn people_actions(resource: &str) -> Vec<PublishedAction> {
    use tilo_action_policy::roles::governance;
    let person = || {
        tilo_actions::ActionParam::required(
            "person",
            tilo_actions::ParamType::Text,
            "The member's person: their user root key, 64 hex characters.",
        )
    };
    let role = || {
        tilo_actions::ActionParam::required(
            "role",
            tilo_actions::ParamType::Text,
            "The role they get: viewer, commenter, editor or moderator.",
        )
    };
    vec![
        people_action(
            resource,
            governance::ADD_MEMBER,
            "People",
            "Add a member",
            vec![person(), role()],
        ),
        people_action(
            resource,
            governance::SET_ROLE,
            "People",
            "Change a member's role",
            vec![person(), role()],
        ),
        people_action(
            resource,
            governance::REMOVE_MEMBER,
            "People",
            "Remove a member",
            vec![person()],
        ),
        people_action(resource, governance::BAN, "People", "Ban", vec![person()]),
        people_action(
            resource,
            governance::UNBAN,
            "People",
            "Lift a ban",
            vec![person()],
        ),
        people_action(
            resource,
            governance::RESTRICT,
            "People",
            "Restrict a member",
            vec![
                person(),
                tilo_actions::ActionParam::required(
                    "until_ms",
                    tilo_actions::ParamType::Integer,
                    "Until when they may change nothing, in Unix milliseconds.",
                ),
            ],
        ),
        people_action(
            resource,
            governance::UNRESTRICT,
            "People",
            "Lift a restriction",
            vec![person()],
        ),
        people_action(
            resource,
            "rename",
            "Workspace",
            "Rename the workspace",
            vec![tilo_actions::ActionParam::required(
                "name",
                tilo_actions::ParamType::Text,
                "The workspace's new name.",
            )],
        ),
    ]
}

/// One of them, published as the workspace's governance resource its clients
/// already look for.
fn people_action(
    resource: &str,
    action: &str,
    label: &str,
    action_label: &str,
    params: Vec<tilo_actions::ActionParam>,
) -> PublishedAction {
    let mut published = membership_action(resource, action, label, action_label, params);
    published.resource_kind = tilo_actions::remote::WORKSPACE_GOVERNANCE_KIND.into();
    published
}

/// The membership action and arguments a bot command travels as.
fn bot_invocation(command: &WorkspaceMembershipCommand) -> Result<(&'static str, String), String> {
    Ok(match command {
        WorkspaceMembershipCommand::InstallBot(install) => {
            let argument = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::json!({ "install": install })
            );
            ("install-bot", argument)
        }
        WorkspaceMembershipCommand::RenewBot(renewal) => {
            let argument = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::json!({ "renewal": renewal })
            );
            ("renew-bot", argument)
        }
        WorkspaceMembershipCommand::AdoptBot(adoption) => {
            let argument = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::json!({ "adoption": adoption })
            );
            ("adopt-bot", argument)
        }
        WorkspaceMembershipCommand::UninstallBot { installation }
        | WorkspaceMembershipCommand::PauseBot { installation }
        | WorkspaceMembershipCommand::ResumeBot { installation } => {
            let action = match command {
                WorkspaceMembershipCommand::UninstallBot { .. } => "uninstall-bot",
                WorkspaceMembershipCommand::PauseBot { .. } => "pause-bot",
                _ => "resume-bot",
            };
            let argument = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::json!({ "installation": installation })
            );
            (action, argument)
        }
        WorkspaceMembershipCommand::SetBotAppearance {
            installation,
            appearance,
        } => {
            let argument = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::json!({ "installation": installation, "appearance": appearance })
            );
            ("set-bot-appearance", argument)
        }
        WorkspaceMembershipCommand::RequestBot(request) => {
            let argument = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::json!({ "entry": request.entry, "version": request.version })
            );
            ("request-bot", argument)
        }
        WorkspaceMembershipCommand::WithdrawBotRequest { entry } => {
            let argument = format!(
                "{}{}",
                tilo_actions::ARGUMENTS_JSON_PREFIX,
                serde_json::json!({ "entry": entry })
            );
            ("withdraw-bot-request", argument)
        }
        _ => return Err("not a bot command".into()),
    })
}

/// The governor's bot commands, as membership actions.
fn bot_actions() -> Vec<PublishedAction> {
    let mut actions = Vec::new();
    actions.push(membership_action(
        WORKSPACE,
        "install-bot",
        "Workspace",
        "Install a bot",
        vec![tilo_actions::ActionParam::required(
            "install",
            tilo_actions::ParamType::Object,
            "The installation, its program text and, for a new bot, its device",
        )],
    ));
    actions.push(membership_action(
        WORKSPACE,
        "renew-bot",
        "Workspace",
        "Renew a bot's authorization",
        vec![tilo_actions::ActionParam::required(
            "renewal",
            tilo_actions::ParamType::Object,
            "The installation and its device with a fresh delegation",
        )],
    ));
    actions.push(membership_action(
        WORKSPACE,
        "adopt-bot",
        "Workspace",
        "Adopt a bot",
        vec![tilo_actions::ActionParam::required(
            "adoption",
            tilo_actions::ParamType::Object,
            "The installation and the adopter's device and delegation",
        )],
    ));
    for (action, label) in [
        ("uninstall-bot", "Uninstall a bot"),
        ("pause-bot", "Pause a bot"),
        ("resume-bot", "Resume a bot"),
    ] {
        actions.push(membership_action(
            WORKSPACE,
            action,
            "Workspace",
            label,
            vec![tilo_actions::ActionParam::required(
                "installation",
                tilo_actions::ParamType::Text,
                "Installation id",
            )],
        ));
    }
    actions.push(membership_action(
        WORKSPACE,
        "set-bot-appearance",
        "Workspace",
        "Change how a bot may appear",
        vec![
            tilo_actions::ActionParam::required(
                "installation",
                tilo_actions::ParamType::Text,
                "Installation id",
            ),
            tilo_actions::ActionParam::required(
                "appearance",
                tilo_actions::ParamType::Text,
                "own, or personas to post as other names",
            ),
        ],
    ));
    actions.push(membership_action(
        WORKSPACE,
        "request-bot",
        "Workspace",
        "Add a bot",
        vec![
            tilo_actions::ActionParam::required(
                "entry",
                tilo_actions::ParamType::Text,
                "The bot's directory entry",
            ),
            tilo_actions::ActionParam::required(
                "version",
                tilo_actions::ParamType::Text,
                "The listed version to run",
            ),
        ],
    ));
    actions.push(membership_action(
        WORKSPACE,
        "withdraw-bot-request",
        "Workspace",
        "Remove a bot",
        vec![tilo_actions::ActionParam::required(
            "entry",
            tilo_actions::ParamType::Text,
            "The bot's directory entry",
        )],
    ));
    actions
}

fn membership_action(
    resource: &str,
    action: &str,
    label: &str,
    action_label: &str,
    params: Vec<tilo_actions::ActionParam>,
) -> PublishedAction {
    PublishedAction {
        state: tilo_actions::ActionState::default(),
        semantic_identity: None,
        resource: resource.into(),
        action: action.into(),
        resource_kind: "workspace-membership".into(),
        resource_label: label.into(),
        action_label: action_label.into(),
        keywords: Vec::new(),
        icon: None,
        description: None,
        operation_kind: tilo_actions::OperationKind::Effect,
        params,
        contract: tilo_actions::ActionContract {
            kind: tilo_actions::ActionKind::Outside,
            result: tilo_actions::ResultSpec::Json {
                schema: serde_json::json!({"type": "object"}),
            },
            ..Default::default()
        },
        view: None,
        input: PublishedInput::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{HostServices, WorkspaceMemberships};

    struct DiscoveryMemberships;

    /// A service standing in for a workspace this device holds but a host
    /// stewards: the workspace's own membership changes are what it publishes.
    struct HostedElsewhereMemberships;

    impl WorkspaceMemberships for HostedElsewhereMemberships {
        fn requires_workspace(&self, _: &str) -> bool {
            false
        }

        fn grants_authorship_at_host(&self, _: &str) -> bool {
            true
        }

        fn health(&self) -> Result<(), String> {
            Ok(())
        }

        fn execute(&self, _: OwnerCommitRequest) -> tilo_peer_state::owner_commit::CommitFuture {
            Box::pin(async { Err("discovery fixture must not mutate the owner".into()) })
        }
    }

    impl WorkspaceMemberships for DiscoveryMemberships {
        fn requires_workspace(&self, _: &str) -> bool {
            true
        }

        fn health(&self) -> Result<(), String> {
            Ok(())
        }

        fn execute(&self, _: OwnerCommitRequest) -> tilo_peer_state::owner_commit::CommitFuture {
            Box::pin(async { Err("discovery fixture must not mutate the owner".into()) })
        }
    }

    /// A service standing in for a workspace this device has just made: held,
    /// and published once its replica is projected, which `projected` says.
    struct ProjectedLaterMemberships {
        workspace: String,
        projected: std::sync::atomic::AtomicBool,
    }

    impl WorkspaceMemberships for ProjectedLaterMemberships {
        fn requires_workspace(&self, workspace: &str) -> bool {
            workspace == self.workspace && self.projected.load(std::sync::atomic::Ordering::SeqCst)
        }

        fn membership_pending(&self, workspace: &str) -> bool {
            workspace == self.workspace
        }

        fn health(&self) -> Result<(), String> {
            Ok(())
        }

        fn execute(&self, _: OwnerCommitRequest) -> tilo_peer_state::owner_commit::CommitFuture {
            Box::pin(async { Err("discovery fixture must not mutate the owner".into()) })
        }
    }

    fn membership_surface_counts(state: &AppBrokerState, workspace: &str) -> (usize, usize) {
        let surfaces = state.published_surfaces();
        let matching = surfaces
            .iter()
            .filter(|surface| {
                surface.app_id == APP
                    && surface.route.record_id == RECORD
                    && surface.route.cluster_id == workspace
            })
            .collect::<Vec<_>>();
        (
            matching.len(),
            matching.iter().map(|surface| surface.actions.len()).sum(),
        )
    }

    fn assert_membership_action_set(
        state: &AppBrokerState,
        workspace: &str,
        expected: &[(&str, &str)],
    ) {
        let mut actual = state
            .published_surfaces()
            .iter()
            .filter(|surface| {
                surface.app_id == APP
                    && surface.route.record_id == RECORD
                    && surface.route.cluster_id == workspace
            })
            .flat_map(|surface| &surface.actions)
            .map(|action| (action.resource.clone(), action.action.clone()))
            .collect::<Vec<_>>();
        let mut expected = expected
            .iter()
            .map(|(resource, action)| ((*resource).to_owned(), (*action).to_owned()))
            .collect::<Vec<_>>();
        actual.sort();
        expected.sort();
        assert_eq!(
            actual, expected,
            "discovery exposes the exact action contract"
        );
    }

    #[tokio::test]
    async fn a_workspace_a_host_stewards_offers_its_own_membership() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut tree = crate::test_support::WorkspaceTree::default();
                tree.desktop
                    .workspaces
                    .push(tilo_desktop_model::Workspace::new(
                        tilo_desktop_model::WorkspaceSlot(1),
                        "Hosted elsewhere",
                    ));
                let workspace = tree.desktop.workspaces.first().unwrap().id().to_string();
                let tree = tree.shared();
                let state = AppBrokerState::test_state(
                    Some(tree.authority()),
                    HostServices {
                        workspace_memberships: Some(Arc::new(HostedElsewhereMemberships)),
                        ..Default::default()
                    },
                )
                .0;
                state.refresh_workspace_membership_services().unwrap();
                // The workspace's own governance is offered, so giving and
                // taking back a grant are actions here.
                let expected = vec![
                    (WORKSPACE, "grant-app-authorship"),
                    (WORKSPACE, "grant"),
                    (WORKSPACE, "revoke"),
                    (WORKSPACE, "install-bot"),
                    (WORKSPACE, "renew-bot"),
                    (WORKSPACE, "adopt-bot"),
                    (WORKSPACE, "uninstall-bot"),
                    (WORKSPACE, "pause-bot"),
                    (WORKSPACE, "resume-bot"),
                    (WORKSPACE, "set-bot-appearance"),
                    (WORKSPACE, "request-bot"),
                    (WORKSPACE, "withdraw-bot-request"),
                ];
                assert_membership_action_set(&state, &workspace, &expected);
                {
                    use crate::test_support::{Grants as _, KindSet};
                    tree.write()
                        .desktop
                        .workspaces
                        .first_mut()
                        .unwrap()
                        .allow_agent("scribe", KindSet::READ);
                }
                state.refresh_workspace_membership_services().unwrap();
                // Admission is the record's: the agent it names changes no
                // membership action here.
                assert_membership_action_set(&state, &workspace, &expected);
                drop(state);
            })
            .await;
    }

    /// An app opened in a workspace that appeared since the broker last published
    /// its workspaces' membership actions waits for the broker's loop to publish
    /// them, rather than being refused because it came first.
    #[tokio::test]
    async fn an_open_in_a_new_workspace_is_told_once_its_membership_actions_are_published() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut tree = crate::test_support::WorkspaceTree::default();
                tree.desktop
                    .workspaces
                    .push(tilo_desktop_model::Workspace::new(
                        tilo_desktop_model::WorkspaceSlot(1),
                        "Made just now",
                    ));
                let workspace = tree.desktop.workspaces.first().unwrap().id().to_string();
                let tree = tree.shared();
                let state = AppBrokerState::test_state(
                    Some(tree.authority()),
                    HostServices {
                        workspace_memberships: Some(Arc::new(DiscoveryMemberships)),
                        ..Default::default()
                    },
                )
                .0;
                assert!(!state.membership_published(&workspace));
                let published = state.await_membership_surface(&workspace);
                assert!(
                    published.try_recv().is_err(),
                    "not told before the broker's loop has looked"
                );
                state.refresh_policy_surfaces();
                assert_eq!(published.try_recv(), Ok(true));
                assert_eq!(
                    state.await_membership_surface(&workspace).try_recv(),
                    Ok(true),
                    "a workspace already published is told at once"
                );

                let unknown = uuid::Uuid::new_v4().to_string();
                let refused = state.await_membership_surface(&unknown);
                state.refresh_policy_surfaces();
                assert_eq!(
                    refused.try_recv(),
                    Ok(false),
                    "a workspace the broker publishes nothing for is told so, not left waiting"
                );
                drop(state);
            })
            .await;
    }

    /// An app opened in a workspace this device holds before its replica is
    /// projected waits through the refreshes that find the workspace still
    /// unpublished, and is told once the refresh after the projection
    /// publishes it.
    #[tokio::test]
    async fn an_open_before_its_workspace_is_projected_is_told_once_the_workspace_is_published() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut tree = crate::test_support::WorkspaceTree::default();
                tree.desktop
                    .workspaces
                    .push(tilo_desktop_model::Workspace::new(
                        tilo_desktop_model::WorkspaceSlot(1),
                        "Made just now",
                    ));
                let workspace = tree.desktop.workspaces.first().unwrap().id().to_string();
                let tree = tree.shared();
                let service = Arc::new(ProjectedLaterMemberships {
                    workspace: workspace.clone(),
                    projected: std::sync::atomic::AtomicBool::new(false),
                });
                let state = AppBrokerState::test_state(
                    Some(tree.authority()),
                    HostServices {
                        workspace_memberships: Some(service.clone()),
                        ..Default::default()
                    },
                )
                .0;
                let published = state.await_membership_surface(&workspace);
                state.refresh_policy_surfaces();
                state.refresh_policy_surfaces();
                assert!(!state.membership_published(&workspace));
                assert_eq!(
                    published.try_recv(),
                    Err(std::sync::mpsc::TryRecvError::Empty),
                    "a workspace on its way is waited for, not refused for coming first"
                );
                service
                    .projected
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                state.refresh_policy_surfaces();
                assert_eq!(published.try_recv(), Ok(true));
                drop(state);
            })
            .await;
    }

    #[tokio::test]
    async fn service_surface_lifecycle_invalidates_discovery_only_on_actual_changes() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut tree = crate::test_support::WorkspaceTree::default();
                tree.desktop
                    .workspaces
                    .push(tilo_desktop_model::Workspace::new(
                        tilo_desktop_model::WorkspaceSlot(1),
                        "Discovery",
                    ));
                let workspace = tree.desktop.workspaces.first().unwrap().id().to_string();
                let tree = tree.shared();
                let state = AppBrokerState::test_state(
                    Some(tree.authority()),
                    HostServices {
                        workspace_memberships: Some(Arc::new(DiscoveryMemberships)),
                        ..Default::default()
                    },
                )
                .0;
                assert_eq!(state.published_surfaces_generation(), 0);
                state.refresh_workspace_membership_services().unwrap();
                assert_eq!(state.published_surfaces_generation(), 1);
                assert_eq!(membership_surface_counts(&state, &workspace), (1, 12));
                let expected = vec![
                    (WORKSPACE, "grant-app-authorship"),
                    (WORKSPACE, "grant"),
                    (WORKSPACE, "revoke"),
                    (WORKSPACE, "install-bot"),
                    (WORKSPACE, "renew-bot"),
                    (WORKSPACE, "adopt-bot"),
                    (WORKSPACE, "uninstall-bot"),
                    (WORKSPACE, "pause-bot"),
                    (WORKSPACE, "resume-bot"),
                    (WORKSPACE, "set-bot-appearance"),
                    (WORKSPACE, "request-bot"),
                    (WORKSPACE, "withdraw-bot-request"),
                ];
                assert_membership_action_set(&state, &workspace, &expected);
                state.refresh_workspace_membership_services().unwrap();
                assert_eq!(
                    state.published_surfaces_generation(),
                    1,
                    "unchanged polling is not a source mutation"
                );
                {
                    use crate::test_support::{Grants as _, KindSet};
                    tree.write()
                        .desktop
                        .workspaces
                        .first_mut()
                        .unwrap()
                        .allow_agent("scribe", KindSet::READ);
                }
                state.refresh_workspace_membership_services().unwrap();
                assert_eq!(
                    state.published_surfaces_generation(),
                    1,
                    "an admitted agent adds no membership action"
                );
                assert_eq!(membership_surface_counts(&state, &workspace), (1, 12));
                assert_membership_action_set(&state, &workspace, &expected);
                let peer = *state
                    .workspace_membership_peers
                    .lock()
                    .unwrap()
                    .get(&workspace)
                    .unwrap();
                state.invocation_subscribers.lock().unwrap().remove(&peer);
                for _ in 0..16 {
                    if state.published_surfaces_generation() == 2 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                assert_eq!(
                    state.published_surfaces_generation(),
                    2,
                    "real service retirement invalidates discovery"
                );
                assert_eq!(membership_surface_counts(&state, &workspace), (0, 0));
                assert_membership_action_set(&state, &workspace, &[]);
                drop(state);
            })
            .await;
    }

    fn envelope(
        resource: &str,
        action: &str,
        argument: Option<serde_json::Value>,
    ) -> InvocationEnvelope {
        InvocationEnvelope {
            id: 1,
            invocation: tilo_actions::InvocationId(uuid::Uuid::new_v4()),
            resource: resource.into(),
            action: action.into(),
            argument: argument
                .map(|value| format!("{}{}", tilo_actions::ARGUMENTS_JSON_PREFIX, value)),
            actor: "not-authority".into(),
            context: None,
        }
    }

    #[test]
    fn adding_and_removing_a_directory_bot_are_closed_commands() {
        let request = envelope(
            WORKSPACE,
            "request-bot",
            Some(serde_json::json!({"entry": "so.tilo.bots.standup", "version": "1.2.0"})),
        );
        let command = membership_command(&request).unwrap();
        assert_eq!(
            command,
            WorkspaceMembershipCommand::RequestBot(
                tilo_peer_state::workspace_membership::BotRequest {
                    entry: "so.tilo.bots.standup".into(),
                    version: "1.2.0".into(),
                    inputs: std::collections::BTreeMap::new(),
                    program: None,
                }
            )
        );
        let (action, argument) = bot_invocation(&command).unwrap();
        assert_eq!(action, "request-bot");
        assert_eq!(
            membership_command(&InvocationEnvelope {
                argument: Some(argument),
                ..request
            })
            .unwrap(),
            command,
            "the command travels as the arguments it is read from"
        );
        let withdraw = envelope(
            WORKSPACE,
            "withdraw-bot-request",
            Some(serde_json::json!({"entry": "so.tilo.bots.standup"})),
        );
        assert_eq!(
            membership_command(&withdraw).unwrap(),
            WorkspaceMembershipCommand::WithdrawBotRequest {
                entry: "so.tilo.bots.standup".into()
            }
        );
        for forged in [
            serde_json::json!({"entry": "so.tilo.bots.standup", "version": "1.2.0", "requested_by": "ada"}),
            serde_json::json!({"entry": "../standup", "version": "1.2.0"}),
            serde_json::json!({"entry": "so.tilo.bots.standup"}),
        ] {
            assert!(
                membership_command(&envelope(WORKSPACE, "request-bot", Some(forged.clone())))
                    .is_err(),
                "{forged}"
            );
        }
    }

    #[test]
    fn public_arguments_choose_only_closed_commands() {
        assert!(
            membership_command(&envelope(
                WORKSPACE,
                "add-agent",
                Some(serde_json::json!({"agent": "scribe", "preset": "custom"}))
            ))
            .is_err(),
            "agent admission by preset is retired"
        );
        for field in [
            "caller",
            "certificate",
            "workspace",
            "capability",
            "signing_key",
        ] {
            let mut arguments = serde_json::json!({"app": "so.tilo.chat"});
            arguments[field] = serde_json::json!("forged");
            assert!(
                membership_command(&envelope(
                    WORKSPACE,
                    "grant-app-authorship",
                    Some(arguments)
                ))
                .is_err()
            );
        }
    }

    #[test]
    fn a_bot_install_carries_its_whole_program_and_parses_back() {
        let install = tilo_peer_state::workspace_membership::BotInstall {
            installation: tilo_peer_state::Installation {
                id: tilo_peer_state::InstallationId::parse("6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6")
                    .unwrap(),
                program: "a".repeat(64),
                name: "Discord Bridge".into(),
                icon: None,
                installed_by: "0".repeat(64),
                installed_at_ms: 1,
                appearance: tilo_peer_state::AppearancePolicy::Personas,
                source: None,
                armed_at: vec![],
                connector_cursors_from_ms: 1,
                inputs: std::collections::BTreeMap::default(),
                paused: false,
            },
            program: "x".repeat(8 * 1024),
            device: None,
        };
        let parsed = membership_command(&envelope(
            WORKSPACE,
            "install-bot",
            Some(serde_json::json!({ "install": install })),
        ))
        .unwrap();
        assert_eq!(
            parsed,
            WorkspaceMembershipCommand::InstallBot(Box::new(install.clone()))
        );
        let forged = membership_command(&envelope(
            WORKSPACE,
            "install-bot",
            Some(serde_json::json!({ "install": install, "caller": "forged" })),
        ));
        assert!(forged.is_err());
        assert!(
            membership_command(&envelope(
                WORKSPACE,
                "grant-app-authorship",
                Some(serde_json::json!({ "app": "x".repeat(2048) })),
            ))
            .is_err(),
            "other commands keep their small bound"
        );
        let pause = membership_command(&envelope(
            WORKSPACE,
            "pause-bot",
            Some(serde_json::json!({ "installation": "6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6" })),
        ))
        .unwrap();
        assert!(matches!(pause, WorkspaceMembershipCommand::PauseBot { .. }));
        assert!(
            membership_command(&envelope(
                WORKSPACE,
                "pause-bot",
                Some(serde_json::json!({ "installation": "nope" })),
            ))
            .is_err()
        );
        let appearance = membership_command(&envelope(
            WORKSPACE,
            "set-bot-appearance",
            Some(serde_json::json!({
                "installation": "6f1c2a8e-3b4d-4c5e-9f60-718293a4b5c6",
                "appearance": "personas"
            })),
        ))
        .unwrap();
        assert!(matches!(
            appearance,
            WorkspaceMembershipCommand::SetBotAppearance {
                appearance: tilo_peer_state::AppearancePolicy::Personas,
                ..
            }
        ));
    }

    #[test]
    fn a_change_to_the_workspaces_people_is_the_governance_action_it_names() {
        use tilo_action_policy::Change;
        let governance = governance_resource("8f0ee7bd-30e8-4e2f-9f17-2e57cbcf84b2");
        for change in [
            Change::SetRole {
                person: "a".repeat(64),
                role: tilo_action_policy::Role::Editor,
            },
            Change::Ban {
                person: "b".repeat(64),
            },
            Change::Restrict {
                person: "c".repeat(64),
                until_ms: 1_790_000_000_000,
            },
            Change::Unrestrict {
                person: "c".repeat(64),
            },
        ] {
            let request = envelope(
                &governance,
                change.action(),
                Some(serde_json::to_value(&change).unwrap()),
            );
            assert_eq!(
                membership_command(&request).unwrap(),
                WorkspaceMembershipCommand::Member {
                    change: change.clone()
                },
                "{} travels as its own change",
                change.action()
            );
            assert_eq!(
                encoded(&change).unwrap(),
                request.argument.clone().unwrap(),
                "the change the broker publishes is the argument it reads back"
            );
        }
        let rename = envelope(&governance, "rename", Some(serde_json::json!("Plans")));
        assert!(
            membership_command(&rename).is_err(),
            "a name is not a bare string"
        );
        assert_eq!(
            membership_command(&envelope(
                &governance,
                "rename",
                Some(serde_json::json!({"name": "Plans"})),
            ))
            .unwrap(),
            WorkspaceMembershipCommand::Settings(SettingsChange::Rename {
                name: "Plans".into()
            })
        );
        let ban = serde_json::to_value(Change::Ban {
            person: "b".repeat(64),
        })
        .unwrap();
        assert!(
            membership_command(&envelope(&governance, "unban", Some(ban.clone()))).is_err(),
            "a verb names the change it makes"
        );
        assert!(
            membership_command(&envelope(WORKSPACE, "ban", Some(ban))).is_err(),
            "a change to the people is made on the workspace's governance, not elsewhere"
        );
        assert!(
            membership_command(&envelope(WORKSPACE, "grant", None)).is_err(),
            "the closed membership verbs keep their own arguments"
        );
    }

    #[test]
    fn membership_actions_are_admin_only() {
        let workspace =
            tilo_desktop_model::Workspace::new(tilo_desktop_model::WorkspaceSlot(1), "Membership");
        let actions = membership_actions();
        assert!(actions.iter().all(|action| {
            action.contract.kind == tilo_actions::ActionKind::Outside
                && action.operation_kind == tilo_actions::OperationKind::Effect
        }));
        assert!(actions.iter().any(|action| {
            action.resource == WORKSPACE && action.action == "grant-app-authorship"
        }));
        // The changes to the workspace's people are the governance resource a
        // client names without a catalogue page.
        let governance = governance_resource(&workspace.id().to_string());
        let people = people_actions(&governance);
        assert_eq!(
            people.len(),
            8,
            "every change the people's governance takes"
        );
        assert!(people.iter().all(|action| {
            action.resource == governance
                && action.resource_kind == tilo_actions::remote::WORKSPACE_GOVERNANCE_KIND
                && action.contract.kind == tilo_actions::ActionKind::Outside
        }));
        assert!(people.iter().any(|action| action.action == "ban"));
    }

    /// The changes to a workspace's people are published where every client
    /// names them, and a change reaches the one surface that makes it.
    #[tokio::test]
    async fn the_workspaces_people_are_offered_under_the_provider_clients_name() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut tree = crate::test_support::WorkspaceTree::default();
                tree.desktop
                    .workspaces
                    .push(tilo_desktop_model::Workspace::new(
                        tilo_desktop_model::WorkspaceSlot(1),
                        "Governed",
                    ));
                let workspace = tree.desktop.workspaces.first().unwrap().id().to_string();
                let tree = tree.shared();
                let state = AppBrokerState::test_state(
                    Some(tree.authority()),
                    HostServices {
                        workspace_memberships: Some(Arc::new(DiscoveryMemberships)),
                        ..Default::default()
                    },
                )
                .0;
                state.refresh_workspace_membership_services().unwrap();
                let resource = governance_resource(&workspace);
                let (surface, offered) = state
                    .governance_surface(&workspace, &resource, "set-role")
                    .expect("the people's changes are published");
                assert_eq!(
                    surface.app_id,
                    tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER
                );
                assert_eq!(offered.resource_kind, "workspace.permissions");
                assert_eq!(surface.actions.len(), 8);
                assert!(
                    state
                        .governance_surface(&workspace, &resource, "call")
                        .is_none(),
                    "a verb the workspace's people do not offer names nothing here"
                );
                assert!(
                    state
                        .governance_surface(
                            "another-workspace",
                            &governance_resource("another-workspace"),
                            "set-role"
                        )
                        .is_none(),
                    "a workspace this device does not publish is not answered for"
                );
                drop(state);
            })
            .await;
    }

    /// A governance request that names no published change starts nothing: the
    /// surface it would run on is the one that offers it.
    #[tokio::test]
    async fn a_governance_request_naming_no_published_change_starts_nothing() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut tree = crate::test_support::WorkspaceTree::default();
                tree.desktop
                    .workspaces
                    .push(tilo_desktop_model::Workspace::new(
                        tilo_desktop_model::WorkspaceSlot(1),
                        "Governed",
                    ));
                let workspace = tree.desktop.workspaces.first().unwrap().id().to_string();
                let tree = tree.shared();
                let state = AppBrokerState::test_state(
                    Some(tree.authority()),
                    HostServices {
                        workspace_memberships: Some(Arc::new(DiscoveryMemberships)),
                        ..Default::default()
                    },
                )
                .0;
                state.refresh_workspace_membership_services().unwrap();
                let caller =
                    tilo_actions::remote::Caller::local_person("someone").in_workspace(&workspace);
                let request = |resource: &str, action: &str| tilo_actions::remote::InvokeRequest {
                    invocation: tilo_actions::InvocationId::new(),
                    target: tilo_actions::remote::RemoteActionTarget {
                        provider: tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER.into(),
                        address: tilo_actions::ActionAddress::new(resource, action),
                    },
                    argument: tilo_actions::remote::InvokeArgument::None,
                    confirmation: None,
                    context: None,
                    revision: None,
                    wait_ms: None,
                    appearance: None,
                    on_behalf_of: None,
                };
                for (resource, action) in [
                    (governance_resource(&workspace), "call"),
                    ("desktop:workspace".to_owned(), "grant"),
                ] {
                    let request = request(&resource, action);
                    assert!(
                        matches!(
                            state.start_governance_action(&caller, None, &request, &request, None),
                            Err(tilo_actions::remote::ActionHostError::UnknownAction)
                        ),
                        "{action} on {resource} is not one of the people's changes"
                    );
                }
                drop(state);
            })
            .await;
    }
}
