//! Grants: who, which node, which kinds, what answer, for how long.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::{KindSet, TreePath};

/// The answer a grant gives. Ordered `Deny < Ask < Allow`, so the stricter of
/// two answers is their minimum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    /// Refuse.
    Deny,
    /// Pause for the person's approval, witnessed by the host on their device.
    Ask,
    /// Run.
    Allow,
}

impl Answer {
    /// The answer as people read it in a grant sentence.
    #[must_use]
    pub const fn phrase(self) -> &'static str {
        match self {
            Self::Deny => "not allowed",
            Self::Ask => "ask first",
            Self::Allow => "allowed",
        }
    }
}

impl fmt::Display for Answer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Deny => "deny",
            Self::Ask => "ask",
            Self::Allow => "allow",
        })
    }
}

/// A role preset. A member of a room holds one; the room keeps the grants each
/// role carries (see [`crate::roles`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Viewer,
    Commenter,
    Editor,
    Owner,
}

impl Role {
    pub const ALL: [Self; 4] = [Self::Viewer, Self::Commenter, Self::Editor, Self::Owner];

    /// Whether a holder of `self` may hand out `other` through an invite: an
    /// invite never carries more than its inviter's preset.
    #[must_use]
    pub const fn includes(self, other: Self) -> bool {
        self.rank() >= other.rank()
    }

    const fn rank(self) -> u8 {
        match self {
            Self::Viewer => 0,
            Self::Commenter => 1,
            Self::Editor => 2,
            Self::Owner => 3,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Viewer => "Viewer",
            Self::Commenter => "Commenter",
            Self::Editor => "Editor",
            Self::Owner => "Owner",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Whom a grant is for.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Subject {
    /// A person, by the lowercase hex of their user root key.
    Person(String),
    /// Every member holding a role preset.
    Role(Role),
    /// An agent acting for a person.
    Agent(String),
    /// A task an agent works on.
    Task(String),
    /// A Shortcut run for a person.
    Shortcut(String),
    /// An app acting under its own identity.
    App(String),
    /// A person working from a device that is not one of theirs.
    ForeignDevice,
}

impl Subject {
    #[must_use]
    pub fn person(id: impl Into<String>) -> Self {
        Self::Person(id.into())
    }

    #[must_use]
    pub fn role(role: Role) -> Self {
        Self::Role(role)
    }

    #[must_use]
    pub fn agent(id: impl Into<String>) -> Self {
        Self::Agent(id.into())
    }

    #[must_use]
    pub fn task(id: impl Into<String>) -> Self {
        Self::Task(id.into())
    }

    #[must_use]
    pub fn shortcut(id: impl Into<String>) -> Self {
        Self::Shortcut(id.into())
    }

    #[must_use]
    pub fn app(id: impl Into<String>) -> Self {
        Self::App(id.into())
    }
}

impl fmt::Display for Subject {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Person(id) => write!(f, "person {}", short(id)),
            Self::Role(role) => write!(f, "every {role}"),
            Self::Agent(id) => write!(f, "agent {id}"),
            Self::Task(id) => write!(f, "task {id}"),
            Self::Shortcut(id) => write!(f, "Shortcut {id}"),
            Self::App(id) => write!(f, "app {id}"),
            Self::ForeignDevice => f.write_str("a device that is not theirs"),
        }
    }
}

fn short(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

/// How long a grant is live.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Lifetime {
    /// For one use; the door that used it revokes it.
    Once,
    /// While the task is open.
    Task(String),
    /// While the session lasts.
    Session(String),
    /// Until a time, in Unix milliseconds (exclusive).
    Until(u64),
    /// Until someone revokes it.
    UntilRevoked,
}

/// Names one grant, so it can be revoked or consumed.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct GrantId(pub String);

impl GrantId {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for GrantId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One grant.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Grant {
    pub id: GrantId,
    pub subject: Subject,
    pub node: TreePath,
    pub kinds: KindSet,
    pub answer: Answer,
    pub lifetime: Lifetime,
    pub granted_by: Subject,
}

impl Grant {
    /// A grant until revoked, given by `granted_by`.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        subject: Subject,
        node: TreePath,
        kinds: KindSet,
        answer: Answer,
        granted_by: Subject,
    ) -> Self {
        Self {
            id: GrantId::new(id),
            subject,
            node,
            kinds,
            answer,
            lifetime: Lifetime::UntilRevoked,
            granted_by,
        }
    }

    #[must_use]
    pub fn with_lifetime(mut self, lifetime: Lifetime) -> Self {
        self.lifetime = lifetime;
        self
    }

    /// Whether the grant is live in `cx`.
    #[must_use]
    pub fn is_live(&self, cx: &Context<'_>) -> bool {
        match &self.lifetime {
            Lifetime::Once | Lifetime::UntilRevoked => true,
            Lifetime::Until(at) => cx.now_unix_ms < *at,
            Lifetime::Session(session) => cx.session.is_none_or(|current| current == session),
            Lifetime::Task(task) => cx.task_open.is_none_or(|open| open(task)),
        }
    }
}

/// What liveness depends on where a decision is made.
#[derive(Clone, Copy)]
pub struct Context<'a> {
    /// Wall clock, Unix milliseconds.
    pub now_unix_ms: u64,
    /// The current session. `None` where sessions are not known (a relay): a
    /// session grant then counts as live, and the device that holds the session
    /// is the one that refuses after it ends.
    pub session: Option<&'a str>,
    /// Whether a task is open. `None` where tasks are not known.
    pub task_open: Option<&'a dyn Fn(&str) -> bool>,
}

impl<'a> Context<'a> {
    #[must_use]
    pub const fn at(now_unix_ms: u64) -> Self {
        Self {
            now_unix_ms,
            session: None,
            task_open: None,
        }
    }

    #[must_use]
    pub const fn in_session(mut self, session: &'a str) -> Self {
        self.session = Some(session);
        self
    }

    #[must_use]
    pub const fn with_tasks(mut self, task_open: &'a dyn Fn(&str) -> bool) -> Self {
        self.task_open = Some(task_open);
        self
    }
}

impl fmt::Debug for Context<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Context")
            .field("now_unix_ms", &self.now_unix_ms)
            .field("session", &self.session)
            .field("task_open", &self.task_open.is_some())
            .finish()
    }
}
