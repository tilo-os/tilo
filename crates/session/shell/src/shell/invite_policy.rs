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

use crate::shell::search::ResourceKind;
use crate::shell::search::action_api::{ActionVisibility as _, ResourceRef};
use crate::shell::search::visibility::CapabilityVisibility;
use crate::shell::state::{Actor, Workspace, WorkspaceTree};

/// The shell action whose capability gates every invitation.
pub(crate) const INVITE_ACTION: &str = "peer.invite.named";

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

/// Decide whether `actor` may invite into `workspace`, and through which
/// authority, as `tree` knows the room.
pub(crate) fn may_issue_invitation(
    tree: &WorkspaceTree,
    workspace: &Workspace,
    actor: Actor,
) -> InviteRoute {
    let permitted = CapabilityVisibility::new(workspace, actor).can_see(
        &ResourceRef::new("actions", INVITE_ACTION),
        &ResourceKind::Action,
    );
    if !permitted {
        return InviteRoute::Refused(InviteRefusal::NotPermitted);
    }
    let room = workspace.room;
    let native_governor = tree.installed_native_owner().and_then(|owner| {
        owner
            .with_current(room, |view| Ok(view.governor().user_public_key))
            .ok()
    });
    match native_governor {
        Some(governor) if tree.local_person == Some(governor) => InviteRoute::Relay,
        Some(_) => InviteRoute::ViaGovernor,
        None if tree.requires_native_owner(room) => {
            InviteRoute::Refused(InviteRefusal::AuthorityUnavailable)
        }
        None if tree.governs_room(room) => InviteRoute::ShareOnRelay,
        None => InviteRoute::ViaGovernor,
    }
}
