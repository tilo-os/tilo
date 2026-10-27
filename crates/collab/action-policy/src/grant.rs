//! Grants: who, which node, which kinds, what answer, for how long.

use std::fmt;

use serde::{Deserialize, Serialize};
use tilo_clock::{SystemTime, UNIX_EPOCH};

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

    /// Whether this answer lets an action run without anyone being asked:
    /// allowed; or asking first when the person acts themselves (`direct`),
    /// their own gesture being the answer. Someone acting for them, or a
    /// device not theirs, gets past an ask only with the person's
    /// confirmation.
    #[must_use]
    pub const fn admits(self, direct: bool) -> bool {
        match self {
            Self::Allow => true,
            Self::Ask => direct,
            Self::Deny => false,
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
    /// An Editor who also moderates: other people's content through the
    /// `moderation` node, and the membership of those ranked below.
    Moderator,
    Owner,
}

impl Role {
    pub const ALL: [Self; 5] = [
        Self::Viewer,
        Self::Commenter,
        Self::Editor,
        Self::Moderator,
        Self::Owner,
    ];

    /// Whether a holder of `self` may hand out `other` through an invite: an
    /// invite never carries more than its inviter's preset.
    #[must_use]
    pub const fn includes(self, other: Self) -> bool {
        self.rank() >= other.rank()
    }

    /// Whether `self` ranks strictly above `other`: a Moderator's
    /// governance reaches only the members it outranks.
    #[must_use]
    pub const fn outranks(self, other: Self) -> bool {
        self.rank() > other.rank()
    }

    const fn rank(self) -> u8 {
        match self {
            Self::Viewer => 0,
            Self::Commenter => 1,
            Self::Editor => 2,
            Self::Moderator => 3,
            Self::Owner => 4,
        }
    }

    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Viewer => "Viewer",
            Self::Commenter => "Commenter",
            Self::Editor => "Editor",
            Self::Moderator => "Moderator",
            Self::Owner => "Owner",
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Subject {
    /// A person, by the lowercase hex of their user root key.
    Person(String),
    /// Every member holding a role preset.
    Role(Role),
    /// An agent acting for a person.
    Agent(String),
    /// The work done on an issue, by its address: grants naming it bound
    /// every run of the issue, and give no one more than they hold.
    Task(String),
    /// A Shortcut run for a person.
    Shortcut(String),
    App(String),
    /// An installed bot, by installation id. It acts for its installer and
    /// never as them.
    Bot(String),
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

    #[must_use]
    pub fn bot(id: impl Into<String>) -> Self {
        Self::Bot(id.into())
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
            Self::Bot(id) => write!(f, "bot {}", short(id)),
            Self::ForeignDevice => f.write_str("a device that is not theirs"),
        }
    }
}

fn short(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

/// How long a grant is live.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(
    rename_all = "snake_case",
    from = "StoredLifetime",
    into = "StoredLifetime"
)]
pub enum Lifetime {
    /// For one use; the door that used it revokes it.
    Once,
    /// While the session lasts.
    Session(String),
    /// Until a time, in Unix milliseconds (exclusive).
    Until(u64),
    /// Until someone revokes it.
    UntilRevoked,
}

/// A lifetime as rooms store it, read and written. A grant once lived while
/// a task of the room was open; those tasks are gone, and such a grant reads
/// as one that has expired, so a room that holds one still loads. The task
/// keeps its place: compact encodings (postcard) name a variant by its
/// position, and a room's stored grants and the digests of its
/// administration requests are made of those bytes, so every lifetime is
/// written as it always was.
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredLifetime {
    Once,
    Task(String),
    Session(String),
    Until(u64),
    UntilRevoked,
}

impl From<Lifetime> for StoredLifetime {
    fn from(lifetime: Lifetime) -> Self {
        match lifetime {
            Lifetime::Once => Self::Once,
            Lifetime::Session(session) => Self::Session(session),
            Lifetime::Until(at) => Self::Until(at),
            Lifetime::UntilRevoked => Self::UntilRevoked,
        }
    }
}

impl From<StoredLifetime> for Lifetime {
    fn from(stored: StoredLifetime) -> Self {
        match stored {
            StoredLifetime::Once => Self::Once,
            StoredLifetime::Session(session) => Self::Session(session),
            StoredLifetime::Until(at) => Self::Until(at),
            StoredLifetime::Task(_) => Self::Until(0),
            StoredLifetime::UntilRevoked => Self::UntilRevoked,
        }
    }
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

    #[must_use]
    pub fn is_live(&self, cx: &Context<'_>) -> bool {
        match &self.lifetime {
            Lifetime::Once | Lifetime::UntilRevoked => true,
            Lifetime::Until(at) => cx.now_unix_ms < *at,
            Lifetime::Session(session) => cx.session.is_none_or(|current| current == session),
        }
    }
}

/// What liveness depends on where a decision is made.
#[derive(Clone, Copy, Debug)]
pub struct Context<'a> {
    /// Wall clock, Unix milliseconds.
    pub now_unix_ms: u64,
    /// The current session. `None` where sessions are not known (a relay): a
    /// session grant then counts as live, and the device that holds the session
    /// is the one that refuses after it ends.
    pub session: Option<&'a str>,
}

impl<'a> Context<'a> {
    #[must_use]
    pub const fn at(now_unix_ms: u64) -> Self {
        Self {
            now_unix_ms,
            session: None,
        }
    }

    /// At `now`, Unix seconds.
    #[must_use]
    pub const fn at_secs(now: u64) -> Self {
        Self::at(now.saturating_mul(1000))
    }

    /// At the wall-clock time `now`; the Unix epoch for a time before it.
    #[must_use]
    pub fn at_time(now: SystemTime) -> Self {
        Self::at(
            now.duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
        )
    }

    #[must_use]
    pub const fn in_session(mut self, session: &'a str) -> Self {
        self.session = Some(session);
        self
    }
}
