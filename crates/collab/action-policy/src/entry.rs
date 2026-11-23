//! A workspace's entry: who it lets in, and where it is listed.
//!
//! The entry is one of the workspace's settings, not a host's: a server serves a
//! public workspace, and a person's own machine holds the same workspace, so
//! both read the same resource and decide alike. Whoever may change the
//! workspace's settings ([`crate::roles::governance::SETTINGS`]) makes the
//! change once; every host reading the workspace then answers its entry the
//! same way, including after a restart.

use serde::{Deserialize, Serialize};

use crate::{ActionKind, Answer, Grant, GrantId, Grantee, KindSet, Lifetime, Role, TreePath};

/// The app that keeps what visitors leave at a workspace's entry: one resource per address left
/// there by someone asking to be let in.
pub const VISITOR_ENTRY_APP: &str = "so.tilo.lang.entry";

/// What a visitor may do at the entry, and nothing else of it: leave an address, confirm it from
/// the link mailed there, and take it off again.
pub const VISITOR_ENTRY_ACTIONS: [&str; 3] = ["join", "confirm", "leave"];

/// The longest description a listing carries, in characters: what a
/// directory shows of a workspace, in a sentence or two.
pub const MAX_DESCRIPTION_CHARS: usize = 280;

/// A workspace as a directory lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ListingPolicy {
    /// What the workspace is, in a sentence or two.
    pub description: String,
    /// The role everyone who joins through the entry holds to begin with.
    pub default_role: Role,
}

/// Why an entry's setting cannot stand.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EntryError {
    /// The description is longer than [`MAX_DESCRIPTION_CHARS`] or holds a
    /// control character other than a line break.
    Description,
    /// Moderators are chosen, never handed out at the entry.
    Role,
    /// A wait list is only kept by an entry that takes people by invitation.
    WaitList,
}

impl std::fmt::Display for EntryError {
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
                f.write_str("only an entry that takes people by invitation keeps a wait list")
            }
        }
    }
}

impl std::error::Error for EntryError {}

/// What a workspace's entry decides.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryPolicy {
    /// Listed in a directory, where anyone who asks gets in at
    /// [`ListingPolicy::default_role`]. Unlisted, the entry admits only the
    /// people the workspace has in it, invited, or pre-admitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing: Option<ListingPolicy>,
    /// The entry admits only people it invited or pre-admitted: a listed
    /// workspace takes nobody who simply asks.
    #[serde(default, skip_serializing_if = "is_false")]
    pub invitation_only: bool,
    /// How many requests left at the entry it keeps for whoever may invite.
    /// `0`: the entry keeps none.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub wait_list_max: usize,
}

impl EntryPolicy {
    /// Whether the workspace is public: a directory lists it.
    #[must_use]
    pub const fn is_listed(&self) -> bool {
        self.listing.is_some()
    }

    /// Whether the entry keeps the addresses people leave when it turns them
    /// away.
    #[must_use]
    pub const fn keeps_requests(&self) -> bool {
        self.invitation_only && self.wait_list_max > 0
    }

    /// What the entry opens to any key ([`Grantee::AnyKey`]) in `workspace`: on an entry that
    /// keeps requests, leaving an address there, confirming it and taking it off. Nothing on any other
    /// entry. Every device derives these from the entry alike, so a visitor's call is admitted,
    /// and its change taken in, by the same grants everywhere.
    #[must_use]
    pub fn visitor_grants(&self, workspace: &str) -> Vec<Grant> {
        if !self.keeps_requests() {
            return Vec::new();
        }
        VISITOR_ENTRY_ACTIONS
            .into_iter()
            .map(|action| Grant {
                id: GrantId::new(format!("entry:visitor:{action}")),
                grantee: Grantee::AnyKey,
                node: TreePath::workspace(workspace)
                    .app(VISITOR_ENTRY_APP)
                    .rest()
                    .action(action),
                kinds: KindSet::of(ActionKind::Read).with(ActionKind::Undoable),
                answer: Answer::Allow,
                lifetime: Lifetime::UntilRevoked,
                granted_by: Grantee::role(Role::Owner),
            })
            .collect()
    }

    /// The entry with its listing's description trimmed, when it may stand.
    ///
    /// # Errors
    ///
    /// [`EntryError`]: what a directory cannot show, a role the entry may not
    /// hand out, or a wait list no entry takes people by invitation for.
    pub fn checked(self) -> Result<Self, EntryError> {
        let listing = match self.listing {
            Some(listing) => {
                let description = listing.description.trim().to_owned();
                if description.chars().count() > MAX_DESCRIPTION_CHARS
                    || description.chars().any(|c| c.is_control() && c != '\n')
                {
                    return Err(EntryError::Description);
                }
                if !Role::Editor.includes(listing.default_role) {
                    return Err(EntryError::Role);
                }
                Some(ListingPolicy {
                    description,
                    default_role: listing.default_role,
                })
            }
            None => None,
        };
        if self.wait_list_max > 0 && !self.invitation_only {
            return Err(EntryError::WaitList);
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
