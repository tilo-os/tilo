//! Why a host turned something away, typed.
//!
//! Every refusal a host sends ([`crate::transport::Envelope::CommitRefused`],
//! [`crate::transport::Envelope::NotAuthorised`],
//! [`crate::transport::Envelope::RemoteActionUndeliverable`],
//! [`crate::transport::Envelope::RecordContentRefused`]) carries one
//! [`Refusal`]. A device decides what to do by its variant: wait for the
//! workspace to be ready, wait the time it says, ask the person, or give up.
//! A person reads [`Refusal::plain`]; a log reads its [`Display`](std::fmt::Display).

use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::time::Duration;

/// Why a host turned a change, a subscription, an action or a piece of
/// content away.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Refusal {
    /// The host had not taken the workspace up for this session yet: the
    /// session had not subscribed to it, the workspace was not open here, or
    /// its admission was not installed. The same thing goes through once the
    /// host says the workspace is ready ([`crate::transport::Envelope::Ready`]).
    NotReady,
    /// The member change budget is spent: the next change is taken after
    /// `wait_ms`.
    TooFast { wait_ms: u64 },
    /// The member content budget is spent: content is taken again after
    /// `wait_ms`.
    TooMuchContent { wait_ms: u64 },
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
    /// A newer connection of the same device took over: what this one asked
    /// is asked again on that one.
    Superseded,
    /// The request is malformed: what is wrong with it.
    Invalid(String),
    /// The operation's id was already used for different content.
    Conflict,
    /// Anything else: the host's own words, for the log.
    Other(String),
}

/// Sending too fast: the change budget refused it.
pub const PLAIN_TOO_FAST: &str = "You're sending too fast; slow down a moment.";
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
    /// The refusal of a change budget that admits the next change after
    /// `wait`.
    #[must_use]
    pub fn too_fast(wait: Duration) -> Self {
        Self::TooFast {
            wait_ms: millis(wait),
        }
    }

    /// The refusal of a content budget that holds content again after
    /// `wait`.
    #[must_use]
    pub fn too_much_content(wait: Duration) -> Self {
        Self::TooMuchContent {
            wait_ms: millis(wait),
        }
    }

    /// How long to wait before the same thing goes through, when the
    /// refusal says.
    #[must_use]
    pub const fn wait(&self) -> Option<Duration> {
        match self {
            Self::TooFast { wait_ms } | Self::TooMuchContent { wait_ms } => {
                Some(Duration::from_millis(*wait_ms))
            }
            _ => None,
        }
    }

    /// Whether the same thing goes through once the workspace is ready on
    /// the host ([`Self::NotReady`]) or once another connection carries it
    /// ([`Self::Superseded`]): nothing is wrong with it.
    #[must_use]
    pub const fn passes_once_ready(&self) -> bool {
        matches!(self, Self::NotReady | Self::Superseded)
    }

    /// The refusal's stable machine-readable code, as a CLI, MCP or SDK
    /// caller matches on it.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::NotReady => "not_ready",
            Self::TooFast { .. } => "rate_limited",
            Self::TooMuchContent { .. } => "content_limited",
            Self::Full => "storage_full",
            Self::NotGranted(_) => "not_granted",
            Self::NotAllowed(_) => "not_allowed",
            Self::NotAMember(_) => "not_a_member",
            Self::NotAdmitted => "not_admitted",
            Self::WorkspaceDeleted => "workspace_deleted",
            Self::Offline => "offline",
            Self::Superseded => "superseded",
            Self::Invalid(_) => "invalid_request",
            Self::Conflict => "conflict",
            Self::Other(_) => "refused",
        }
    }

    /// What a person is told.
    #[must_use]
    pub fn plain(&self) -> Cow<'static, str> {
        match self {
            Self::TooFast { .. } => PLAIN_TOO_FAST.into(),
            Self::TooMuchContent { wait_ms } => {
                plain_too_much(Duration::from_millis(*wait_ms)).into()
            }
            Self::Full => PLAIN_FULL.into(),
            Self::NotGranted(_) => PLAIN_NOT_GRANTED.into(),
            Self::NotAllowed(_) => PLAIN_NOT_ALLOWED.into(),
            Self::NotAMember(_) | Self::NotAdmitted => PLAIN_NOT_A_MEMBER.into(),
            Self::WorkspaceDeleted => PLAIN_DELETED.into(),
            Self::NotReady
            | Self::Offline
            | Self::Superseded
            | Self::Invalid(_)
            | Self::Conflict
            | Self::Other(_) => PLAIN_REFUSED.into(),
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotReady => f.write_str("the workspace is not ready on this host yet"),
            Self::TooFast { wait_ms } => {
                write!(f, "too many changes; slow down; retry in {wait_ms} ms")
            }
            Self::TooMuchContent { wait_ms } => {
                write!(f, "too much content this hour; retry in {wait_ms} ms")
            }
            Self::Full => f.write_str("the account's storage is full"),
            Self::NotGranted(detail) => write!(f, "not granted: {detail}"),
            Self::NotAllowed(detail) => write!(f, "not allowed: {detail}"),
            Self::NotAMember(detail) => write!(f, "not a member: {detail}"),
            Self::NotAdmitted => f.write_str("not a member here"),
            Self::WorkspaceDeleted => f.write_str("workspace deleted"),
            Self::Offline => f.write_str("the device is not connected to the workspace"),
            Self::Superseded => f.write_str("a newer connection of this device took over"),
            Self::Invalid(detail) => write!(f, "invalid request: {detail}"),
            Self::Conflict => f.write_str("operation id already committed with different content"),
            Self::Other(detail) => f.write_str(detail),
        }
    }
}

/// The hourly content budget refused it, and holds it again after `wait`:
/// said in whole minutes, rounded up, so the person never tries too early.
#[must_use]
pub fn plain_too_much(wait: Duration) -> String {
    let minutes = wait.as_secs().div_ceil(60).max(1);
    if minutes == 1 {
        "You've shared too much this hour. Try again in a minute.".to_owned()
    } else {
        format!("You've shared too much this hour. Try again in {minutes} minutes.")
    }
}

fn millis(wait: Duration) -> u64 {
    u64::try_from(wait.as_millis()).unwrap_or(u64::MAX).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn refusals_read_as_what_the_reader_should_do;

        phase budgets_say_their_wait;
        {
            let fast = Refusal::too_fast(Duration::from_millis(480));
            assert_eq!(fast.wait(), Some(Duration::from_millis(480)));
            assert_eq!(fast.code(), "rate_limited");
            assert_eq!(fast.plain(), PLAIN_TOO_FAST);
            let content = Refusal::too_much_content(Duration::from_secs(19 * 60 + 1));
            assert_eq!(
                content.plain(),
                "You've shared too much this hour. Try again in 20 minutes."
            );
            assert_eq!(
                Refusal::too_much_content(Duration::from_millis(1)).plain(),
                "You've shared too much this hour. Try again in a minute."
            );
        }

        phase only_a_host_that_was_not_ready_passes_later;
        {
            assert!(Refusal::NotReady.passes_once_ready());
            assert!(Refusal::Superseded.passes_once_ready());
            assert!(!Refusal::Conflict.passes_once_ready());
            assert!(!Refusal::too_fast(Duration::from_secs(1)).passes_once_ready());
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
                Refusal::too_fast(Duration::from_millis(3)),
                Refusal::NotGranted("the author is not granted Undoable".into()),
                Refusal::Other("something".into()),
            ] {
                let bytes = postcard::to_allocvec(&refusal).unwrap();
                assert_eq!(postcard::from_bytes::<Refusal>(&bytes).unwrap(), refusal);
            }
        }
    }
}
