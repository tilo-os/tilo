//! The Tilo community and the Tilo team, as this shell holds them.
//!
//! What the community is (the release's `community.toml`, the team
//! principal) and the rules that keep a person in it are shared with every
//! host that holds a person's rooms ([`tilo_community`]); this module
//! re-exports them. What stays here is the shell's side: setting up the
//! team's direct room in its workspace tree.

use crate::shell::state::WorkspaceAuthority as _;

pub use tilo_community::{
    AUTO_JOIN_FIRST_BACKOFF, AUTO_JOIN_MAX_BACKOFF, AutoJoin, COMMUNITY_CONFIG_ENV,
    CommunityConfig, CommunityConfigError, DEFAULT_COMMUNITY_CONFIG, Observed, Step, TeamPrincipal,
    config_path, load, load_from, team_direct_room,
};

/// How a direct room is set up on this end.
#[derive(Clone, Copy)]
pub struct DirectRoomSetup<'a> {
    pub room: crate::shell::state::RoomId,
    /// The other member of the pair: the team, from a reporter's end.
    pub other: &'a str,
    pub label: &'a str,
    /// This end creates and governs the room (the reporter's end).
    pub governing: bool,
    /// Admit the team's triage agent, operated by the team, when the room is
    /// created here. Once only: a room that already exists is left as its
    /// owner made it, so an agent the reporter removed stays removed.
    pub triage: bool,
}

/// Whether the team has its triage agent join reporters' direct rooms, as
/// the community room `community` says: its governance turns it on, and the
/// agent is admitted there.
pub fn team_triages_direct_rooms(
    tree: &crate::shell::state::WorkspaceTree,
    community: crate::shell::state::RoomId,
) -> bool {
    tree.desktop
        .workspaces
        .iter()
        .find(|workspace| workspace.room == community)
        .is_some_and(|workspace| {
            workspace.triage_in_direct_rooms
                && workspace
                    .role_of_agent(tilo_desktop::workspace_settings::TRIAGE_AGENT)
                    .is_some_and(|role| role != collab_types::role::Role::Denied)
        })
}

/// Hold the direct room in `tree`, as [`DirectRoomSetup`] says: a new room
/// is created, and on the governing end owned here, with the other member an
/// Editor and, when asked, the team's triage agent admitted with the team as
/// its operator.
///
/// # Errors
///
/// When this end governs the room and may not administer it.
pub fn prepare_direct_room(
    tree: &mut crate::shell::state::WorkspaceTree,
    setup: &DirectRoomSetup<'_>,
) -> Result<(), String> {
    use crate::shell::state::{CapOverride, Subject, Verb};
    let room = setup.room;
    let held = tree
        .desktop
        .workspaces
        .iter()
        .any(|workspace| workspace.room == room);
    let me = crate::shell::state::local_user_id_for_room(&room);
    if !held {
        let local = tree.append_empty_workspace(setup.label);
        tree.set_room(local, room);
        if setup.governing {
            // Joining a derived room surrenders governance by default; this
            // end creates it, so it governs it.
            tree.room_authority.remove(&room);
            if let Some(workspace) = tree.desktop.workspaces.iter_mut().find(|ws| ws.id == local) {
                workspace.set_owner(me.clone());
                if setup.triage {
                    let room_id = room.to_string();
                    let agent = tilo_desktop::workspace_settings::TRIAGE_AGENT;
                    for cap in
                        tilo_desktop::workspace_settings::triage_agent_grants(&room_id, agent)
                            .into_iter()
                            .chain(std::iter::once(
                                tilo_desktop::workspace_settings::triage_operator_grant(
                                    &room_id,
                                    agent,
                                    setup.other,
                                ),
                            ))
                    {
                        workspace
                            .grant_as(&me, cap)
                            .ok_or_else(|| format!("{me} may not administer room {room}"))?;
                    }
                }
            }
        }
        // Otherwise the other end created and governs it: `set_room` left the
        // authority unpinned, and only the other member of the derived pair
        // may supply its first governance.
    }
    // Idempotent: the creating end makes sure the other member holds Editor,
    // whether the room is new or was opened before.
    if setup.governing && tree.governs_room(room) {
        let workspace = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|ws| ws.room == room)
            .ok_or_else(|| format!("no workspace holds room {room}"))?;
        if workspace.role_of(&setup.other.to_string()).is_none() {
            workspace
                .grant_as(
                    &me,
                    CapOverride::membership(
                        Subject::person(setup.other.to_string()),
                        room.to_string(),
                        Verb::Write,
                    ),
                )
                .ok_or_else(|| format!("{me} may not administer room {room}"))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// With the team's choice on, a reporter's new direct room admits the
    /// triage agent with the team as its operator; the team may launch it
    /// there; the reporter removes it, and opening the room again does not
    /// bring it back.
    #[test]
    fn a_reporters_direct_room_admits_the_team_triage_agent_once() {
        use crate::shell::state::{RoomId, WorkspaceTree};
        use tilo_desktop::workspace_settings::{TRIAGE_AGENT, remove_agent};
        const TEAM: &str = "team-person";
        let room = RoomId::parse("7a1a9e00-0000-4c0d-9a11-0000000000d1").expect("room id");
        let setup = super::DirectRoomSetup {
            room,
            other: TEAM,
            label: "Tilo team",
            governing: true,
            triage: true,
        };
        let mut tree = WorkspaceTree::default();
        super::prepare_direct_room(&mut tree, &setup).expect("the room is created");
        let me = crate::shell::state::local_user_id_for_room(&room);
        let now = tilo_clock::unix_now();
        {
            let ws = tree
                .desktop
                .workspaces
                .iter()
                .find(|ws| ws.room == room)
                .expect("the room is held");
            assert_eq!(
                ws.role_of_agent(TRIAGE_AGENT),
                Some(collab_types::role::Role::Commenter)
            );
            assert!(ws.may_operate_agent_at(&TEAM.to_string(), TRIAGE_AGENT, now));
            assert!(
                !ws.may_operate_agent_at(&TEAM.to_string(), "recorder", now),
                "the operator grant names exactly the triage agent"
            );
            assert!(
                !ws.may_administer(&TEAM.to_string()),
                "the team operates its agent without governing the room"
            );
        }

        let ws = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|ws| ws.room == room)
            .expect("the room is held");
        let removed = remove_agent(ws, &me, TRIAGE_AGENT);
        assert_eq!(
            removed.revoked.len(),
            3,
            "membership, Chat and the operator grant"
        );
        assert!(!ws.may_operate_agent_at(&TEAM.to_string(), TRIAGE_AGENT, now));

        super::prepare_direct_room(&mut tree, &setup).expect("opened again");
        let ws = tree
            .desktop
            .workspaces
            .iter()
            .find(|ws| ws.room == room)
            .expect("the room is held");
        assert_eq!(ws.role_of_agent(TRIAGE_AGENT), None, "a removal sticks");
        assert!(
            ws.role_of(&TEAM.to_string()).is_some(),
            "the team is still a member"
        );
    }

    /// The team is an Editor of a reporter's direct room, not its
    /// administrator, yet the action broker admits its session launching the
    /// triage agent there because the room named it the agent's operator; no
    /// other agent, and not after the reporter removed it.
    #[test]
    fn the_team_launches_its_triage_agent_in_a_reporters_room_as_its_operator() {
        use crate::shell::state::{RoomId, WorkspaceTree};
        use tilo_action_broker::collab_broker_capnp::{
            admit_agent_delegation_for_test, principal_id_for_root,
        };
        use tilo_desktop::workspace_settings::{TRIAGE_AGENT, remove_agent};
        let team_root = [45; 32];
        let team = principal_id_for_root(team_root);
        let room = RoomId::parse("7a1a9e00-0000-4c0d-9a11-0000000000d2").expect("id");
        let cluster = room.to_string();
        let mut tree = WorkspaceTree::default();
        super::prepare_direct_room(
            &mut tree,
            &super::DirectRoomSetup {
                room,
                other: &team,
                label: "Tilo team",
                governing: true,
                triage: true,
            },
        )
        .expect("the reporter's end creates the room");
        assert!(
            admit_agent_delegation_for_test(&tree.desktop, &cluster, team_root, "scribe")
                .expect("the team is a member")
                .is_err(),
            "only the agent it operates"
        );
        admit_agent_delegation_for_test(&tree.desktop, &cluster, team_root, TRIAGE_AGENT)
            .expect("the team is a member")
            .expect("the operator launches the triage agent");

        let reporter = crate::shell::state::local_user_id_for_room(&room);
        let workspace = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|ws| ws.room == room)
            .expect("the room");
        remove_agent(workspace, &reporter, TRIAGE_AGENT);
        assert!(
            admit_agent_delegation_for_test(&tree.desktop, &cluster, team_root, TRIAGE_AGENT)
                .expect("the team stays a member")
                .is_err(),
            "a removed agent cannot be launched"
        );
    }
}
