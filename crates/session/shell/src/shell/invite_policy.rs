//! Who may invite into a workspace, and through which authority.
//!
//! One decision, asked by every door: the collaborators panel's invite
//! composer (`ShellOverlayHost::may_invite_here`), the palette's
//! `peer.invite*` rows and the issuer itself (`ShellNetwork::issue_invite*`).
//! A second opinion would be a second security boundary, and the two used to
//! disagree: a member holding a delegated `Invite` grant was shown the
//! composer and then refused by the issuer.
//!
//! Permission is the capability policy: `Invite` on the shell's
//! `peer.invite.named` action, which a workspace-wide grant covers too. Which
//! device may sign the invitation is the room's authority: an invitation is
//! issued by whoever governs the room, so a member who may invite but does not
//! govern is sent to the owner rather than minting a token nobody would
//! honour.

use crate::shell::chrome_state::ChromeState;
use crate::shell::native_workspace_owner::NativeWorkspaceOwner;
use crate::shell::state::{RoomId, UserId, Workspace, WorkspaceTree, person_id_of};

/// How this device may issue an invitation into one workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InviteRoute {
    /// The room is governed by a relay of this device's person: that relay
    /// issues the invitation.
    Relay,
    /// This device governs a room no relay holds yet: it shares the room on
    /// its person's home relay, which then issues the invitation.
    ShareOnRelay,
    /// The actor may invite, but another person's device governs the room,
    /// and only that device can sign an invitation it will honour.
    ViaGovernor,
    /// No invitation can be issued from here.
    Refused(InviteRefusal),
}

/// Why [`InviteRoute::Refused`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum InviteRefusal {
    /// The capability policy does not grant the actor `Invite`.
    NotPermitted,
    /// The room is relay-governed but its admitted state is not available on
    /// this device yet, so nobody here can tell who governs it.
    AuthorityUnavailable,
}

impl InviteRoute {
    /// Whether the invite composer should be offered: only when submitting it
    /// can produce an invitation.
    pub(crate) const fn issues_here(self) -> bool {
        matches!(self, Self::Relay | Self::ShareOnRelay)
    }

    /// The error the issuer answers with when it cannot issue, `None` when it
    /// can.
    pub(crate) const fn refusal(self) -> Option<&'static str> {
        match self {
            Self::Relay | Self::ShareOnRelay => None,
            Self::ViaGovernor => Some(
                "only the governing workspace administrator can create invitations here; \
                 ask the workspace owner to invite this person",
            ),
            Self::Refused(InviteRefusal::NotPermitted) => {
                Some("you do not have permission to invite people to this workspace")
            }
            Self::Refused(InviteRefusal::AuthorityUnavailable) => {
                Some("this workspace is still connecting to its owner; try again in a moment")
            }
        }
    }
}

/// Decide whether the person this device acts as in `workspace`'s room,
/// `local_user`, may invite into it, and through which authority, as `tree`
/// knows the room.
///
/// `native_governor` is the user root governing the room's admitted native
/// state, when this device holds it (read by [`native_governor`] outside the
/// tree's lock). The room is this person's own when that root is the
/// identity selected for this room, which may be a workspace-specific
/// identity rather than the device's default person: the identity
/// `ShellNetwork::administered_room` then asks the relay as.
pub(crate) fn may_issue_invitation(
    tree: &WorkspaceTree,
    workspace: &Workspace,
    local_user: &UserId,
    native_governor: Option<[u8; 32]>,
) -> InviteRoute {
    let permitted = workspace.may_govern_at(
        local_user,
        tilo_action_policy::roles::governance::INVITE,
        tilo_clock::unix_now(),
    );
    if !permitted {
        return InviteRoute::Refused(InviteRefusal::NotPermitted);
    }
    let room = workspace.room;
    match native_governor {
        Some(governor) if person_id_of(governor).as_ref() == Some(local_user) => InviteRoute::Relay,
        None if tree.requires_native_owner(room) => {
            InviteRoute::Refused(InviteRefusal::AuthorityUnavailable)
        }
        None if tree.governs_room(room) => InviteRoute::ShareOnRelay,
        // Another person's relay governs the room, or another device the
        // self-governed one.
        Some(_) | None => InviteRoute::ViaGovernor,
    }
}

/// The user root governing `room`'s admitted native state, when `owner`
/// holds it.
///
/// Call it without holding the tree's lock: this takes the owner's lock, and
/// publishing a native commit takes that lock before the tree's writer.
pub(crate) fn native_governor(
    owner: Option<NativeWorkspaceOwner>,
    room: RoomId,
) -> Option<[u8; 32]> {
    owner?
        .with_current(room, |view| Ok(view.governor().user_public_key))
        .ok()
}

/// [`may_issue_invitation`] for the workspace the chrome shows, as the
/// person the chrome acts as in its room. `None` without an active
/// workspace.
pub(crate) fn active_invite_route(state: &ChromeState) -> Option<InviteRoute> {
    let (local, room, owner) = {
        let tree = state.tree();
        let workspace = tree.desktop.active_workspace()?;
        (workspace.id, workspace.room, tree.installed_native_owner())
    };
    let governor = native_governor(owner, room);
    let local_user = state.local_user_for_room(&room);
    let tree = state.tree();
    let route = tree
        .desktop
        .active_workspace()
        .filter(|workspace| workspace.id == local && workspace.room == room)
        .map(|workspace| may_issue_invitation(&tree, workspace, &local_user, governor));
    drop(tree);
    route
}

#[cfg(test)]
pub(crate) mod tests {
    use super::{InviteRoute, may_issue_invitation, native_governor};
    use crate::shell::native_workspace_owner::{test_enrolled_owner, test_tree_of};
    use crate::shell::state::person_id_of;

    /// Grant `person` the room's invite governance action in `workspace`.
    pub(crate) fn grant_invite(workspace: &mut crate::shell::state::Workspace, person: &str) {
        let node = tilo_action_policy::roles::governance_action(
            &workspace.policy.room,
            tilo_action_policy::roles::governance::INVITE,
        );
        workspace.grant(tilo_action_policy::Grant::new(
            format!("invite:{person}"),
            tilo_action_policy::Subject::person(person),
            node,
            tilo_action_policy::KindSet::of(tilo_action_policy::roles::GOVERNANCE_KIND),
            tilo_action_policy::Answer::Allow,
            tilo_action_policy::Subject::person("room-owner"),
        ));
    }

    /// A person who governs a relay room under a workspace-specific identity
    /// issues through that relay, whatever the device's default person is:
    /// the policy compares the governor with the identity this room selects,
    /// the one the relay request is signed with, not with `local_person`.
    /// Another person who merely holds an `Invite` grant is sent to them.
    #[test]
    fn a_workspace_identity_that_governs_the_relay_room_issues_through_it() {
        let (_directory, owner, room, _now, _key) = test_enrolled_owner();
        let governor = native_governor(Some(owner.clone()), room).expect("admitted governor");
        let workspace_identity = person_id_of(governor).expect("person id");
        let member = person_id_of(
            ed25519_dalek::SigningKey::from_bytes(&[64; 32])
                .verifying_key()
                .to_bytes(),
        )
        .expect("person id");
        let handle = test_tree_of(&owner);
        let mut tree = handle.write();
        // The device's default person is somebody other than the identity
        // this workspace selects.
        tree.local_person = Some([0x35; 32]);
        let workspace = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room == room)
            .expect("the enrolled room's workspace");
        for person in [&workspace_identity, &member] {
            grant_invite(workspace, person);
        }
        drop(tree);

        let tree = handle.read();
        let workspace = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room == room)
            .expect("the enrolled room's workspace");
        let routes = [&workspace_identity, &member]
            .map(|person| may_issue_invitation(&tree, workspace, person, Some(governor)));
        drop(tree);

        assert_eq!(
            routes,
            [InviteRoute::Relay, InviteRoute::ViaGovernor],
            "the governing workspace identity issues through its relay; a \
             delegated inviter is sent to the governor"
        );
    }
}
