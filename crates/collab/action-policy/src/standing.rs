//! One actor's standing in one room, as owned data a host hands to a door or
//! an app: the person, the room's policy, and the delegations bounding them.

use serde::{Deserialize, Serialize};

use crate::{
    ActionKind, Answer, Authority, Context, Grant, Party, RoomPolicy, Subject, TreePath, Verdict,
    roles,
};

/// A delegation bounding a person: its subject and the grants it carries
/// itself (an agent delegation's, a Shortcut's). The room's own grants naming
/// the subject count too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bound {
    pub subject: Subject,
    #[serde(default)]
    pub grants: Vec<Grant>,
}

/// Who is acting in a room and everything their authority depends on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standing {
    pub person: String,
    pub policy: RoomPolicy,
    #[serde(default)]
    pub bounds: Vec<Bound>,
}

impl Standing {
    /// A person acting as themselves.
    #[must_use]
    pub fn person(policy: RoomPolicy, person: impl Into<String>) -> Self {
        Self {
            person: person.into(),
            policy,
            bounds: Vec::new(),
        }
    }

    /// The person acting through a delegation.
    #[must_use]
    pub fn bounded_by(mut self, subject: Subject, grants: Vec<Grant>) -> Self {
        self.bounds.push(Bound { subject, grants });
        self
    }

    /// The person working from a device that is not theirs.
    #[must_use]
    pub fn on_foreign_device(self) -> Self {
        self.bounded_by(Subject::ForeignDevice, roles::foreign_device_bound())
    }

    /// Whether a delegation of `subject` bounds this standing.
    #[must_use]
    pub fn is_bounded_by(&self, subject: &Subject) -> bool {
        self.bounds.iter().any(|bound| &bound.subject == subject)
    }

    /// The room this standing is in.
    #[must_use]
    pub fn room(&self) -> &str {
        &self.policy.room
    }

    /// `relative` (a path below the room) as a node of this room.
    #[must_use]
    pub fn node(&self, relative: &TreePath) -> TreePath {
        self.policy.node().joined(relative)
    }

    /// The authority this standing confers.
    #[must_use]
    pub fn authority(&self) -> Authority<'_> {
        let mut authority = Authority::person(self.policy.party(&self.person));
        for bound in &self.bounds {
            authority = authority.bounded_by(
                Party::new(vec![bound.subject.clone()], &self.policy.grants)
                    .with_grants(&bound.grants),
            );
        }
        authority
    }

    /// The verdict for an action of `kind` at `node`.
    #[must_use]
    pub fn verdict(&self, node: &TreePath, kind: ActionKind, cx: &Context<'_>) -> Verdict<'_> {
        self.authority().decide(node, kind, cx)
    }

    /// The answer for an action of `kind` at `node`.
    #[must_use]
    pub fn answer(&self, node: &TreePath, kind: ActionKind, cx: &Context<'_>) -> Answer {
        self.verdict(node, kind, cx).answer
    }
}
