//! Taking the governance changes made at this device to their rooms'
//! governors. A room's governance changes only at its governor, its host's
//! `Rooms`; this device asks and reads the answer back from the room's
//! admitted document, as every other member does.
use super::ShellNetwork;
use crate::shell::state::{GovernanceRequest, RoomId};
use tilo_action_policy::{Change, roles::governance};

/// Why a change has no route to its governor yet.
const NO_GOVERNOR_ACTION: &str = "the workspace host has no action for this change yet";

/// What a request asks of the room's host: one of its room governance
/// actions, or an action its governor publishes for the room's desktop.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Route {
    /// A room governance action of the host's `Rooms`, with its arguments.
    Rooms {
        action: &'static str,
        arguments: serde_json::Map<String, serde_json::Value>,
    },
    /// A membership action of the room itself (`desktop:workspace`).
    Membership {
        action: &'static str,
        argument: serde_json::Value,
    },
    /// A desktop command, applied by the governor.
    Command(tilo_desktop_model::commands::DesktopCommand),
}

/// The route `request` takes to its room's governor, or why it has none.
pub(super) fn route(request: GovernanceRequest, now_ms: u64) -> Result<Route, String> {
    let person = |person: String| -> Result<serde_json::Map<_, _>, String> {
        let mut arguments = serde_json::Map::new();
        if person.len() != 64 || hex::decode(&person).is_err() {
            return Err("a member is named by their user root key".into());
        }
        arguments.insert("person".into(), person.into());
        Ok(arguments)
    };
    match request {
        GovernanceRequest::Policy(
            Change::AddMember { person: who, role } | Change::SetRole { person: who, role },
        ) => {
            let mut arguments = person(who)?;
            arguments.insert("role".into(), role.name().to_ascii_lowercase().into());
            Ok(Route::Rooms {
                action: governance::SET_ROLE,
                arguments,
            })
        }
        GovernanceRequest::Policy(Change::Restrict {
            person: who,
            until_ms,
        }) => {
            let mut arguments = person(who)?;
            let minutes = until_ms.saturating_sub(now_ms).div_ceil(60_000).max(1);
            arguments.insert("minutes".into(), minutes.into());
            Ok(Route::Rooms {
                action: governance::RESTRICT,
                arguments,
            })
        }
        GovernanceRequest::Policy(Change::Unrestrict { person: who }) => Ok(Route::Rooms {
            action: governance::UNRESTRICT,
            arguments: person(who)?,
        }),
        GovernanceRequest::Policy(Change::Unban { person: who }) => Ok(Route::Rooms {
            action: governance::UNBAN,
            arguments: person(who)?,
        }),
        GovernanceRequest::Policy(Change::Grant(grant)) => Ok(Route::Membership {
            action: governance::GRANT,
            argument: serde_json::json!({ "grant": grant }),
        }),
        GovernanceRequest::Policy(Change::Revoke(grant)) => Ok(Route::Membership {
            action: governance::REVOKE,
            argument: serde_json::json!({ "grant": grant }),
        }),
        GovernanceRequest::Command(command) => Ok(Route::Command(command)),
        GovernanceRequest::Policy(Change::RemoveMember { .. } | Change::Ban { .. })
        | GovernanceRequest::GrantSandbox(_)
        | GovernanceRequest::RevokeSandbox(_) => Err(NO_GOVERNOR_ACTION.into()),
    }
}

impl ShellNetwork {
    /// Take the governance changes made here to their governors: on the
    /// peer-sync worker, as each waits for its governor's answer. Costs one
    /// read of the tree when there is none.
    pub(super) fn send_governance_requests_off_loop(&self) {
        if !self.tree.read().has_governance_requests() {
            return;
        }
        let requests = self.tree.write().take_governance_requests();
        if let Err(requests) = self.governance_off_loop(requests) {
            self.send_governance_requests(requests);
        }
    }

    /// Ask each request's governor, in order, and say what was refused.
    pub(super) fn send_governance_requests(&self, requests: Vec<(RoomId, GovernanceRequest)>) {
        for (room, request) in requests {
            if let Err(error) = self.ask_governor(room, request) {
                tracing::warn!(%room, %error, "the workspace host did not take a governance change");
            }
        }
    }

    /// Ask `room`'s governor for `request` and wait for its answer: for a
    /// worker, never the frame loop.
    pub(crate) fn ask_governor(
        &self,
        room: RoomId,
        request: GovernanceRequest,
    ) -> Result<(), String> {
        match route(request, tilo_clock::unix_now_ms())? {
            Route::Rooms { action, arguments } => {
                let held = self
                    .administered_room(room)
                    .ok_or("this device holds no session with the workspace host")?;
                self.govern(&held, action, arguments).map(drop)
            }
            Route::Membership { action, argument } => {
                let answer = self.invoke_governor(
                    room,
                    tilo_desktop_model::commands::APP_ID,
                    WORKSPACE_MEMBERSHIP_RESOURCE,
                    action,
                    Some(argument),
                );
                super::governor_actions::wait_for_governor(answer)
            }
            Route::Command(command) => self.govern_with_command_and_wait(room, command),
        }
    }
}

/// The resource the room's own membership actions are published on.
const WORKSPACE_MEMBERSHIP_RESOURCE: &str = "desktop:workspace";

#[cfg(test)]
mod tests {
    use super::{Route, route};
    use crate::shell::state::GovernanceRequest;
    use tilo_action_policy::{Change, Role, roles::governance};

    const PERSON: &str = "ab";

    fn person() -> String {
        PERSON.repeat(32)
    }

    #[test]
    fn a_role_change_asks_the_hosts_set_role_action() {
        let Ok(Route::Rooms { action, arguments }) = route(
            GovernanceRequest::Policy(Change::SetRole {
                person: person(),
                role: Role::Editor,
            }),
            0,
        ) else {
            panic!("a role change goes to the host's room governance");
        };
        assert_eq!(action, governance::SET_ROLE);
        assert_eq!(arguments["person"], person());
        assert_eq!(arguments["role"], "editor");
    }

    #[test]
    fn a_restriction_asks_for_whole_minutes_from_now() {
        let Ok(Route::Rooms { action, arguments }) = route(
            GovernanceRequest::Policy(Change::Restrict {
                person: person(),
                until_ms: 1_000 + 90_000,
            }),
            1_000,
        ) else {
            panic!("a restriction goes to the host's room governance");
        };
        assert_eq!(action, governance::RESTRICT);
        assert_eq!(arguments["minutes"], 2);
    }

    #[test]
    fn a_rename_is_a_desktop_command_for_the_governor() {
        let command = tilo_desktop_model::commands::DesktopCommand::RenameWorkspace {
            name: "Design".into(),
        };
        assert_eq!(
            route(GovernanceRequest::Command(command.clone()), 0),
            Ok(Route::Command(command))
        );
    }

    #[test]
    fn a_change_the_host_has_no_action_for_is_said_so_not_applied_here() {
        for request in [
            GovernanceRequest::Policy(Change::RemoveMember { person: person() }),
            GovernanceRequest::RevokeSandbox("cap-1".into()),
        ] {
            assert!(route(request, 0).is_err());
        }
    }

    #[test]
    fn a_member_is_named_by_their_user_root_key() {
        assert!(
            route(
                GovernanceRequest::Policy(Change::SetRole {
                    person: "ada".into(),
                    role: Role::Viewer,
                }),
                0,
            )
            .is_err()
        );
    }
}
