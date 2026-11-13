//! A workspace's door: who it lets in, and where it is listed.
//!
//! The door is one of the workspace's settings, not a host's: a relay serves a
//! public workspace, and a person's own machine holds the same workspace, so
//! both read the same record and decide alike. Whoever may change the
//! workspace's settings ([`crate::roles::governance::SETTINGS`]) makes the
//! change once; every host reading the workspace then answers its door the
//! same way, including after a restart.

use serde::{Deserialize, Serialize};

use crate::Role;

/// The longest description a listing carries, in characters: what a
/// directory shows of a workspace, in a sentence or two.
pub const MAX_DESCRIPTION_CHARS: usize = 280;

/// A workspace as a directory lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListingPolicy {
    /// What the workspace is, in a sentence or two.
    pub description: String,
    /// The role everyone who joins through the door holds to begin with.
    pub default_role: Role,
}

/// Why a door's setting cannot stand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DoorError {
    /// The description is longer than [`MAX_DESCRIPTION_CHARS`] or holds a
    /// control character other than a line break.
    Description,
    /// Moderators are chosen, never handed out at the door.
    Role,
    /// A wait list is only kept by a door that takes people by invitation.
    WaitList,
}

impl std::fmt::Display for DoorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Description => write!(
                f,
                "a description is at most {MAX_DESCRIPTION_CHARS} characters of text"
            ),
            Self::Role => {
                f.write_str("people who join on their own become viewers, commenters or editors")
            }
            Self::WaitList => {
                f.write_str("only a door that takes people by invitation keeps a wait list")
            }
        }
    }
}

impl std::error::Error for DoorError {}

/// What a workspace's door decides.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DoorPolicy {
    /// Listed in a directory, where anyone who asks gets in at
    /// [`ListingPolicy::default_role`]. Unlisted, the door admits only the
    /// people the workspace has in it, invited, or pre-admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing: Option<ListingPolicy>,
    /// The door admits only people it invited or pre-admitted: a listed
    /// workspace takes nobody who simply asks.
    #[serde(default, skip_serializing_if = "is_false")]
    pub invitation_only: bool,
    /// How many requests left at the door it keeps for whoever may invite.
    /// `0`: the door keeps none.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub wait_list_max: usize,
}

impl DoorPolicy {
    /// Whether the workspace is public: a directory lists it.
    #[must_use]
    pub const fn is_listed(&self) -> bool {
        self.listing.is_some()
    }

    /// Whether the door keeps the addresses people leave when it turns them
    /// away.
    #[must_use]
    pub const fn keeps_requests(&self) -> bool {
        self.invitation_only && self.wait_list_max > 0
    }

    /// The door with its listing's description trimmed, when it may stand.
    ///
    /// # Errors
    ///
    /// [`DoorError`]: what a directory cannot show, a role the door may not
    /// hand out, or a wait list no door takes people by invitation for.
    pub fn checked(self) -> Result<Self, DoorError> {
        let listing = match self.listing {
            Some(listing) => {
                let description = listing.description.trim().to_owned();
                if description.chars().count() > MAX_DESCRIPTION_CHARS
                    || description.chars().any(|c| c.is_control() && c != '\n')
                {
                    return Err(DoorError::Description);
                }
                if matches!(listing.default_role, Role::Moderator | Role::Owner) {
                    return Err(DoorError::Role);
                }
                Some(ListingPolicy {
                    description,
                    default_role: listing.default_role,
                })
            }
            None => None,
        };
        if self.wait_list_max > 0 && !self.invitation_only {
            return Err(DoorError::WaitList);
        }
        Ok(Self { listing, ..self })
    }
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if takes the field by reference"
)]
const fn is_false(value: &bool) -> bool {
    !*value
}

#[expect(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if takes the field by reference"
)]
const fn is_zero(value: &usize) -> bool {
    *value == 0
}
