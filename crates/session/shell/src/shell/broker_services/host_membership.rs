//! Committing a membership change in a room a host stewards.
//!
//! A room whose channels its host governs is sealed to that host: a change
//! one of its members makes reaches the others through the host, so the
//! change is the host's to commit, decided there by the room's own
//! governance. This forwards each change and answers once it is in this
//! device's admitted room, the same barrier the room's other commits use.

/// How long a membership change a host committed may take to reach this
/// device's admitted room. With the host's own answer (at most
/// `relay_rooms::ADMIN_TIMEOUT`) it stays inside the 30 s a launch waits.
const HOST_AUTHORSHIP_ADMISSION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// In a room a host governs (a home relay, or a workspace someone hosts for
/// this person), a membership change is the host's to commit: its channels
/// are sealed to the room's governor, so every member's change reaches them
/// through it. The host decides it by the room's own governance, as it
/// decides any other change there.
/// Answers with this device's committed view once it carries the change.
pub(super) async fn commit_membership_at_host(
    network: &super::super::network::ShellNetwork,
    owner: &super::super::native_workspace_owner::NativeWorkspaceOwner,
    room: super::super::state::RoomId,
    request: &tilo_owner_commit::OwnerCommitRequest,
) -> Result<tilo_owner_commit::OwnerCommitReceipt, String> {
    const REFUSED: &str = "workspace membership requires the governing owner";
    let tilo_owner_commit::CommitOperation::WorkspaceMembershipCommand {
        caller, command, ..
    } = &request.operation
    else {
        return Err(REFUSED.into());
    };
    let held = MembershipChange::of(command)?;
    let (action, arguments) = match command {
        tilo_peer_state::workspace_membership::WorkspaceMembershipCommand::GrantAppAuthorship {
            app,
        } => {
            network
                .grant_app_authorship_at_host(room, caller, app)
                .await
                .ok_or(REFUSED)??;
            return wait_for_membership(network, owner, room, request, caller, &held).await;
        }
        command => host_membership_invocation(command)?,
    };
    network
        .govern_membership_at_host(room, caller, action, arguments)
        .await
        .ok_or(REFUSED)??;
    wait_for_membership(network, owner, room, request, caller, &held).await
}

/// Wait until the change a host committed is in this device's actual
/// admitted room, and answer with this device's committed view of it.
async fn wait_for_membership(
    network: &super::super::network::ShellNetwork,
    owner: &super::super::native_workspace_owner::NativeWorkspaceOwner,
    room: super::super::state::RoomId,
    request: &tilo_owner_commit::OwnerCommitRequest,
    caller: &tilo_user_identity::DeviceCert,
    held: &MembershipChange,
) -> Result<tilo_owner_commit::OwnerCommitReceipt, String> {
    tokio::time::timeout(HOST_AUTHORSHIP_ADMISSION_TIMEOUT, async {
        loop {
            network.check_native_custody(owner, room)?;
            let receipt = owner.with_current(room, |view| {
                let now = std::time::SystemTime::now();
                let member = view.peer().permissions(caller.device_public_key, now)?;
                if !held.arrived(&member.policy, &tilo_action_policy::Context::at_time(now)) {
                    return Ok(None);
                }
                Ok(Some(tilo_owner_commit::OwnerCommitReceipt {
                    binding: request.binding()?,
                    generation: view.generation(),
                    aggregate_hash: *blake3::hash(&view.peer().durable()?).as_bytes(),
                }))
            })?;
            if let Some(receipt) = receipt {
                return Ok(receipt);
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
    })
    .await
    .map_err(
        |_| "the host committed the membership change but it did not reach this device in time",
    )?
}

/// The room-governance action and arguments a membership command travels as
/// to the host that governs the room, so the wire carries what the room's
/// own actions carry.
fn host_membership_invocation(
    command: &tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
) -> Result<(&'static str, serde_json::Map<String, serde_json::Value>), String> {
    use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand as Command;
    let mut arguments = serde_json::Map::new();
    let action = match command {
        Command::AddAgent { agent, preset } => {
            arguments.insert("agent".into(), agent.clone().into());
            arguments.insert("preset".into(), preset.id().into());
            "add-agent"
        }
        Command::RemoveAgent { agent } => {
            arguments.insert("agent".into(), agent.clone().into());
            "remove-agent"
        }
        Command::Grant { grant } => {
            arguments.insert(
                "grant".into(),
                serde_json::to_value(grant).map_err(|error| error.to_string())?,
            );
            "grant"
        }
        Command::Revoke { grant } => {
            arguments.insert("grant".into(), grant.as_str().into());
            "revoke"
        }
        Command::GrantAppAuthorship { .. } => {
            return Err(
                "letting an app author the room is the host's own operation, not a membership change"
                    .into(),
            );
        }
        _ => return Err(REFUSED_BY_COMMAND.into()),
    };
    Ok((action, arguments))
}

/// Why a command that is not one of the room's own membership changes does
/// not travel to the host.
const REFUSED_BY_COMMAND: &str = "only a room's own membership changes are committed at its host";

/// What a membership change looks like in this device's admitted room, so
/// the wait knows when the host's commit has arrived.
enum MembershipChange {
    AppAuthors {
        app: String,
    },
    Agent {
        agent: String,
        present: bool,
        preset: Option<&'static str>,
    },
    Grant {
        grant: tilo_action_policy::Grant,
        present: bool,
    },
    Revoke {
        id: tilo_action_policy::GrantId,
    },
}

impl MembershipChange {
    fn of(
        command: &tilo_peer_state::workspace_membership::WorkspaceMembershipCommand,
    ) -> Result<Self, &'static str> {
        use tilo_peer_state::workspace_membership::WorkspaceMembershipCommand as Command;
        Ok(match command {
            Command::GrantAppAuthorship { app } => Self::AppAuthors { app: app.clone() },
            Command::AddAgent { agent, preset } => Self::Agent {
                agent: agent.clone(),
                present: true,
                preset: Some(preset.id()),
            },
            Command::RemoveAgent { agent } => Self::Agent {
                agent: agent.clone(),
                present: false,
                preset: None,
            },
            Command::Grant { grant } => Self::Grant {
                grant: grant.clone(),
                present: true,
            },
            Command::Revoke { grant } => Self::Revoke { id: grant.clone() },
            _ => return Err(REFUSED_BY_COMMAND),
        })
    }

    /// Whether `policy` is the room after this change: the authorship, the
    /// agent's grants, or the grant itself, as the change makes it.
    fn arrived(
        &self,
        policy: &tilo_action_policy::RoomPolicy,
        cx: &tilo_action_policy::Context,
    ) -> bool {
        match self {
            Self::AppAuthors { app } => policy.app_authors(app, cx),
            Self::Agent {
                agent,
                present,
                preset,
            } => {
                let subject = tilo_action_policy::Subject::agent(agent);
                let held = policy
                    .grants
                    .iter()
                    .filter(|grant| grant.subject == subject);
                let expected = preset
                    .and_then(tilo_action_policy::roles::AgentPreset::parse)
                    .is_some_and(|preset| {
                        held.clone().any(|grant| {
                            grant
                                .id
                                .as_str()
                                .starts_with(&format!("agent-member:{agent}:{}", preset.id()))
                        })
                    });
                if *present {
                    expected
                } else {
                    held.count() == 0
                }
            }
            Self::Grant { grant, present } => {
                let held = policy.grants.iter().any(|held| held.id == grant.id);
                held == *present
            }
            Self::Revoke { id } => !policy.grants.iter().any(|grant| &grant.id == id),
        }
    }
}
