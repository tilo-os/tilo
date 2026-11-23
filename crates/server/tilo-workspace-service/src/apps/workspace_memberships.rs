//! Governed agent membership through the ordinary durable invocation surface.
//! Public arguments select only a closed command; caller ownership and signing
//! authority remain host-owned.
use super::published_service::{ServiceOutcome, refused};
use super::{
    AppBrokerState, Arc, CrdtRoute, InvocationEnvelope, PublishedAction, PublishedInput,
    PublishedSurface,
};
use tilo_peer_state::workspace_membership::{SettingsChange, WorkspaceMembershipCommand};

const APP: &str = "so.tilo.desktop";
const RECORD: &str = "membership";
const WORKSPACE: &str = "desktop:workspace";

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
            WorkspaceMembershipCommand::Grant { grant } => {
                let argument = encoded(&serde_json::json!({ "grant": grant }))?;
                (WORKSPACE.into(), "grant", Some(argument))
            }
            WorkspaceMembershipCommand::Revoke { grant } => {
                let argument = encoded(&serde_json::json!({ "grant": grant }))?;
                (WORKSPACE.into(), "revoke", Some(argument))
            }
            WorkspaceMembershipCommand::Member { change } => (
                governance_resource(workspace),
                change.action(),
                Some(encoded(change)?),
            ),
            // The host's own `member.add`, which it signs as the workspace's
            // administrator: a member's own device signs this command itself.
            WorkspaceMembershipCommand::Place {
                person,
                place,
                name,
            } => (
                governance_resource(workspace),
                crate::workspaces::ADD_MEMBER_ACTION,
                Some(encoded(&serde_json::json!({
                    "person": hex::encode(person),
                    "role": place.key(),
                    "name": name,
                }))?),
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
            WorkspaceMembershipCommand::Lease { .. }
            | WorkspaceMembershipCommand::AnswerSchedule { .. }
            | WorkspaceMembershipCommand::KeepDone { .. } => {
                return Err("a lease is claimed by the device that does the work".into());
            }
            WorkspaceMembershipCommand::InstallApp(_)
            | WorkspaceMembershipCommand::UninstallApp { .. } => {
                return Err(
                    "a workspace's app version is chosen on its host, through the workspace's `install-app`"
                        .into(),
                );
            }
            WorkspaceMembershipCommand::Call { .. } => {
                return Err("a call is moderated through the workspace's `call` action".into());
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
            .publishers
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
    ) -> Result<super::action_provider::Started, tilo_actions::remote::ActionCallError> {
        use tilo_actions::remote::ActionCallError;
        let address = &request.target.address;
        let Some(workspace) = address
            .resource
            .strip_prefix(&format!(
                "{}:",
                tilo_actions::remote::WORKSPACE_GOVERNANCE_KIND
            ))
            .filter(|workspace| !workspace.is_empty())
        else {
            return Err(ActionCallError::UnknownAction);
        };
        let Some((surface, action)) =
            self.governance_surface(workspace, &address.resource, &address.action)
        else {
            return Err(ActionCallError::UnknownAction);
        };
        let Some((committed, actor, now)) =
            self.decide_for(caller, workspace, |committed, actor, now| {
                (committed.clone(), actor.clone(), now)
            })
        else {
            return Err(ActionCallError::UnknownAction);
        };
        // A workspace's people change at its own host person's device, never
        // by an agent acting for them.
        if actor.direct_person().is_none() {
            return Err(ActionCallError::NotPermitted);
        }
        let need = action.contract.need();
        let node = tilo_action_policy::roles::governance_action(workspace, &address.action);
        if !actor
            .answer_in(&committed, &node, need.kind, now)
            .admits(actor.direct())
        {
            return Err(ActionCallError::NotPermitted);
        }
        let token = self.confirmation_token(
            surface.peer_id,
            &surface.app_id,
            &action,
            argument,
            &actor,
            request.confirmation.as_ref(),
            &self.approval_binding(actor.member(), workspace, original_request),
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
        .map(super::action_provider::Started::Staged)
        .map_err(super::action_provider::refusal)
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
            let surfaces = self.publishers.surfaces.lock().ok()?;
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
        self.workspace_governance_peers
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
    /// ([`crate::apps::services::WorkspaceMemberships::membership_pending`]). The
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
        tilo_identity::policy_change::announce();
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
                .map(|workspace| workspace.id().to_string())
                .collect::<Vec<_>>()
        })?;
        for workspace in workspaces {
            // A workspace its host stewards offers its membership changes here
            // too: this device writes them itself, as in any workspace it holds.
            if !service.requires_workspace(&workspace) {
                continue;
            }
            let governance = governance_actions(&governance_resource(&workspace));
            if self.publish_workspace_surface(&workspace, governance, service.clone())? {
                self.publish_changed_action_catalogue();
            }
        }
        Ok(())
    }

    /// Publish `actions` as the workspace's governance and say the action
    /// catalogue changed, or say the same actions are published already: a
    /// workspace keeps one such surface, replaced in place as what it offers
    /// changes.
    fn publish_workspace_surface(
        self: &Arc<Self>,
        workspace: &str,
        actions: Vec<PublishedAction>,
        service: Arc<dyn crate::apps::services::WorkspaceMemberships>,
    ) -> Result<bool, String> {
        let existing = self
            .workspace_governance_peers
            .lock()
            .map_err(|_| "workspace surface registry unavailable")?
            .get(workspace)
            .copied();
        if let Some(peer) = existing {
            let changed = {
                let mut surfaces = self
                    .publishers
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
        self.start_workspace_surface(workspace.to_owned(), actions, service)
    }

    fn start_workspace_surface(
        self: &Arc<Self>,
        workspace: String,
        actions: Vec<PublishedAction>,
        service: Arc<dyn crate::apps::services::WorkspaceMemberships>,
    ) -> Result<bool, String> {
        let peer = self.next_service_peer_id();
        let surface = PublishedSurface {
            peer_id: peer,
            app_id: tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER.into(),
            route: CrdtRoute::record(APP, &workspace, RECORD),
            live: true,
            actions,
        };
        self.workspace_governance_peers
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
                if let Ok(mut peers) = state.workspace_governance_peers.lock() {
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
        service: &dyn crate::apps::services::WorkspaceMemberships,
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
            || !service.requires_workspace(workspace)
        {
            return Err("membership invocation binding changed".into());
        }
        let original = pending
            .original
            .as_ref()
            .ok_or("membership original ownership unavailable")?;
        let caller = original.owner_person_certificate(self, workspace, envelope)?;
        let command = membership_command(envelope)?;
        service.execute(workspace.into(), caller, command).await?;
        Ok(serde_json::json!({"generation": 0}))
    }
}

fn membership_command(envelope: &InvocationEnvelope) -> Result<WorkspaceMembershipCommand, String> {
    if let Some(command) = people_command(envelope)? {
        return Ok(command);
    }
    Err("unknown workspace membership command".into())
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

fn bounded_argument<'a>(envelope: &'a InvocationEnvelope, name: &str) -> Result<&'a str, String> {
    let encoded = envelope
        .argument
        .as_deref()
        .ok_or_else(|| format!("{name} arguments missing"))?;
    if encoded.len() > MAX_ARGUMENT_BYTES {
        return Err(format!("{name} arguments exceed bounded size"));
    }
    Ok(encoded
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .unwrap_or(encoded))
}

/// Most bytes a membership command's arguments may take: a few ids.
const MAX_ARGUMENT_BYTES: usize = 1024;

/// The changes to a workspace's governance, as it offers them: its people, its own
/// name, and the permission of one app to use another's action — made here at this
/// device and admitted by every device.
fn governance_actions(resource: &str) -> Vec<PublishedAction> {
    people_actions(resource)
}

/// The changes to a workspace's people, as the workspace's governance offers them:
/// the verbs its clients already name (`tilo_action_policy::roles::governance`),
/// and its own name.
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

fn membership_action(
    resource: &str,
    action: &str,
    label: &str,
    action_label: &str,
    params: Vec<tilo_actions::ActionParam>,
) -> PublishedAction {
    PublishedAction {
        did: None,
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
    use crate::apps::services::{ProcessServices, WorkspaceMemberships};

    struct DiscoveryMemberships;

    impl WorkspaceMemberships for DiscoveryMemberships {
        fn requires_workspace(&self, _: &str) -> bool {
            true
        }

        fn health(&self) -> Result<(), String> {
            Ok(())
        }

        fn execute(
            &self,
            _: String,
            _: tilo_user_identity::DeviceCert,
            _: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
        ) -> crate::apps::services::WorkspaceMembershipFuture {
            Box::pin(async { Err("discovery fixture must not mutate the owner".into()) })
        }
    }

    /// A service in place of a workspace this device has just made: held,
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

        fn execute(
            &self,
            _: String,
            _: tilo_user_identity::DeviceCert,
            _: tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
        ) -> crate::apps::services::WorkspaceMembershipFuture {
            Box::pin(async { Err("discovery fixture must not mutate the owner".into()) })
        }
    }

    fn membership_surface_counts(state: &AppBrokerState, workspace: &str) -> (usize, usize) {
        let surfaces = state.published_surfaces();
        let matching = surfaces
            .iter()
            .filter(|surface| {
                surface.app_id == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER
                    && surface.route.document_id == RECORD
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
                surface.app_id == tilo_actions::remote::WORKSPACE_GOVERNANCE_PROVIDER
                    && surface.route.document_id == RECORD
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

    /// An app opened in a workspace that appeared since the broker last published
    /// its workspaces' membership actions waits for the broker's loop to publish
    /// them, rather than being refused because it came first.
    #[tokio::test]
    async fn an_open_in_a_new_workspace_is_told_once_its_membership_actions_are_published() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut tree = crate::apps::test_support::WorkspaceTree::default();
                tree.desktop
                    .workspaces
                    .push(tilo_desktop_model::Workspace::new(
                        tilo_desktop_model::WorkspaceId::from_u128(1),
                        "Made just now",
                    ));
                let workspace = tree.desktop.workspaces.first().unwrap().id().to_string();
                let tree = tree.shared();
                let state = AppBrokerState::test_state(
                    Some(tree.authority()),
                    ProcessServices {
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
                let mut tree = crate::apps::test_support::WorkspaceTree::default();
                tree.desktop
                    .workspaces
                    .push(tilo_desktop_model::Workspace::new(
                        tilo_desktop_model::WorkspaceId::from_u128(1),
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
                    ProcessServices {
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
                let mut tree = crate::apps::test_support::WorkspaceTree::default();
                tree.desktop
                    .workspaces
                    .push(tilo_desktop_model::Workspace::new(
                        tilo_desktop_model::WorkspaceId::from_u128(1),
                        "Discovery",
                    ));
                let workspace = tree.desktop.workspaces.first().unwrap().id().to_string();
                let tree = tree.shared();
                let state = AppBrokerState::test_state(
                    Some(tree.authority()),
                    ProcessServices {
                        workspace_memberships: Some(Arc::new(DiscoveryMemberships)),
                        ..Default::default()
                    },
                )
                .0;
                assert_eq!(state.published_surfaces_generation(), 0);
                state.refresh_workspace_membership_services().unwrap();
                assert_eq!(state.published_surfaces_generation(), 1);
                assert_eq!(membership_surface_counts(&state, &workspace), (1, 8));
                let people = governance_resource(&workspace);
                let expected = [
                    "add-member",
                    "set-role",
                    "remove-member",
                    "ban",
                    "unban",
                    "restrict",
                    "unrestrict",
                    "rename",
                ]
                .map(|action| (people.as_str(), action));
                assert_membership_action_set(&state, &workspace, &expected);
                state.refresh_workspace_membership_services().unwrap();
                assert_eq!(
                    state.published_surfaces_generation(),
                    1,
                    "unchanged polling is not a source mutation"
                );
                {
                    use crate::apps::test_support::{Grants as _, KindSet};
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
                assert_eq!(membership_surface_counts(&state, &workspace), (1, 8));
                assert_membership_action_set(&state, &workspace, &expected);
                let peer = *state
                    .workspace_governance_peers
                    .lock()
                    .unwrap()
                    .get(&workspace)
                    .unwrap();
                state.publishers.streams.lock().unwrap().remove(&peer);
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
            actor: tilo_actions::MemberId::person("not-authority").into(),
            context: None,
        }
    }

    tilo_test_env::scenario! {
        fn membership_commands_are_closed_and_admin_only;

        phase public_arguments_choose_only_closed_commands;
        {
            let governance = governance_resource("8f0ee7bd-30e8-4e2f-9f17-2e57cbcf84b2");
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
                let mut arguments = serde_json::json!({"name": "Studio"});
                arguments[field] = serde_json::json!("forged");
                assert!(membership_command(&envelope(&governance, "rename", Some(arguments))).is_err());
            }
        }

        phase a_change_to_the_workspaces_people_is_the_governance_action_it_names;
        {
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
                "a grant is the governance's, not the shell's own"
            );
        }

        phase membership_actions_are_outside_effects;
        {
            let workspace =
                tilo_desktop_model::Workspace::new(tilo_desktop_model::WorkspaceId::from_u128(1), "Membership");
            // The changes to the workspace's governance are the governance resource a
            // client names without a catalogue page.
            let governance = governance_resource(&workspace.id().to_string());
            let changes = governance_actions(&governance);
            assert_eq!(
                changes.len(),
                8,
                "every change the workspace's governance takes"
            );
            assert!(changes.iter().all(|action| {
                action.resource == governance
                    && action.resource_kind == tilo_actions::remote::WORKSPACE_GOVERNANCE_KIND
                    && action.contract.kind == tilo_actions::ActionKind::Outside
            }));
            assert!(changes.iter().any(|action| action.action == "ban"));
            assert!(
                changes.iter().all(|offered| offered
                    .params
                    .iter()
                    .all(|param| param.name != "target_app")),
                "an app's use of another app's action is the person's setting, not the workspace's"
            );
        }
    }

    /// The changes to a workspace's people are published where every client
    /// names them, and a change reaches the one surface that makes it.
    #[tokio::test]
    async fn the_workspaces_people_are_offered_under_the_provider_clients_name() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut tree = crate::apps::test_support::WorkspaceTree::default();
                tree.desktop
                    .workspaces
                    .push(tilo_desktop_model::Workspace::new(
                        tilo_desktop_model::WorkspaceId::from_u128(1),
                        "Governed",
                    ));
                let workspace = tree.desktop.workspaces.first().unwrap().id().to_string();
                let tree = tree.shared();
                let state = AppBrokerState::test_state(
                    Some(tree.authority()),
                    ProcessServices {
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
                let mut tree = crate::apps::test_support::WorkspaceTree::default();
                tree.desktop
                    .workspaces
                    .push(tilo_desktop_model::Workspace::new(
                        tilo_desktop_model::WorkspaceId::from_u128(1),
                        "Governed",
                    ));
                let workspace = tree.desktop.workspaces.first().unwrap().id().to_string();
                let tree = tree.shared();
                let state = AppBrokerState::test_state(
                    Some(tree.authority()),
                    ProcessServices {
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
                    signed: None,
                };
                for (resource, action) in [
                    (governance_resource(&workspace), "call"),
                    ("desktop:workspace".to_owned(), "grant"),
                ] {
                    let request = request(&resource, action);
                    assert!(
                        matches!(
                            state.start_governance_action(&caller, None, &request, &request, None),
                            Err(tilo_actions::remote::ActionCallError::UnknownAction)
                        ),
                        "{action} on {resource} is not one of the people's changes"
                    );
                }
                drop(state);
            })
            .await;
    }
}
