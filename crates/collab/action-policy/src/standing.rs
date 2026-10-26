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

impl Bound {
    /// The delegation `person` gives `subject`: the grants of `preset`,
    /// granted by the person. The delegate acts with the person's authority
    /// bounded by them ([`crate::Authority::bounded_by`]): never more than
    /// the person, never more than the preset.
    #[must_use]
    pub fn delegation(subject: Subject, preset: &roles::Preset, person: &str) -> Self {
        let grants = preset.assign(
            &format!("delegation:{}", subject_key(&subject)),
            &subject,
            &Subject::Person(person.to_owned()),
            &crate::Lifetime::UntilRevoked,
        );
        Self { subject, grants }
    }

    /// The party this delegation stands for in a room whose own grants are
    /// `room_grants`: its own grants and the room's grants naming its
    /// subject.
    #[must_use]
    pub fn party<'a>(&'a self, room_grants: &'a [Grant]) -> Party<'a> {
        Party::new(vec![self.subject.clone()], room_grants).with_grants(&self.grants)
    }

    /// [`Self::party`] with the delegation's own grants that have run out by
    /// `cx` left out, so a standing projected before the instant it is
    /// decided at answers as one built at that instant does.
    #[must_use]
    pub fn party_at<'a>(&'a self, room_grants: &'a [Grant], cx: &Context<'_>) -> Party<'a> {
        self.party(room_grants)
            .with_grants(self.grants.iter().filter(|grant| grant.is_live(cx)))
    }
}

/// `subject` as one token of a grant id.
fn subject_key(subject: &Subject) -> String {
    match subject {
        Subject::Person(id) => format!("person:{id}"),
        Subject::Role(role) => format!("role:{}", role.name().to_ascii_lowercase()),
        Subject::Agent(id) => format!("agent:{id}"),
        Subject::Task(id) => format!("task:{id}"),
        Subject::Shortcut(id) => format!("shortcut:{id}"),
        Subject::App(id) => format!("app:{id}"),
        Subject::Bot(id) => format!("bot:{id}"),
        Subject::ForeignDevice => "foreign-device".to_owned(),
    }
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
    pub fn bounded_by(self, subject: Subject, grants: Vec<Grant>) -> Self {
        self.with_bound(Bound { subject, grants })
    }

    /// The person acting through `bound`, such as
    /// [`roles::agent_delegation`].
    #[must_use]
    pub fn with_bound(mut self, bound: Bound) -> Self {
        self.bounds.push(bound);
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

    #[must_use]
    pub fn room(&self) -> &str {
        &self.policy.room
    }

    /// `relative` (a path below the room) as a node of this room.
    #[must_use]
    pub fn node(&self, relative: &TreePath) -> TreePath {
        self.policy.node().joined(relative)
    }

    /// This standing as it answers when the person it is about invokes a
    /// change *through* `subject`, a delegation of theirs: the person's
    /// grants in the room, bounded by everything bounding them, and bounded
    /// by that delegation's grants.
    ///
    /// A person editing a document is decided by this standing as it is
    /// (their own role); an agent or a bot doing the same is decided by this
    /// one. Ids naming nobody in the room simply bound it away.
    #[must_use]
    pub fn through(&self, subject: Subject) -> Self {
        if self.is_bounded_by(&subject) {
            return self.clone();
        }
        let grants: Vec<Grant> = self
            .policy
            .grants
            .iter()
            .filter(|grant| grant.subject == subject)
            .cloned()
            .collect();
        self.clone().with_bound(Bound { subject, grants })
    }

    /// The authority this standing confers, every bound as it stands now.
    #[must_use]
    pub fn authority(&self) -> Authority<'_> {
        self.authority_at(&Context::at(u64::MAX))
    }

    /// The authority this standing confers at `cx`.
    #[must_use]
    pub fn authority_at(&self, cx: &Context<'_>) -> Authority<'_> {
        let mut authority = Authority::person(self.policy.party(&self.person));
        for bound in &self.bounds {
            authority = authority.bounded_by(bound.party_at(&self.policy.grants, cx));
        }
        authority
    }

    #[must_use]
    pub fn verdict(&self, node: &TreePath, kind: ActionKind, cx: &Context<'_>) -> Verdict<'_> {
        self.authority_at(cx).decide(node, kind, cx)
    }

    #[must_use]
    pub fn answer(&self, node: &TreePath, kind: ActionKind, cx: &Context<'_>) -> Answer {
        self.verdict(node, kind, cx).answer
    }
}
