//! Governed agent membership through the ordinary durable invocation surface.
//! Public arguments select only a closed command; caller custody and signing
//! authority remain host-owned.
use super::room_service::{ServiceOutcome, refused};
use super::{
    Arc, CapnpBrokerState, CrdtRoute, InvocationEnvelope, PublishedAction, PublishedInput,
    PublishedSurface,
};
use tilo_action_policy::roles::AgentPreset;
use tilo_owner_commit::{CommitOperation, OwnerCommitRequest};
use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand;

const APP: &str = "so.tilo.desktop";
const RECORD: &str = "membership";
const WORKSPACE: &str = "desktop:workspace";
const AGENT_PREFIX: &str = "desktop:agent:";
const TASK_PREFIX: &str = "desktop:task:";
const GRANT_APP_AUTHORSHIP: &str = "grant-app-authorship";

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
                (WORKSPACE.into(), GRANT_APP_AUTHORSHIP, Some(argument))
            }
            WorkspaceMembershipCommand::GrantForTask {
                task,
                node,
                kinds,
                answer,
            } => {
                let argument = format!(
                    "{}{}",
                    tilo_actions::ARGUMENTS_JSON_PREFIX,
                    serde_json::json!({"node": node, "kinds": kinds, "answer": answer})
                );
                (format!("desktop:{task}"), "grant", Some(argument))
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
            WorkspaceMembershipCommand::RevokeForTask { task, grant } => {
                let argument = format!(
                    "{}{}",
                    tilo_actions::ARGUMENTS_JSON_PREFIX,
                    serde_json::json!({"grant": grant})
                );
                (format!("desktop:{task}"), "revoke", Some(argument))
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
            super::cert_capped_in(Some(authority.as_ref()), room, certificate),
        );
        let mut admitted = membership.decide_in_room(&route.cluster_id, |workspace| {
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

    pub(super) fn refresh_workspace_membership_services(self: &Arc<Self>) -> Result<(), String> {
        let Some(service) = self.services.workspace_memberships.clone() else {
            return Ok(());
        };
        let Some(authority) = &self.membership else {
            return Ok(());
        };
        let now = self.policy_now_ns()?;
        // Describe every room on one borrowed sample; the membership
        // service is asked about each room only after the borrow ends.
        let rooms = authority.decide_in_tree(|tree| {
            tree.workspaces
                .iter()
                .map(|workspace| {
                    (
                        workspace.room.to_string(),
                        workspace.name.clone(),
                        membership_actions(workspace, now / 1_000_000_000),
                        workspace
                            .agent_members_at(now / 1_000_000_000)
                            .into_iter()
                            .map(|(agent, _)| agent)
                            .collect::<std::collections::BTreeSet<_>>(),
                    )
                })
                .collect::<Vec<_>>()
        })?;
        for (room, name, mut actions, current_agents) in rooms {
            if service.requires_room(&room) {
                actions.extend(super::workspace_checkpoints::actions());
                actions.extend(super::workspace_tasks::actions(
                    &service.task_snapshot(&room)?,
                    &name,
                    &current_agents,
                ));
            } else if service.grants_authorship_at_host(&room) {
                // Governed by the node that stewards it: the one change
                // this device asks there is an opening app's authorship.
                actions.retain(|action| action.action == GRANT_APP_AUTHORSHIP);
            } else {
                continue;
            }

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
        let surface = PublishedSurface {
            peer_id: peer,
            app_id: APP.into(),
            route: CrdtRoute::record(APP, &room, RECORD),
            live: true,
            actions,
        };
        self.workspace_membership_peers
            .lock()
            .map_err(|_| "membership service registry unavailable")?
            .insert(room.clone(), peer);
        let serve_room = room.clone();
        self.serve_room(
            surface,
            move |state, envelope| {
                let (service, room) = (service.clone(), serve_room.clone());
                async move {
                    match state
                        .execute_workspace_membership(peer, &room, &envelope, service.as_ref())
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
                if let Ok(mut peers) = state.workspace_membership_peers.lock() {
                    peers.remove(&room);
                }
            },
        )?;
        self.publish_changed_action_catalogue();
        Ok(())
    }

    async fn execute_workspace_checkpoint(
        &self,
        room: &str,
        envelope: &InvocationEnvelope,
        command: tilo_peer_state::workspace_checkpoint::CheckpointCommand,
        pending: &super::PendingInvocation,
        service: &dyn crate::services::WorkspaceMemberships,
    ) -> Result<serde_json::Value, String> {
        if !service.requires_room(room) {
            return Err("checkpoint room changed".into());
        }
        if command.mutates() {
            let key = pending
                .journal_key
                .as_ref()
                .ok_or("checkpoint has no durable admission")?;
            if key.room != room
                || key.invocation != envelope.invocation
                || key.owner != pending.owner
            {
                return Err("checkpoint invocation binding changed".into());
            }
        }
        let original = pending
            .original
            .as_ref()
            .ok_or("checkpoint original custody unavailable")?;
        let caller = original.owner_person_certificate(self, room, envelope)?;
        service.health()?;
        service
            .checkpoint(tilo_peer_state::workspace_checkpoint::CheckpointRequest {
                room: room.into(),
                invocation: envelope.invocation.0,
                caller,
                command,
            })
            .await
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
            let snapshot = service.task_snapshot(room)?;
            if let Some(agent) = super::workspace_tasks::inspection_agent(&snapshot, envelope)? {
                let now = self.policy_now_ns()? / 1_000_000_000;
                let membership = self
                    .membership
                    .as_ref()
                    .ok_or("task inspection workspace authority unavailable")?;
                let current = membership.decide_in_room(room, |workspace| {
                    workspace.is_some_and(|workspace| {
                        workspace
                            .agent_members_at(now)
                            .iter()
                            .any(|(current, _)| current == &agent)
                    })
                })?;
                if !current {
                    return Err("task inspection agent is not a current room member".into());
                }
            }
            return super::workspace_tasks::inspect(&snapshot, envelope);
        }
        if let Some(command) = super::workspace_checkpoints::command(envelope)? {
            return self
                .execute_workspace_checkpoint(room, envelope, command, &pending, service)
                .await;
        }
        let key = pending
            .journal_key
            .as_ref()
            .ok_or("membership invocation has no durable admission")?;
        if key.room != room
            || key.invocation != envelope.invocation
            || key.owner != pending.owner
            || !(service.requires_room(room) || service.grants_authorship_at_host(room))
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

fn membership_command(envelope: &InvocationEnvelope) -> Result<WorkspaceMembershipCommand, String> {
    let command = if let Some(command) = bot_command(envelope)? {
        command
    } else if envelope.resource == WORKSPACE && envelope.action == "add-agent" {
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
            preset: AgentPreset::parse(&input.preset).ok_or("unknown agent membership preset")?,
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
    } else if let Some(task) = envelope.resource.strip_prefix(TASK_PREFIX)
        && envelope.action == "grant"
    {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct TaskGrant {
            node: tilo_action_policy::TreePath,
            kinds: tilo_action_policy::KindSet,
            answer: tilo_action_policy::Answer,
        }
        let encoded = bounded_argument(envelope, "task grant")?;
        let input: TaskGrant = serde_json::from_str(encoded).map_err(|error| error.to_string())?;
        WorkspaceMembershipCommand::GrantForTask {
            task: format!("task:{task}"),
            node: input.node,
            kinds: input.kinds,
            answer: input.answer,
        }
    } else if let Some(task) = envelope.resource.strip_prefix(TASK_PREFIX)
        && envelope.action == "revoke"
    {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct TaskRevoke {
            grant: tilo_action_policy::GrantId,
        }
        let encoded = bounded_argument(envelope, "task grant")?;
        let input: TaskRevoke = serde_json::from_str(encoded).map_err(|error| error.to_string())?;
        WorkspaceMembershipCommand::RevokeForTask {
            task: format!("task:{task}"),
            grant: input.grant,
        }
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
        GRANT_APP_AUTHORSHIP,
        "Workspace",
        "Allow app authorship",
        vec![tilo_actions::ActionParam::required(
            "app",
            tilo_actions::ParamType::Text,
            "Application id",
        )],
    ));
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
                grants: Vec::new(),
            })
        }

        fn execute(&self, _: OwnerCommitRequest) -> tilo_owner_commit::CommitFuture {
            Box::pin(async { Err("discovery fixture must not mutate the owner".into()) })
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
                assert_eq!(membership_surface_counts(&state, &room), (1, 21));
                let mut expected = vec![
                    (WORKSPACE, "add-agent"),
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
                    (WORKSPACE, "create-task"),
                    (WORKSPACE, "queue-agent-task"),
                    ("desktop:tasks", "queue"),
                    ("desktop:tasks", "read-current-tasks"),
                    ("desktop:checkpoints", "capture"),
                    ("desktop:checkpoints", "inspect"),
                    ("desktop:checkpoints", "name-bind"),
                    ("desktop:checkpoints", "find"),
                ];
                assert_membership_action_set(&state, &room, &expected);
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
                        .admit_agent("scribe", KindSet::READ);
                }
                state.refresh_workspace_membership_services().unwrap();
                assert_eq!(state.published_surfaces_generation(), 2);
                assert_eq!(membership_surface_counts(&state, &room), (1, 22));
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
                }
            )
        );
        let (action, argument) = bot_invocation(&command).unwrap();
        assert_eq!(action, "request-bot");
        assert_eq!(
            membership_command(&InvocationEnvelope {
                argument: Some(argument),
                ..request.clone()
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
        let add = envelope(
            WORKSPACE,
            "add-agent",
            Some(serde_json::json!({"agent": " Scribe ", "preset": "custom"})),
        );
        assert_eq!(
            membership_command(&add).unwrap(),
            WorkspaceMembershipCommand::AddAgent {
                agent: "scribe".into(),
                preset: AgentPreset::Custom,
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
            // Far above the few ids other membership commands carry.
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
    fn membership_actions_are_admin_only() {
        let workspace =
            tilo_desktop_model::Workspace::new(tilo_desktop_model::WorkspaceId(1), "Membership");
        let actions = membership_actions(&workspace, 0);
        assert!(actions.iter().all(|action| {
            action.contract.kind == tilo_actions::ActionKind::Outside
                && action.operation_kind == tilo_actions::OperationKind::Effect
        }));
        assert!(actions.iter().any(|action| {
            action.resource == WORKSPACE && action.action == "grant-app-authorship"
        }));
    }
}
