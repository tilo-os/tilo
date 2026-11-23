//! Why a host turned something away, typed.
//!
//! Every refusal a host sends ([`crate::transport::Envelope::CommitRefused`],
//! [`crate::transport::Envelope::NotAuthorised`],
//! [`crate::transport::Envelope::RemoteActionUndeliverable`]) carries one
//! [`Refusal`]. A device decides what to do by its variant: wait for the
//! workspace to be ready, ask the person, or give up.
//! A person reads [`Refusal::plain`]; a log reads its [`Display`](std::fmt::Display).

use serde::{Deserialize, Serialize};
use std::borrow::Cow;

use tilo_user_identity::RevocationEntry;

/// Why a host turned a change, a subscription or an action away.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    /// The host had not taken the workspace up for this session yet: the
    /// session had not subscribed to it, the workspace was not open here, or
    /// its admission was not installed. The same thing goes through once the
    /// host says the workspace is ready ([`crate::transport::Envelope::Ready`]).
    NotReady,
    /// The person's own workspaces keep as much as the host keeps for them:
    /// the change was not kept.
    Full,
    /// A grant the person does not hold, which a workspace owner can give.
    NotGranted(String),
    /// The workspace's rules or its app's surface do not allow it.
    NotAllowed(String),
    /// The person is no longer a member, or this device no longer counts as
    /// one.
    NotAMember(String),
    /// The host keeps nothing of the device's person: its state was reset,
    /// or it pruned them. The person joins again through the host's entry.
    NotAdmitted,
    /// The workspace's owner deleted it: a device forgets it rather than
    /// tries again.
    WorkspaceDeleted,
    /// The device an action was addressed to is not connected to the
    /// workspace.
    Offline,
    /// The request is malformed: what is wrong with it.
    Invalid(String),
    /// The operation's id was already used for different content.
    Conflict,
    /// Anything else: the host's own words, for the log.
    Other(String),
    /// The root of the person whose workspace this is retired the dialling
    /// device's key: the root-signed entry that says so. A device it is sent to
    /// keeps the entry as an announced revocation, which its root signed, so it
    /// refuses its own key everywhere it is due rather than only at the link
    /// that said it. A key lost, stolen or compromised is not told this: the
    /// entry names why, and the reason is the person's. Kept last: a refusal's
    /// variant index is its wire form.
    Revoked(Box<RevocationEntry>),
}

/// A person's own workspace keeps as much as the host keeps for them, so the
/// change was not kept: make room, then send it again.
pub const PLAIN_FULL: &str = "Your workspace is full, so this change wasn't kept. Remove something to make room, then try again.";
/// The workspace's grants or its app's surface do not allow the change.
pub const PLAIN_NOT_ALLOWED: &str = "This workspace doesn't allow that.";
/// The same, where the reason is a grant the person does not hold: a
/// workspace owner can give it.
pub const PLAIN_NOT_GRANTED: &str =
    "This workspace doesn't allow that. Ask a workspace owner for access.";
/// The person is no longer a member, or this device no longer counts as one.
pub const PLAIN_NOT_A_MEMBER: &str = "You're no longer a member of this workspace.";
/// The workspace is gone.
pub const PLAIN_DELETED: &str = "This workspace was deleted.";
/// Anything else.
pub const PLAIN_REFUSED: &str = "The workspace didn't accept this change.";

impl Refusal {
    /// Whether the same thing goes through once the workspace is ready on
    /// the host ([`Self::NotReady`]): nothing is wrong with it.
    #[must_use]
    pub const fn passes_once_ready(&self) -> bool {
        matches!(self, Self::NotReady)
    }

    /// The refusal's stable machine-readable code, as a CLI, MCP or SDK
    /// caller matches on it.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotReady => "not_ready",
            Self::Full => "storage_full",
            Self::NotGranted(_) => "not_granted",
            Self::NotAllowed(_) => "not_allowed",
            Self::NotAMember(_) | Self::Revoked(_) => "not_a_member",
            Self::NotAdmitted => "not_admitted",
            Self::WorkspaceDeleted => "workspace_deleted",
            Self::Offline => "offline",
            Self::Invalid(_) => "invalid_request",
            Self::Conflict => "conflict",
            Self::Other(_) => "refused",
        }
    }

    /// What a person is told.
    #[must_use]
    pub fn plain(&self) -> Cow<'static, str> {
        match self {
            Self::Full => PLAIN_FULL.into(),
            Self::NotGranted(_) => PLAIN_NOT_GRANTED.into(),
            Self::NotAllowed(_) => PLAIN_NOT_ALLOWED.into(),
            Self::NotAMember(_) | Self::Revoked(_) | Self::NotAdmitted => PLAIN_NOT_A_MEMBER.into(),
            Self::WorkspaceDeleted => PLAIN_DELETED.into(),
            Self::NotReady | Self::Offline | Self::Invalid(_) | Self::Conflict | Self::Other(_) => {
                PLAIN_REFUSED.into()
            }
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotReady => f.write_str("the workspace is not ready on this host yet"),
            Self::Full => f.write_str("the account's storage is full"),
            Self::NotGranted(detail) => write!(f, "not granted: {detail}"),
            Self::NotAllowed(detail) => write!(f, "not allowed: {detail}"),
            Self::NotAMember(detail) => write!(f, "not a member: {detail}"),
            Self::Revoked(entry) => write!(
                f,
                "the device was retired by its person's root at {}",
                entry.revoked_at_ns
            ),
            Self::NotAdmitted => f.write_str("not a member here"),
            Self::WorkspaceDeleted => f.write_str("workspace deleted"),
            Self::Offline => f.write_str("the device is not connected to the workspace"),
            Self::Invalid(detail) => write!(f, "invalid request: {detail}"),
            Self::Conflict => f.write_str("operation id already committed with different content"),
            Self::Other(detail) => f.write_str(detail),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn refusals_read_as_what_the_reader_should_do;

        phase only_a_host_that_was_not_ready_passes_later;
        {
            assert!(Refusal::NotReady.passes_once_ready());
            assert!(!Refusal::Conflict.passes_once_ready());
        }

        phase a_person_reads_what_to_do;
        {
            assert_eq!(Refusal::Full.plain(), PLAIN_FULL);
            assert_eq!(Refusal::NotGranted("x".into()).plain(), PLAIN_NOT_GRANTED);
            assert_eq!(Refusal::NotAllowed("x".into()).plain(), PLAIN_NOT_ALLOWED);
            assert_eq!(Refusal::NotAMember("x".into()).plain(), PLAIN_NOT_A_MEMBER);
            assert_eq!(Refusal::Other("new".into()).plain(), PLAIN_REFUSED);
        }

        phase it_travels_whole;
        {
            for refusal in [
                Refusal::NotReady,
                Refusal::NotGranted("the author is not granted Undoable".into()),
                Refusal::Other("something".into()),
            ] {
                let bytes = postcard::to_allocvec(&refusal).unwrap();
                assert_eq!(postcard::from_bytes::<Refusal>(&bytes).unwrap(), refusal);
            }
        }
    }
}
