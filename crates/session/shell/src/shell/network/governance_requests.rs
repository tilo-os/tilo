//! Taking the governance changes made at this device to their rooms'
//! governors. A room's governance changes only at its governor, its host's
//! `Rooms`; this device asks and reads the answer back from the room's
//! admitted document, as every other member does.
use super::ShellNetwork;
use crate::shell::state::{GovernanceRequest, OwnershipRequest, RoomId};
use tilo_action_policy::{Change, roles::governance};
use tilo_desktop_model::{
    OWNERSHIP_CANCEL_ACTION_ID, OWNERSHIP_DECLINE_ACTION_ID, OWNERSHIP_OFFER_ACTION_ID,
};

/// Why a change has no route to its governor yet.
const NO_GOVERNOR_ACTION: &str = "the workspace host has no action for this change yet";

/// What a request asks of the room's host: one of its room governance
/// actions, or an action its governor publishes for the room's desktop.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum GovernanceRoute {
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
    /// Accepting the open offer `offer` (its id, hex) of the room to this
    /// device's person: signed by this device's own host, which governs the
    /// room once its governor hands it on.
    AcceptOwnership { offer: String },
    /// An ownership step of the person the room is offered to, asked of
    /// the host governing it, whoever's that is.
    Offered {
        action: &'static str,
        arguments: serde_json::Map<String, serde_json::Value>,
    },
}

/// The route `request` takes to its room's governor, or why it has none.
pub(crate) fn route(request: GovernanceRequest, now_ms: u64) -> Result<GovernanceRoute, String> {
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
            Ok(GovernanceRoute::Rooms {
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
            Ok(GovernanceRoute::Rooms {
                action: governance::RESTRICT,
                arguments,
            })
        }
        GovernanceRequest::Policy(Change::Unrestrict { person: who }) => {
            Ok(GovernanceRoute::Rooms {
                action: governance::UNRESTRICT,
                arguments: person(who)?,
            })
        }
        // Removing or banning a member takes the person out of the room at
        // its host: their grants, role and devices go with them.
        GovernanceRequest::Policy(Change::RemoveMember { person: who }) => {
            Ok(GovernanceRoute::Rooms {
                action: governance::REMOVE_MEMBER,
                arguments: person(who)?,
            })
        }
        GovernanceRequest::Policy(Change::Ban { person: who }) => Ok(GovernanceRoute::Rooms {
            action: governance::BAN,
            arguments: person(who)?,
        }),
        GovernanceRequest::Policy(Change::Unban { person: who }) => Ok(GovernanceRoute::Rooms {
            action: governance::UNBAN,
            arguments: person(who)?,
        }),
        GovernanceRequest::Policy(Change::Grant(grant)) => Ok(GovernanceRoute::Membership {
            action: governance::GRANT,
            argument: serde_json::json!({ "grant": grant }),
        }),
        GovernanceRequest::Policy(Change::Revoke(grant)) => Ok(GovernanceRoute::Membership {
            action: governance::REVOKE,
            argument: serde_json::json!({ "grant": grant }),
        }),
        GovernanceRequest::Command(command) => Ok(GovernanceRoute::Command(command)),
        GovernanceRequest::Ownership(OwnershipRequest::Offer { person: who }) => {
            Ok(GovernanceRoute::Rooms {
                action: OWNERSHIP_OFFER_ACTION_ID,
                arguments: person(who)?,
            })
        }
        GovernanceRequest::Ownership(OwnershipRequest::Cancel { offer }) => {
            Ok(GovernanceRoute::Rooms {
                action: OWNERSHIP_CANCEL_ACTION_ID,
                arguments: offer_named(offer)?,
            })
        }
        GovernanceRequest::Ownership(OwnershipRequest::Decline { offer }) => {
            Ok(GovernanceRoute::Offered {
                action: OWNERSHIP_DECLINE_ACTION_ID,
                arguments: offer_named(offer)?,
            })
        }
        GovernanceRequest::Ownership(OwnershipRequest::Accept { offer }) => {
            Ok(GovernanceRoute::AcceptOwnership { offer })
        }
        GovernanceRequest::GrantSandbox(_) | GovernanceRequest::RevokeSandbox(_) => {
            Err(NO_GOVERNOR_ACTION.into())
        }
    }
}

/// The room host this device runs, answering for the rooms placed on it
/// through its in-process caller: a request for one of those rooms is
/// asked there, with or without a running network.
pub(crate) trait LocalGovernor: Send + Sync {
    /// Whether this device's own host governs `room`.
    fn governs(&self, room: RoomId) -> bool;
    /// Ask the host for `route` in `room`, and wait for its answer.
    ///
    /// # Errors
    /// The host refused it, or could not be asked.
    fn ask(&self, room: RoomId, route: GovernanceRoute) -> Result<(), String>;
}

impl ShellNetwork {
    /// Install the room host this device runs as the governor of the rooms
    /// placed on it.
    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "the in-process room host installs itself")
    )]
    pub(crate) fn install_local_governor(&self, governor: std::sync::Arc<dyn LocalGovernor>) {
        let _ = self.local_governor.set(governor);
        // Requests waiting for a governor out of reach may reach this one.
        if let Ok(mut waiting) = self.governance_waiting.lock() {
            *waiting = None;
        }
    }

    /// Take the governance changes made here to their governors: on the
    /// peer-sync worker, as each waits for its governor's answer. Costs one
    /// read of the tree when there is none, or when every waiting one is
    /// for a governor out of reach and neither a room nor the queue changed
    /// since.
    pub(super) fn send_governance_requests_off_loop(&self) {
        let queued = {
            let tree = self.tree.read();
            if !tree.has_governance_requests() {
                return;
            }
            (tree.desktop.room_epoch, tree.governance_requests.len())
        };
        if self
            .governance_waiting
            .lock()
            .is_ok_and(|waiting| *waiting == Some(queued))
        {
            return;
        }
        let requests = self.tree.write().take_governance_requests();
        if let Err(requests) = self.governance_off_loop(requests) {
            self.send_governance_requests(requests);
        }
    }

    /// Ask each request's governor, in order, and say what was refused.
    /// A request whose governor is out of reach is kept, saved with the
    /// tree, and asked again once a room's state changed (a session to a
    /// host came up), another request was made, or this device's own host
    /// was installed.
    pub(super) fn send_governance_requests(&self, requests: Vec<(RoomId, GovernanceRequest)>) {
        let mut waiting = Vec::new();
        for (room, request) in requests {
            match self.ask_governor_if_reachable(room, &request) {
                Some(Ok(())) => {}
                Some(Err(error)) => {
                    tracing::warn!(%room, %error, "the workspace host did not take a governance change");
                }
                None => waiting.push((room, request)),
            }
        }
        if waiting.is_empty() {
            return;
        }
        let mut tree = self.tree.write();
        tree.requeue_governance(waiting);
        let queued = (tree.desktop.room_epoch, tree.governance_requests.len());
        drop(tree);
        if let Ok(mut gate) = self.governance_waiting.lock() {
            *gate = Some(queued);
        }
    }

    /// Ask `room`'s governor for `request` when it can be reached now:
    /// `None` when it cannot, and nothing was asked.
    fn ask_governor_if_reachable(
        &self,
        room: RoomId,
        request: &GovernanceRequest,
    ) -> Option<Result<(), String>> {
        let route = match route(request.clone(), tilo_clock::unix_now_ms()) {
            Ok(route) => route,
            Err(refused) => return Some(Err(refused)),
        };
        if let Some(local) = self.local_governor.get()
            && local.governs(room)
        {
            return Some(local.ask(room, route));
        }
        if !self.governor_reachable(room, &route) {
            return None;
        }
        Some(self.ask_governor_over_network(room, route))
    }

    /// Whether `room`'s governor can be asked for `route` over the network
    /// now: the network runs and this device holds a session with the host.
    fn governor_reachable(&self, room: RoomId, route: &GovernanceRoute) -> bool {
        if self.runtime.is_none() {
            return false;
        }
        match route {
            GovernanceRoute::Rooms { .. } => self.administered_room(room).is_some(),
            GovernanceRoute::Membership { .. } | GovernanceRoute::Command(_) => {
                self.governed_by_a_reachable_host(room)
            }
            GovernanceRoute::AcceptOwnership { .. } | GovernanceRoute::Offered { .. } => {
                self.held_room(room).is_some()
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
        let route = route(request, tilo_clock::unix_now_ms())?;
        if let Some(local) = self.local_governor.get()
            && local.governs(room)
        {
            return local.ask(room, route);
        }
        self.ask_governor_over_network(room, route)
    }

    fn ask_governor_over_network(
        &self,
        room: RoomId,
        route: GovernanceRoute,
    ) -> Result<(), String> {
        match route {
            GovernanceRoute::Rooms { action, arguments } => {
                let held = self
                    .administered_room(room)
                    .ok_or("this device holds no session with the workspace host")?;
                self.govern(&held, action, arguments).map(drop)
            }
            GovernanceRoute::Membership { action, argument } => {
                let answer = self.invoke_governor(
                    room,
                    tilo_desktop_model::commands::APP_ID,
                    WORKSPACE_MEMBERSHIP_RESOURCE,
                    action,
                    Some(argument),
                );
                super::governor_actions::wait_for_governor(answer)
            }
            GovernanceRoute::Command(command) => self.govern_with_command_and_wait(room, command),
            GovernanceRoute::AcceptOwnership { offer } => self.accept_ownership(room, &offer),
            GovernanceRoute::Offered { action, arguments } => {
                let held = self
                    .held_room(room)
                    .ok_or("this device holds no session with the workspace host")?;
                self.govern(&held, action, arguments).map(drop)
            }
        }
    }
}

/// The arguments naming the open offer `offer` (its id, hex).
fn offer_named(offer: String) -> Result<serde_json::Map<String, serde_json::Value>, String> {
    if offer.len() != 64 || hex::decode(&offer).is_err() {
        return Err("an offer is named by its id".into());
    }
    let mut arguments = serde_json::Map::new();
    arguments.insert("offer".into(), offer.into());
    Ok(arguments)
}

/// The resource the room's own membership actions are published on.
const WORKSPACE_MEMBERSHIP_RESOURCE: &str = "desktop:workspace";

#[cfg(test)]
mod tests {
    use super::{GovernanceRoute, route};
    use crate::shell::state::GovernanceRequest;
    use tilo_action_policy::{Change, Role, roles::governance};

    const PERSON: &str = "ab";

    fn person() -> String {
        PERSON.repeat(32)
    }

    #[test]
    fn a_role_change_asks_the_hosts_set_role_action() {
        let Ok(GovernanceRoute::Rooms { action, arguments }) = route(
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
        let Ok(GovernanceRoute::Rooms { action, arguments }) = route(
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
            Ok(GovernanceRoute::Command(command))
        );
    }

    #[test]
    fn removing_or_banning_a_member_is_the_hosts_room_governance() {
        for (change, expected) in [
            (
                Change::RemoveMember { person: person() },
                governance::REMOVE_MEMBER,
            ),
            (Change::Ban { person: person() }, governance::BAN),
        ] {
            let Ok(GovernanceRoute::Rooms { action, arguments }) =
                route(GovernanceRequest::Policy(change), 0)
            else {
                panic!("a removal goes to the host's room governance");
            };
            assert_eq!(action, expected);
            assert_eq!(arguments["person"], person());
        }
    }

    #[test]
    fn taking_an_offer_back_or_declining_it_names_the_offer_to_the_host() {
        use crate::shell::state::OwnershipRequest;
        let offer = "cd".repeat(32);
        let Ok(GovernanceRoute::Rooms { action, arguments }) = route(
            GovernanceRequest::Ownership(OwnershipRequest::Cancel {
                offer: offer.clone(),
            }),
            0,
        ) else {
            panic!("the owner takes an offer back at the room's own host");
        };
        assert_eq!(action, tilo_desktop_model::OWNERSHIP_CANCEL_ACTION_ID);
        assert_eq!(arguments["offer"], offer);

        let Ok(GovernanceRoute::Offered { action, arguments }) = route(
            GovernanceRequest::Ownership(OwnershipRequest::Decline {
                offer: offer.clone(),
            }),
            0,
        ) else {
            panic!("the person offered the room declines at whoever's host governs it");
        };
        assert_eq!(action, tilo_desktop_model::OWNERSHIP_DECLINE_ACTION_ID);
        assert_eq!(arguments["offer"], offer);

        assert!(
            route(
                GovernanceRequest::Ownership(OwnershipRequest::Decline { offer: "x".into() }),
                0,
            )
            .is_err(),
            "an offer is named by its id"
        );
    }

    #[test]
    fn a_change_the_host_has_no_action_for_is_said_so_not_applied_here() {
        assert!(route(GovernanceRequest::RevokeSandbox("cap-1".into()), 0).is_err());
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

    /// This device's own host, applying what it is asked as the room's
    /// projection then carries it: a rename shows as the workspace's name.
    struct OwnHost {
        room: crate::shell::state::RoomId,
        tree: crate::shell::state::WorkspaceTreeHandle,
    }

    impl super::LocalGovernor for OwnHost {
        fn governs(&self, room: crate::shell::state::RoomId) -> bool {
            room == self.room
        }

        fn ask(
            &self,
            room: crate::shell::state::RoomId,
            route: GovernanceRoute,
        ) -> Result<(), String> {
            let GovernanceRoute::Command(
                tilo_desktop_model::commands::DesktopCommand::RenameWorkspace { name },
            ) = route
            else {
                return Err("this host only renames".into());
            };
            self.tree
                .write()
                .desktop
                .workspaces
                .iter_mut()
                .find(|workspace| workspace.room() == room)
                .ok_or("no such room")?
                .name = name;
            Ok(())
        }
    }

    #[test]
    fn a_rename_made_with_its_host_out_of_reach_applies_after_a_restart() {
        let directory = tempfile::tempdir().expect("tempdir");
        let path = directory.path().join("state.json");
        let mut tree = crate::shell::state::WorkspaceTree::bootstrap_default();
        tree.persist_path = Some(path.clone());
        let room = tree.desktop.active_workspace().expect("a workspace").room();
        let rename = GovernanceRequest::Command(
            tilo_desktop_model::commands::DesktopCommand::RenameWorkspace {
                name: "Design".into(),
            },
        );
        tree.request_governance(room, rename.clone());
        let handle = tree.shared();

        // No network and no host yet: nothing is asked, nothing is lost.
        let (network, _commands) = crate::shell::network::shell_network_for_test(handle.clone(), 1);
        network.send_governance_requests_off_loop();
        assert_eq!(
            handle.read().governance_requests,
            vec![(room, rename.clone())],
            "a change whose host is out of reach waits"
        );
        network.send_governance_requests_off_loop();
        assert_eq!(
            handle.read().governance_requests.len(),
            1,
            "and is not asked again on every tick"
        );
        crate::shell::persist::save_tree_to(&handle.read(), &path).expect("save");
        drop(network);
        drop(handle);

        // A restart, with this device's own host governing the room.
        let mut loaded = crate::shell::persist::load_tree_from(&path).expect("load");
        assert_eq!(
            loaded.governance_requests,
            vec![(room, rename)],
            "the waiting change survives the restart"
        );
        loaded.persist_path = Some(path);
        let handle = loaded.shared();
        let (network, _commands) = crate::shell::network::shell_network_for_test(handle.clone(), 1);
        network.install_local_governor(std::sync::Arc::new(OwnHost {
            room,
            tree: handle.clone(),
        }));
        network.send_governance_requests_off_loop();
        assert!(
            handle.read().governance_requests.is_empty(),
            "the host took it"
        );
        assert_eq!(
            handle
                .read()
                .desktop
                .workspaces
                .iter()
                .find(|workspace| workspace.room() == room)
                .map(|workspace| workspace.name.as_str()),
            Some("Design"),
            "and the rename applies"
        );
    }
}
