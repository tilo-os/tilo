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
        let (_, now) = self.checked_policy_time(None)?;
        for workspace in authority.snapshot()?.workspaces {
            let room = workspace.room.to_string();
            if !service.requires_room(&room) {
                continue;
            }
            let actions = membership_actions(&workspace, now / 1_000_000_000);
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
                    self.publish_action_catalogue();
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
        self.publish_action_catalogue();
        let state = self.clone();
        tokio::task::spawn_local(async move {
            while let Some(envelope) = receiver.recv().await {
                let result = state
                    .execute_workspace_membership(peer, &room, &envelope, service.as_ref())
                    .await;
                let update = match result {
                    Ok(generation) => tilo_actions::InvocationUpdate::Succeeded {
                        invocation: envelope.invocation,
                        output: tilo_actions::ActionOutput {
                            value: Some(serde_json::json!({"generation": generation})),
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
            if let Ok(mut surfaces) = state.surfaces.lock() {
                surfaces.remove(&peer);
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
    ) -> Result<u64, String> {
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
        Ok(receipt.generation)
    }
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
            .agent_members_at(now)
            .into_iter()
            .map(|(agent, _)| {
                membership_action(
                    &format!("{AGENT_PREFIX}{agent}"),
                    "remove",
                    &agent,
                    "Remove agent",
                    Vec::new(),
                )
            }),
    );
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

    #[test]
    fn membership_actions_are_admin_only() {
        let workspace =
            tilo_desktop_model::Workspace::new(tilo_desktop_model::WorkspaceId(1), "Membership");
        let actions = membership_actions(&workspace, 0);
        assert!(actions.iter().all(|action| {
            action.contract.permission == tilo_actions::PermissionRequirement::Admin
                && action.operation_kind == tilo_actions::OperationKind::Effect
        }));
        assert!(actions.iter().any(|action| {
            action.resource == WORKSPACE && action.action == "grant-app-authorship"
        }));
    }
}
