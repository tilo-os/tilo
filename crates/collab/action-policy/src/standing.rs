//! One actor's standing in one workspace, as owned data a host hands to a door or
//! an app: the person, the workspace's policy, and the delegations bounding them.

use serde::{Deserialize, Serialize};

use crate::{
    ActionKind, Answer, Authority, Context, Grant, Subject, TreePath, Verdict, WorkspacePolicy,
    roles,
};

/// A delegation bounding a person: its subject and the grants it carries
/// itself (an agent delegation's, a Shortcut's). The workspace's own grants naming
/// the subject count too.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bound {
    pub subject: Subject,
    #[serde(default)]
    pub grants: Vec<Grant>,
    /// Whom the delegate is known as too: an agent's run of an issue is the
    /// agent and the issue, so grants naming either decide for it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also: Vec<Subject>,
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
        Self {
            subject,
            grants,
            also: Vec::new(),
        }
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

/// Who is acting in a workspace and everything their authority depends on.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Standing {
    pub person: String,
    pub policy: WorkspacePolicy,
    #[serde(default)]
    pub bounds: Vec<Bound>,
}

impl Standing {
    /// A person acting as themselves.
    #[must_use]
    pub fn person(policy: WorkspacePolicy, person: impl Into<String>) -> Self {
        Self {
            person: person.into(),
            policy,
            bounds: Vec::new(),
        }
    }

    /// The person acting through a delegation.
    #[must_use]
    pub fn bounded_by(self, subject: Subject, grants: Vec<Grant>) -> Self {
        self.with_bound(Bound {
            subject,
            grants,
            also: Vec::new(),
        })
    }

    /// The person acting through `bound`, such as
    /// [`roles::agent_turn`].
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
    pub fn workspace(&self) -> &str {
        &self.policy.workspace
    }

    /// `relative` (a path below the workspace) as a node of this workspace.
    #[must_use]
    pub fn node(&self, relative: &TreePath) -> TreePath {
        self.policy.node().joined(relative)
    }

    /// The authority this standing confers.
    #[must_use]
    pub fn authority(&self) -> Authority<'_> {
        crate::invoker::authority_of(&self.policy, &self.person, &self.bounds)
    }

    #[must_use]
    pub fn verdict(&self, node: &TreePath, kind: ActionKind, cx: &Context<'_>) -> Verdict<'_> {
        self.authority().decide(node, kind, cx)
    }

    #[must_use]
    pub fn answer(&self, node: &TreePath, kind: ActionKind, cx: &Context<'_>) -> Answer {
        self.verdict(node, kind, cx).answer
    }
}
