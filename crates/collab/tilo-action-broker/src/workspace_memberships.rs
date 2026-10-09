//! Governed agent membership through the ordinary durable invocation surface.
//! Public arguments select only a closed command; caller custody and signing
//! authority remain host-owned.
use super::{
    Arc, CapnpBrokerState, CrdtRoute, InvocationEnvelope, PublishedAction, PublishedInput,
    PublishedSurface,
};
use tilo_peer_state::{
    owner_commit::{CommitOperation, OwnerCommitRequest},
    workspace_membership::{AgentMembershipPreset, WorkspaceMembershipCommand},
};

const APP: &str = "so.tilo.desktop";
const RECORD: &str = "membership";
const WORKSPACE: &str = "desktop:workspace";
const AGENT_PREFIX: &str = "desktop:agent:";

impl CapnpBrokerState {
    pub(crate) fn invoke_workspace_membership(
        &self,
        room: &str,
        command: &WorkspaceMembershipCommand,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<super::InvocationJob, String> {
        command.validate()?;
        let (resource, action, argument) = match command {
            WorkspaceMembershipCommand::AddAgent { agent, preset } => {
                let argument = format!(
                    "{}{}",
                    tilo_actions::ARGUMENTS_JSON_PREFIX,
                    serde_json::json!({"agent": agent, "preset": preset.id()})
                );
                (WORKSPACE.into(), "add-agent", Some(argument))
            }
            WorkspaceMembershipCommand::RemoveAgent { agent } => {
                (format!("{AGENT_PREFIX}{agent}"), "remove", None)
            }
            WorkspaceMembershipCommand::GrantAppAuthorship { app } => {
                let argument = format!(
                    "{}{}",
                    tilo_actions::ARGUMENTS_JSON_PREFIX,
                    serde_json::json!({"app": app})
                );
                (WORKSPACE.into(), "grant-app-authorship", Some(argument))
            }
        };
        self.invoke_workspace_person_action(
            room,
            &resource,
            action,
            argument.as_deref(),
            certificate,
        )
    }

    fn invoke_workspace_person_action(
        &self,
        room: &str,
        resource: &str,
        action: &str,
        argument: Option<&str>,
        certificate: &tilo_user_identity::DeviceCert,
    ) -> Result<super::InvocationJob, String> {
        let route = CrdtRoute::record(APP, room, RECORD);
        let peers = self
            .surfaces
            .lock()
            .map_err(|_| "membership surfaces unavailable")?
            .values()
            .filter(|surface| {
                surface.live
                    && surface.app_id == APP
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
        let room_id = uuid::Uuid::parse_str(room).map_err(|_| "invalid exact room")?;
        authority.authorize_join(room_id, certificate)?;
        let now = self.policy_now_ns()?;
        tilo_user_identity::verify_device_cert_at(certificate, now)
            .map_err(|_| "membership device certificate rejected")?;
        let membership = self
            .membership
            .as_ref()
            .ok_or("workspace authority unavailable")?;
        let mut admitted = super::admit_verified_join_at(
            &membership.snapshot()?,
            &route,
            super::VerifiedJoinIdentity::of(
                certificate,
                super::cert_capped_in(Some(authority.as_ref()), room, certificate),
            ),
            now / 1_000_000_000,
        )?;
        admitted.certificate = Some(certificate.clone());
        let actor = admitted.broker_actor();
        self.start_invocation_confirmed_for_origin(
            *peer,
            resource,
            action,
            super::InvocationInputs {
                requested: None,
                caller: None,
                execution: None,
                managed_interaction: None,
                argument,
                actor: &actor,
                confirmation_token: None,
                context: None,
            },
            Some(super::InvocationCaller::Local {
                route,
                admitted: Box::new(admitted),
            }),
        )
    }

    pub(super) fn refresh_workspace_membership_services(self: &Arc<Self>) -> Result<(), String> {
        let Some(service) = self.services.workspace_memberships.clone() else {
            return Ok(());
        };
        let Some(authority) = &self.membership else {
            return Ok(());
        };
        let now = self.policy_now_ns()?;
        for workspace in authority.snapshot()?.workspaces {
            let room = workspace.room.to_string();
            if !service.requires_room(&room) {
                continue;
            }
            let mut actions = membership_actions(&workspace, now / 1_000_000_000);
            actions.extend(super::workspace_tasks::actions(
                &service.task_snapshot(&room)?,
                &workspace.name,
            ));
            let existing = self
                .workspace_membership_peers
                .lock()
                .map_err(|_| "membership service registry unavailable")?
                .get(&room)
                .copied();
            if let Some(peer) = existing {
                let changed = {
                    let mut surfaces = self
                        .surfaces
                        .lock()
                        .map_err(|_| "membership surfaces unavailable")?;
                    surfaces.get_mut(&peer).is_some_and(|surface| {
                        if surface.actions == actions {
                            return false;
                        }
                        surface.actions = actions;
                        true
                    })
                };
                if changed {
                    self.publish_changed_action_catalogue();
                }
                continue;
            }
            self.start_workspace_membership_service(room, actions, service.clone())?;
        }
        Ok(())
    }

    fn start_workspace_membership_service(
        self: &Arc<Self>,
        room: String,
        actions: Vec<PublishedAction>,
        service: Arc<dyn crate::services::WorkspaceMemberships>,
    ) -> Result<(), String> {
        let peer = self.next_service_peer_id();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        self.invocation_subscribers
            .lock()
            .map_err(|_| "invocation subscribers unavailable")?
            .insert(peer, sender);
        self.stable_invocation_subscribers
            .lock()
            .map_err(|_| "stable invocation registry unavailable")?
            .insert(peer);
        self.surfaces
            .lock()
            .map_err(|_| "membership surfaces unavailable")?
            .insert(
                peer,
                PublishedSurface {
                    peer_id: peer,
                    app_id: APP.into(),
                    route: CrdtRoute::record(APP, &room, RECORD),
                    live: true,
                    actions,
                },
            );
        self.workspace_membership_peers
            .lock()
            .map_err(|_| "membership service registry unavailable")?
            .insert(room.clone(), peer);
        self.publish_changed_action_catalogue();
        let state = self.clone();
        tokio::task::spawn_local(async move {
            while let Some(envelope) = receiver.recv().await {
                let result = state
                    .execute_workspace_membership(peer, &room, &envelope, service.as_ref())
                    .await;
                let update = match result {
                    Ok(value) => tilo_actions::InvocationUpdate::Succeeded {
                        invocation: envelope.invocation,
                        output: tilo_actions::ActionOutput {
                            value: Some(value),
                            resources: Vec::new(),
                        },
                    },
                    Err(error) => tilo_actions::InvocationUpdate::Failed {
                        invocation: envelope.invocation,
                        error: tilo_actions::ActionFailure {
                            code: "workspace_membership_refused".into(),
                            message: error,
                            retryable: false,
                            details: None,
                        },
                    },
                };
                if let Err(error) = state
                    .apply_invocation_update(peer, envelope.id, update, None)
                    .await
                {
                    if let Ok(mut failure) = state.owner_commit_failure.lock() {
                        failure.get_or_insert(error.to_string());
                    }
                    break;
                }
            }
            let removed = state
                .surfaces
                .lock()
                .is_ok_and(|mut surfaces| surfaces.remove(&peer).is_some());
            if removed {
                state.publish_changed_action_catalogue();
            }
            if let Ok(mut peers) = state.workspace_membership_peers.lock() {
                peers.remove(&room);
            }
        });
        Ok(())
    }

    async fn execute_workspace_membership(
        &self,
        peer: u64,
        room: &str,
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
        if let Some(agent) = agent_availability_target(envelope)? {
            let original = pending
                .original
                .as_ref()
                .ok_or("agent availability custody unavailable")?;
            revalidate_inspection(self, original, room, envelope)?;
            if !service.requires_room(room) {
                return Err("agent availability room changed".into());
            }
            service.health()?;
            if !self.agent_is_active_member(room, &agent)? {
                return Err("agent is no longer a current workspace member".into());
            }
            let availability = match self.agent_task_availability(room, &agent) {
                Ok(true) => "available",
                Ok(false) => "unavailable",
                Err(_) => "unknown",
            };
            revalidate_inspection(self, original, room, envelope)?;
            if !self.agent_is_active_member(room, &agent)? {
                return Err("agent is no longer a current workspace member".into());
            }
            return Ok(serde_json::json!({"availability": availability}));
        }
        if super::workspace_tasks::is_inspect(envelope) {
            let original = pending
                .original
                .as_ref()
                .ok_or("task inspection custody unavailable")?;
            original.owner_person_certificate(self, room, envelope)?;
            if !service.requires_room(room) {
                return Err("task inspection room changed".into());
            }
            service.health()?;
            return super::workspace_tasks::inspect(&service.task_snapshot(room)?, envelope);
        }
        let key = pending
            .journal_key
            .as_ref()
            .ok_or("membership invocation has no durable admission")?;
        if key.room != room
            || key.invocation != envelope.invocation
            || key.owner != pending.owner
            || !service.requires_room(room)
        {
            return Err("membership invocation binding changed".into());
        }
        let original = pending
            .original
            .as_ref()
            .ok_or("membership original custody unavailable")?;
        let caller = original.owner_person_certificate(self, room, envelope)?;
        if let Some(command) = super::workspace_tasks::command(envelope)? {
            let request = OwnerCommitRequest {
                room: room.into(),
                operation: CommitOperation::WorkspaceTaskCommand {
                    invocation: envelope.invocation.0,
                    authority: tilo_peer_state::workspace_task::WorkspaceTaskAuthority::Person {
                        caller: caller.clone(),
                    },
                    command,
                },
            };
            let receipt = service.execute(request.clone()).await?;
            receipt.validate_for(&request)?;
            let snapshot = service.task_snapshot(room)?;
            return super::workspace_tasks::result(
                &snapshot,
                envelope,
                caller.device_public_key,
                receipt.generation,
            );
        }
        let command = membership_command(envelope)?;
        let request = OwnerCommitRequest {
            room: room.into(),
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

fn revalidate_inspection(
    state: &CapnpBrokerState,
    original: &super::OriginalInvocationData,
    room: &str,
    envelope: &InvocationEnvelope,
) -> Result<(), String> {
    original.revalidate_participant_inspection(state, room, envelope)
}

fn agent_availability_target(envelope: &InvocationEnvelope) -> Result<Option<String>, String> {
    let Some(agent) = envelope.resource.strip_prefix(AGENT_PREFIX) else {
        return Ok(None);
    };
    if envelope.action != "read-availability" {
        return Ok(None);
    }
    if envelope.argument.is_some() {
        return Err("agent availability takes no arguments".into());
    }
    WorkspaceMembershipCommand::RemoveAgent {
        agent: agent.into(),
    }
    .validate()?;
    Ok(Some(agent.into()))
}

fn membership_command(envelope: &InvocationEnvelope) -> Result<WorkspaceMembershipCommand, String> {
    let command = if envelope.resource == WORKSPACE && envelope.action == "add-agent" {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct AddAgent {
            agent: String,
            preset: String,
        }
        let encoded = bounded_argument(envelope, "agent membership")?;
        let input: AddAgent = serde_json::from_str(encoded).map_err(|error| error.to_string())?;
        WorkspaceMembershipCommand::AddAgent {
            agent: input.agent.trim().to_lowercase(),
            preset: AgentMembershipPreset::parse(&input.preset)
                .ok_or("unknown agent membership preset")?,
        }
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
    } else if let Some(agent) = envelope.resource.strip_prefix(AGENT_PREFIX)
        && envelope.action == "remove"
        && envelope.argument.is_none()
    {
        WorkspaceMembershipCommand::RemoveAgent {
            agent: agent.into(),
        }
    } else {
        return Err("unknown workspace membership command".into());
    };
    command.validate()?;
    Ok(command)
}

fn bounded_argument<'a>(envelope: &'a InvocationEnvelope, name: &str) -> Result<&'a str, String> {
    let encoded = envelope
        .argument
        .as_deref()
        .ok_or_else(|| format!("{name} arguments missing"))?;
    if encoded.len() > 1024 {
        return Err(format!("{name} arguments exceed bounded size"));
    }
    Ok(encoded
        .strip_prefix(tilo_actions::ARGUMENTS_JSON_PREFIX)
        .unwrap_or(encoded))
}

fn membership_actions(workspace: &tilo_desktop_model::Workspace, now: u64) -> Vec<PublishedAction> {
    let room = workspace.room.to_string();
    let mut actions = vec![membership_action(
        WORKSPACE,
        "add-agent",
        "Workspace",
        "Add agent",
        vec![
            tilo_actions::ActionParam::required(
                "agent",
                tilo_actions::ParamType::Text,
                "Stable agent id",
            ),
            tilo_actions::ActionParam::required(
                "preset",
                tilo_actions::ParamType::Text,
                "observer, contributor, operator, or custom",
            ),
        ],
    )];
    actions.push(membership_action(
        WORKSPACE,
        "grant-app-authorship",
        "Workspace",
        "Allow app authorship",
        vec![tilo_actions::ActionParam::required(
            "app",
            tilo_actions::ParamType::Text,
            "Application id",
        )],
    ));
    actions.extend(
        workspace
            .agent_members_in_room_at(&room, now)
            .into_iter()
            .flat_map(|(agent, _)| {
                [
                    agent_availability_action(&agent),
                    membership_action(
                        &format!("{AGENT_PREFIX}{agent}"),
                        "remove",
                        &agent,
                        "Remove agent",
                        Vec::new(),
                    ),
                ]
            }),
    );
    actions
}

fn agent_availability_action(agent: &str) -> PublishedAction {
    PublishedAction {
        semantic_identity: None,
        resource: format!("{AGENT_PREFIX}{agent}"),
        action: "read-availability".into(),
        resource_kind: "workspace-membership".into(),
        resource_label: agent.into(),
        action_label: "Read agent availability".into(),
        description: Some("Read whether this workspace Agent can currently receive work".into()),
        operation_kind: tilo_actions::OperationKind::View,
        params: Vec::new(),
        contract: tilo_actions::ActionContract {
            permission: tilo_actions::PermissionRequirement::Read,
            result: tilo_actions::ResultSpec::Json {
                schema: serde_json::json!({
                    "type": "object",
                    "additionalProperties": false,
                    "required": ["availability"],
                    "properties": {
                        "availability": {
                            "type": "string",
                            "enum": ["available", "unavailable", "unknown"]
                        }
                    }
                }),
            },
            ..tilo_actions::ActionContract::view()
        },
        view: None,
        input: PublishedInput::None,
    }
}

fn membership_action(
    resource: &str,
    action: &str,
    label: &str,
    action_label: &str,
    params: Vec<tilo_actions::ActionParam>,
) -> PublishedAction {
    PublishedAction {
        semantic_identity: None,
        resource: resource.into(),
        action: action.into(),
        resource_kind: "workspace-membership".into(),
        resource_label: label.into(),
        action_label: action_label.into(),
        description: None,
        operation_kind: tilo_actions::OperationKind::Effect,
        params,
        contract: tilo_actions::ActionContract {
            permission: tilo_actions::PermissionRequirement::Admin,
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

    struct PrivateAvailabilityMemberships;

    impl WorkspaceMemberships for DiscoveryMemberships {
        fn requires_room(&self, _: &str) -> bool {
            true
        }

        fn health(&self) -> Result<(), String> {
            Ok(())
        }

        fn task_snapshot(
            &self,
            _: &str,
        ) -> Result<tilo_peer_state::workspace_task::WorkspaceTaskSnapshot, String> {
            Ok(tilo_peer_state::workspace_task::WorkspaceTaskSnapshot {
                tasks: Vec::new(),
                controls: std::collections::BTreeMap::default(),
            })
        }

        fn execute(&self, _: OwnerCommitRequest) -> tilo_peer_state::owner_commit::CommitFuture {
            Box::pin(async { Err("discovery fixture must not mutate the owner".into()) })
        }
    }

    impl WorkspaceMemberships for PrivateAvailabilityMemberships {
        fn requires_room(&self, _: &str) -> bool {
            true
        }

        fn health(&self) -> Result<(), String> {
            Ok(())
        }

        fn task_snapshot(
            &self,
            _room: &str,
        ) -> Result<tilo_peer_state::workspace_task::WorkspaceTaskSnapshot, String> {
            let task = tilo_desktop_model::WorkspaceTask {
                id: "task:00000000-0000-4000-8000-000000000083".into(),
                title: "private task title".into(),
                created_by: "private creator".into(),
                assigned_agent: Some("scribe".into()),
                route: tilo_desktop_model::WorkspaceTaskRoute::Model,
                status: tilo_desktop_model::WorkspaceTaskStatus::InProgress,
                resource: None,
                created_at: 1,
                updated_at: 2,
                audit: vec![tilo_desktop_model::WorkspaceTaskAuditEvent {
                    at: 2,
                    actor: "private actor".into(),
                    summary: "private audit summary".into(),
                }],
            };
            Ok(tilo_peer_state::workspace_task::WorkspaceTaskSnapshot {
                tasks: vec![task],
                controls: std::collections::BTreeMap::from([(
                    "task:00000000-0000-4000-8000-000000000083".into(),
                    tilo_desktop_model::WorkspaceTaskControl {
                        instruction: Some("private full instruction".into()),
                        ..Default::default()
                    },
                )]),
            })
        }

        fn execute(&self, _: OwnerCommitRequest) -> tilo_peer_state::owner_commit::CommitFuture {
            Box::pin(async { Err("availability fixture must not mutate the owner".into()) })
        }
    }

    fn membership_surface_counts(state: &CapnpBrokerState, room: &str) -> (usize, usize) {
        let surfaces = state.published_surfaces();
        let matching = surfaces
            .iter()
            .filter(|surface| {
                surface.app_id == APP
                    && surface.route.record_id == RECORD
                    && surface.route.cluster_id == room
            })
            .collect::<Vec<_>>();
        (
            matching.len(),
            matching.iter().map(|surface| surface.actions.len()).sum(),
        )
    }

    fn assert_membership_action_set(
        state: &CapnpBrokerState,
        room: &str,
        expected: &[(&str, &str)],
    ) {
        let mut actual = state
            .published_surfaces()
            .iter()
            .filter(|surface| {
                surface.app_id == APP
                    && surface.route.record_id == RECORD
                    && surface.route.cluster_id == room
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
    async fn service_surface_lifecycle_invalidates_discovery_only_on_actual_changes() {
        tokio::task::LocalSet::new()
            .run_until(async {
                let mut tree = crate::test_support::WorkspaceTree::default();
                tree.desktop
                    .workspaces
                    .push(tilo_desktop_model::Workspace::new(
                        tilo_desktop_model::WorkspaceId(1),
                        "Discovery",
                    ));
                let room = tree.desktop.workspaces.first().unwrap().room.to_string();
                let tree = tree.shared();
                let state = CapnpBrokerState::new_configured(
                    &Arc::new(super::super::FanoutState::new()),
                    Arc::new(std::sync::Mutex::new(
                        super::super::BrokerChannels::default(),
                    )),
                    super::super::PeerPublisher::disabled(),
                    Some(tree.authority()),
                    None,
                    None,
                    HostServices {
                        workspace_memberships: Some(Arc::new(DiscoveryMemberships)),
                        ..Default::default()
                    },
                )
                .0;
                assert_eq!(state.published_surfaces_generation(), 0);
                state.refresh_workspace_membership_services().unwrap();
                assert_eq!(state.published_surfaces_generation(), 1);
                assert_eq!(membership_surface_counts(&state, &room), (1, 4));
                let mut expected = vec![
                    (WORKSPACE, "add-agent"),
                    (WORKSPACE, "grant-app-authorship"),
                    (WORKSPACE, "create-task"),
                    (WORKSPACE, "queue-agent-task"),
                ];
                assert_membership_action_set(&state, &room, &expected);
                state.refresh_workspace_membership_services().unwrap();
                assert_eq!(
                    state.published_surfaces_generation(),
                    1,
                    "unchanged polling is not a source mutation"
                );
                tree.write()
                    .desktop
                    .workspaces
                    .first_mut()
                    .unwrap()
                    .capabilities
                    .push(collab_types::CapOverride::membership(
                        collab_types::Subject::agent("scribe"),
                        room.clone(),
                        collab_types::Verb::Read,
                    ));
                state.refresh_workspace_membership_services().unwrap();
                assert_eq!(state.published_surfaces_generation(), 2);
                assert_eq!(membership_surface_counts(&state, &room), (1, 6));
                expected.push(("desktop:agent:scribe", "read-availability"));
                expected.push(("desktop:agent:scribe", "remove"));
                assert_membership_action_set(&state, &room, &expected);
                state.refresh_workspace_membership_services().unwrap();
                assert_eq!(state.published_surfaces_generation(), 2);
                assert_membership_action_set(&state, &room, &expected);
                let peer = *state
                    .workspace_membership_peers
                    .lock()
                    .unwrap()
                    .get(&room)
                    .unwrap();
                state.invocation_subscribers.lock().unwrap().remove(&peer);
                for _ in 0..16 {
                    if state.published_surfaces_generation() == 3 {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                assert_eq!(
                    state.published_surfaces_generation(),
                    3,
                    "real service retirement invalidates discovery"
                );
                assert_eq!(membership_surface_counts(&state, &room), (0, 0));
                assert_membership_action_set(&state, &room, &[]);
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
    fn public_arguments_choose_only_closed_commands() {
        let add = envelope(
            WORKSPACE,
            "add-agent",
            Some(serde_json::json!({"agent": " Scribe ", "preset": "custom"})),
        );
        assert_eq!(
            membership_command(&add).unwrap(),
            WorkspaceMembershipCommand::AddAgent {
                agent: "scribe".into(),
                preset: AgentMembershipPreset::Custom,
            }
        );
        for field in ["caller", "certificate", "room", "capability", "signing_key"] {
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

    struct AvailabilityFixture {
        state: Arc<CapnpBrokerState>,
        membership: crate::test_support::WorkspaceTreeHandle,
        caller: tilo_actions::remote::Caller,
        host: Arc<dyn tilo_actions::remote::ActionHost + Send + Sync>,
        _origin: crate::peer_actions::PolicyOrigin,
        _receiver: Option<tokio::sync::mpsc::UnboundedReceiver<super::super::TaskRunEnvelope>>,
    }

    struct AvailabilityAuthority {
        certificate: tilo_user_identity::DeviceCert,
        room: uuid::Uuid,
        time: crate::peer_actions::PolicyTime,
    }

    impl crate::peer_actions::CertificateAuthority for AvailabilityAuthority {
        fn policy_time(&self) -> Result<Option<crate::peer_actions::PolicyTime>, String> {
            Ok(Some(self.time.clone()))
        }

        fn authorize(
            &self,
            room: uuid::Uuid,
            certificate: &tilo_user_identity::DeviceCert,
        ) -> Result<(), String> {
            if room == self.room && certificate == &self.certificate {
                Ok(())
            } else {
                Err("availability fixture certificate is not current".into())
            }
        }
    }

    type PendingAvailability = tilo_actions::remote::Pending<
        Result<tilo_actions::remote::InvocationReceipt, tilo_actions::remote::ActionHostError>,
    >;

    fn availability_identity() -> (
        ed25519_dalek::SigningKey,
        tilo_user_identity::DeviceCert,
        crate::peer_actions::PolicyOrigin,
    ) {
        let user = ed25519_dalek::SigningKey::from_bytes(&[83; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[84; 32]);
        let clock = crate::peer_actions::OwnerPolicyClock::new().unwrap();
        let origin = clock.new_origin();
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: user.verifying_key().to_bytes(),
            device_public_key: device.verifying_key().to_bytes(),
            device_label: "Availability fixture device".into(),
            capabilities: tilo_user_identity::DeviceCapabilities::member(),
            issued_at_ns: clock.now_ns().unwrap(),
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &user).unwrap();
        (device, certificate, origin)
    }

    fn availability_fixture(available: bool) -> AvailabilityFixture {
        let (device, certificate, origin) = availability_identity();
        let mut workspace = crate::test_support::Workspace::new(
            crate::test_support::WorkspaceId(83),
            "private workspace title",
        );
        workspace.owner = Some(hex::encode(certificate.user_public_key));
        let room = workspace.room.to_string();
        workspace.grant_capability(collab_types::CapOverride::membership(
            collab_types::Subject::agent("scribe"),
            room.clone(),
            collab_types::Verb::Read,
        ));
        let route = CrdtRoute::cluster(&room);
        let mut tree = crate::test_support::WorkspaceTree::default();
        tree.workspaces.push(workspace);
        let mut admitted = super::super::admit_verified_join(
            &tree,
            &route,
            super::super::VerifiedJoinIdentity {
                user_public_key: certificate.user_public_key,
                device_public_key: certificate.device_public_key,
                capped: false,
            },
        )
        .unwrap();
        super::super::admit_agent_delegation(&tree, &route, &mut admitted, "scribe").unwrap();
        let membership = tree.shared();
        let (state, _bridge) = CapnpBrokerState::new_configured(
            &Arc::new(super::super::FanoutState::new()),
            Arc::new(std::sync::Mutex::new(
                super::super::BrokerChannels::default(),
            )),
            super::super::PeerPublisher::disabled(),
            Some(membership.authority()),
            None,
            None,
            HostServices {
                workspace_memberships: Some(Arc::new(PrivateAvailabilityMemberships)),
                ..Default::default()
            },
        );
        assert!(
            state
                .certificate_authority
                .set(Arc::new(AvailabilityAuthority {
                    certificate: certificate.clone(),
                    room: uuid::Uuid::parse_str(&room).unwrap(),
                    time: origin.capture().unwrap(),
                }))
                .is_ok()
        );
        state.refresh_workspace_membership_services().unwrap();
        let receiver = available.then(|| {
            let (tx, receiver) = tokio::sync::mpsc::unbounded_channel();
            state.task_subscribers.lock().unwrap().insert(
                83,
                super::super::TaskSubscription {
                    agent_id: "scribe".into(),
                    cluster_id: room.clone(),
                    tx,
                    subscription: uuid::Uuid::new_v4(),
                    custody: super::super::TaskSubscriptionCustody::Admitted {
                        route: route.clone(),
                        identity: Box::new(admitted),
                    },
                },
            );
            receiver
        });
        let caller = tilo_actions::remote::Caller::authenticated(
            certificate.user_public_key,
            certificate.device_public_key,
        )
        .in_room(room.clone())
        .in_new_execution();
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: room,
            device_certificate: serde_json::to_vec(&certificate).unwrap(),
            proof: ed25519_dalek::Signer::sign(
                &device,
                &runtime_identity::collaboration_join_signing_bytes(&route.cluster_id),
            )
            .to_bytes()
            .to_vec(),
        }
        .encode()
        .unwrap();
        let host = state
            .certified_local_action_host(&caller, &presentation)
            .unwrap();
        AvailabilityFixture {
            state,
            membership,
            caller,
            host,
            _origin: origin,
            _receiver: receiver,
        }
    }

    fn availability_request(
        target: tilo_actions::remote::RemoteActionTarget,
        revision: String,
    ) -> tilo_actions::remote::InvokeRequest {
        tilo_actions::remote::InvokeRequest {
            invocation: tilo_actions::InvocationId::new(),
            target,
            argument: tilo_actions::remote::InvokeArgument::None,
            context: None,
            confirmation: None,
            revision: Some(revision),
            fresh_binding: None,
            on_behalf_of: None,
        }
    }

    async fn listed_availability_request(
        fixture: &AvailabilityFixture,
    ) -> tilo_actions::remote::InvokeRequest {
        let mut pending = fixture.host.query(
            &fixture.caller,
            tilo_actions::remote::CatalogueQuery::default(),
        );
        let page = loop {
            if let tilo_actions::remote::PendingPoll::Ready(result) = pending.poll_ready() {
                break result.expect("the production catalogue lists availability");
            }
            tokio::task::yield_now().await;
        };
        let target = page
            .rows
            .iter()
            .find(|row| {
                row.app_id == APP
                    && row.action.as_ref().is_some_and(|action| {
                        action.address.resource == format!("{AGENT_PREFIX}scribe")
                            && action.address.action == "read-availability"
                    })
            })
            .and_then(tilo_actions::remote::CatalogueRow::target)
            .expect("the exact Agent availability action is publicly discoverable");
        availability_request(target, page.revision)
    }

    async fn invoke_availability(fixture: &AvailabilityFixture) -> serde_json::Value {
        let request = listed_availability_request(fixture).await;
        let mut pending = fixture.host.invoke(&fixture.caller, request.clone());
        admit_pending(&mut pending).await;
        let update = observe_terminal(fixture, request.invocation).await;
        let tilo_actions::InvocationUpdate::Succeeded { output, .. } = update else {
            panic!("agent availability must return a typed successful observation")
        };
        output.value.expect("availability value")
    }

    async fn admit_pending(pending: &mut PendingAvailability) {
        loop {
            if let tilo_actions::remote::PendingPoll::Ready(result) = pending.poll_ready() {
                result.expect("availability read is admitted");
                return;
            }
            tokio::task::yield_now().await;
        }
    }

    async fn assert_refused_without_reservation(
        fixture: &AvailabilityFixture,
        request: tilo_actions::remote::InvokeRequest,
    ) {
        let invocation = request.invocation;
        let mut pending = fixture.host.invoke(&fixture.caller, request);
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                match pending.poll_ready() {
                    tilo_actions::remote::PendingPoll::Ready(result) => {
                        assert!(result.is_err(), "invalid availability request was admitted");
                        break;
                    }
                    tilo_actions::remote::PendingPoll::Waiting => {
                        tokio::task::yield_now().await;
                    }
                    tilo_actions::remote::PendingPoll::Abandoned => {
                        panic!("availability refusal was abandoned")
                    }
                }
            }
        })
        .await
        .expect("availability refusal completes");
        assert!(
            fixture
                .state
                .remote_job(&fixture.caller, &invocation)
                .is_none(),
            "a refused availability read cannot reserve a job"
        );
    }

    async fn observe_terminal(
        fixture: &AvailabilityFixture,
        invocation: tilo_actions::InvocationId,
    ) -> tilo_actions::InvocationUpdate {
        let job = fixture
            .state
            .remote_job(&fixture.caller, &invocation)
            .expect("availability invocation retains its exact broker job");
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let Some(update) = fixture.state.invocation_status(job)
                    && update.is_terminal()
                {
                    break update;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("availability owner turn terminates")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_availability_action_is_exact_typed_and_content_free() {
        tokio::task::LocalSet::new()
            .run_until(Box::pin(async {
                let unavailable = availability_fixture(false);
                assert_eq!(
                    invoke_availability(&unavailable).await,
                    serde_json::json!({"availability": "unavailable"})
                );
                drop(unavailable);
                let available = availability_fixture(true);
                assert_eq!(
                    invoke_availability(&available).await,
                    serde_json::json!({"availability": "available"})
                );

                let unknown = availability_fixture(false);
                let poisoned = Arc::clone(&unknown.state);
                let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                    let _guard = poisoned.task_subscribers.lock().unwrap();
                    panic!("poison only the availability observation lane");
                }));
                assert_eq!(
                    invoke_availability(&unknown).await,
                    serde_json::json!({"availability": "unknown"})
                );
                drop(unknown);

                for revoke_target in [true, false] {
                    let revoked = availability_fixture(true);
                    let request = listed_availability_request(&revoked).await;
                    let mut pending = revoked.host.invoke(&revoked.caller, request.clone());
                    admit_pending(&mut pending).await;
                    {
                        let mut tree = revoked.membership.write();
                        if revoke_target {
                            tree.workspaces[0].capabilities.clear();
                        } else {
                            tree.workspaces[0].owner = None;
                        }
                    }
                    assert!(matches!(
                        observe_terminal(&revoked, request.invocation).await,
                        tilo_actions::InvocationUpdate::Failed { .. }
                    ));
                    drop(revoked);
                }

                let malformed = availability_fixture(false);
                for argument in [
                    serde_json::Map::new(),
                    serde_json::Map::from_iter([("private".into(), serde_json::json!("value"))]),
                ] {
                    let mut request = listed_availability_request(&malformed).await;
                    request.argument = tilo_actions::remote::InvokeArgument::Json(argument);
                    assert_refused_without_reservation(&malformed, request).await;
                }
                drop(malformed);

                let listed = listed_availability_request(&available).await;
                let mut nonmember = listed.clone();
                nonmember.invocation = tilo_actions::InvocationId::new();
                nonmember.target.address.resource = format!("{AGENT_PREFIX}not-a-member");
                let mut wrong_room = listed;
                wrong_room.invocation = tilo_actions::InvocationId::new();
                wrong_room.target.provider =
                    tilo_actions::remote::workspace_provider(&uuid::Uuid::new_v4().to_string());
                for request in [nonmember, wrong_room] {
                    assert_refused_without_reservation(&available, request).await;
                }
                drop(available);
            }))
            .await;
    }

    #[test]
    fn membership_actions_are_admin_only() {
        let mut workspace =
            tilo_desktop_model::Workspace::new(tilo_desktop_model::WorkspaceId(1), "Membership");
        let room = workspace.room.to_string();
        workspace.grant_capability(collab_types::CapOverride::membership(
            collab_types::Subject::agent("scribe"),
            room,
            collab_types::Verb::Read,
        ));
        let actions = membership_actions(&workspace, 0);
        assert!(
            actions
                .iter()
                .filter(|action| action.action != "read-availability")
                .all(|action| {
                    action.contract.permission == tilo_actions::PermissionRequirement::Admin
                        && action.operation_kind == tilo_actions::OperationKind::Effect
                })
        );
        let availability = actions
            .iter()
            .find(|action| action.action == "read-availability")
            .expect("every current Agent has a public availability read");
        assert_eq!(
            availability.contract.permission,
            tilo_actions::PermissionRequirement::Read
        );
        assert_eq!(
            availability.operation_kind,
            tilo_actions::OperationKind::View
        );
        assert!(actions.iter().any(|action| {
            action.resource == WORKSPACE && action.action == "grant-app-authorship"
        }));
    }
}
