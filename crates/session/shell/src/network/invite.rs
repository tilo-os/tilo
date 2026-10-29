//! Invitations into a workspace's room: the role one grants, and the
//! enrollment that redeems one at the room's host. Every invitation is the
//! host's own (`tilo-workspace-join`).

pub(crate) mod enrollment_driver;
pub(crate) use tilo_peer_state::owner::{VerifiedNativeEnrollment, VerifiedNativeEnrollmentIntent};

use serde::{Deserialize, Serialize};

/// Authority carried by an invitation.  Owner is deliberately absent: a
/// bearer invite may add a collaborator, never transfer workspace ownership.
#[derive(Serialize, Deserialize, Debug, Default, PartialEq, Eq, Clone, Copy)]
#[serde(rename_all = "snake_case")]
pub enum InviteRole {
    Viewer,
    Commenter,
    #[default]
    Editor,
}

impl InviteRole {
    /// The room role an accepted invitation admits its holder with.
    #[must_use]
    pub fn role(self) -> crate::shell::state::CapRole {
        match self {
            Self::Viewer => crate::shell::state::CapRole::Viewer,
            Self::Commenter => crate::shell::state::CapRole::Commenter,
            Self::Editor => crate::shell::state::CapRole::Editor,
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Viewer => "Viewer",
            Self::Commenter => "Commenter",
            Self::Editor => "Editor",
        }
    }

    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Viewer => Self::Commenter,
            Self::Commenter => Self::Editor,
            Self::Editor => Self::Viewer,
        }
    }
}
