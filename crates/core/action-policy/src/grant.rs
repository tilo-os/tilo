//! Grants: who, which node, which kinds, what answer, for how long.

use std::fmt;

use serde::{Deserialize, Serialize};
use tilo_clock::time::{SystemTime, UNIX_EPOCH};

use crate::{EarlierIds, KindSet, TreePath};

/// The answer a grant gives. Ordered `Deny < Ask < Allow`, so the stricter of
/// two answers is their minimum.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Answer {
    /// Refuse.
    ///
    /// Refusing to read is an access rule, not secrecy: every member device decrypts the
    /// workspace's content with the same workspace keys, so a Deny on reading one resource keeps it
    /// out of every Tilo read path for the grantee, while another member's device still holds what
    /// it needs to read the bytes ([`Grant::denies_reading_within_workspace`]). The workspace is the
    /// boundary of confidentiality.
    Deny,
    /// Pause for the person's approval, approved by the host on their device.
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

impl Answer {
    /// The answer a [`Display`](fmt::Display) wrote, if it is one.
    #[must_use]
    pub fn parse(key: &str) -> Option<Self> {
        match key {
            "deny" => Some(Self::Deny),
            "ask" => Some(Self::Ask),
            "allow" => Some(Self::Allow),
            _ => None,
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

/// A role preset. A member of a workspace holds one; the workspace keeps the grants each
/// role carries (see [`crate::roles`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Viewer,
    Commenter,
    Editor,
    /// An Editor who also moderates: other people's content through the
    /// `moderation` node, and who is in the workspace through the membership
    /// actions its role grants.
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

    /// Whether each of `other`'s grants is within what a holder of `self`
    /// holds ([`crate::roles::role_within`]): an invite never carries more
    /// than its inviter's preset.
    #[must_use]
    pub fn includes(self, other: Self) -> bool {
        crate::roles::role_within(other, self)
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

    /// The roles a member can be given, least to most. The owner's authority is
    /// the workspace's own and is never handed out.
    pub const ASSIGNABLE: [Self; 4] =
        [Self::Viewer, Self::Commenter, Self::Editor, Self::Moderator];

    /// Whether a member can be given this role ([`Self::ASSIGNABLE`]).
    #[must_use]
    pub const fn assignable(self) -> bool {
        !matches!(self, Self::Owner)
    }

    /// The name a role is stored and asked for by: `viewer`, `commenter`,
    /// `editor`, `moderator`, `owner`.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Viewer => "viewer",
            Self::Commenter => "commenter",
            Self::Editor => "editor",
            Self::Moderator => "moderator",
            Self::Owner => "owner",
        }
    }

    /// The role named by [`Self::key`].
    #[must_use]
    pub fn parse(key: &str) -> Option<Self> {
        match key.trim() {
            "viewer" => Some(Self::Viewer),
            "commenter" => Some(Self::Commenter),
            "editor" => Some(Self::Editor),
            "moderator" => Some(Self::Moderator),
            "owner" => Some(Self::Owner),
            _ => None,
        }
    }
}

impl fmt::Display for Role {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// Who a grant is for.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Grantee {
    /// One member.
    Member(tilo_member_types::MemberId),
    /// Everyone holding `role`, or a role whose grants hold all of its
    /// ([`Role::includes`]): a grant to `Role::Viewer` is everyone in the
    /// workspace.
    Role(Role),
    /// An app, by its id: what it may take of other apps' actions, and of the
    /// desktop's, on its own account.
    App(String),
    /// Members explicitly assigned this workspace-defined role.
    CustomRole(String),
    /// Any key, certified or not: what a workspace opens to visitors with no identity in it,
    /// who sign each call with a key of their own ([`tilo_member_types::Actor::Visitor`]). Only the
    /// workspace's entry sets these ([`crate::EntryPolicy::visitor_grants`]); nobody grants
    /// one by hand.
    AnyKey,
}

impl Grantee {
    #[must_use]
    pub fn member(member: tilo_member_types::MemberId) -> Self {
        Self::Member(member)
    }

    #[track_caller]
    #[must_use]
    pub fn person(id: impl Into<tilo_member_types::PersonId>) -> Self {
        Self::Member(tilo_member_types::MemberId::person(id))
    }

    /// The agent `id` acting for `person` ([`tilo_member_types::MemberId::Agent`]).
    #[track_caller]
    #[must_use]
    pub fn agent(id: impl Into<String>, person: impl Into<tilo_member_types::PersonId>) -> Self {
        Self::Member(tilo_member_types::MemberId::agent(id, person))
    }

    #[must_use]
    pub fn bot(id: impl Into<String>) -> Self {
        Self::Member(tilo_member_types::MemberId::bot(id))
    }

    #[must_use]
    pub fn role(role: Role) -> Self {
        Self::Role(role)
    }

    #[must_use]
    pub fn app(id: impl Into<String>) -> Self {
        Self::App(id.into())
    }

    /// The member, when a grant names one.
    #[must_use]
    pub fn as_member(&self) -> Option<&tilo_member_types::MemberId> {
        match self {
            Self::Member(member) => Some(member),
            Self::Role(_) | Self::App(_) | Self::CustomRole(_) | Self::AnyKey => None,
        }
    }

    #[must_use]
    pub fn person_id(&self) -> Option<&str> {
        match self.as_member()? {
            tilo_member_types::MemberId::Person(person) => Some(person.as_str()),
            tilo_member_types::MemberId::Agent { .. } | tilo_member_types::MemberId::Bot(_) => None,
        }
    }

    #[must_use]
    pub fn agent_id(&self) -> Option<&str> {
        self.as_member()?.agent_id()
    }

    /// The agent a grant names, by its id and its person, when it names one.
    #[must_use]
    pub fn as_agent(&self) -> Option<(&str, &tilo_member_types::PersonId)> {
        self.as_member()?.as_agent()
    }

    #[must_use]
    pub fn bot_id(&self) -> Option<&str> {
        match self.as_member()? {
            tilo_member_types::MemberId::Bot(bot) => Some(bot),
            tilo_member_types::MemberId::Person(_) | tilo_member_types::MemberId::Agent { .. } => {
                None
            }
        }
    }

    #[must_use]
    pub fn app_id(&self) -> Option<&str> {
        match self {
            Self::App(app) => Some(app),
            Self::Member(_) | Self::Role(_) | Self::CustomRole(_) | Self::AnyKey => None,
        }
    }

    #[must_use]
    pub fn role_of(&self) -> Option<Role> {
        match self {
            Self::Role(role) => Some(*role),
            Self::Member(_) | Self::App(_) | Self::CustomRole(_) | Self::AnyKey => None,
        }
    }
}

impl fmt::Display for Grantee {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Member(member) => member.fmt(f),
            Self::Role(role) => write!(f, "every {role}"),
            Self::CustomRole(id) => write!(f, "role {id}"),
            Self::App(app) => write!(f, "app {app}"),
            Self::AnyKey => f.write_str("any visitor"),
        }
    }
}

/// How long an answer stands: the one lifetime every grant, sandbox capability and permission
/// prompt uses.
///
/// Ordered by how long it lives: once, the session, an earlier time, a later one, until revoked.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Lifetime {
    /// For one use; the API that used it revokes it. The default: the least an answer gives.
    #[default]
    Once,
    /// While this session lasts. Kept in the device's memory only, so it ends with the session.
    Session,
    /// Until a time, in Unix milliseconds (exclusive).
    Until(u64),
    /// Until someone revokes it.
    UntilRevoked,
}

/// How long the timed answer a prompt offers lasts: an hour.
pub const OFFERED_SPAN_MS: u64 = 60 * 60 * 1000;

impl Lifetime {
    /// Whether it is still live at `now_ms` (Unix milliseconds). `None` skips the clock, for a
    /// checker that shares none with the writer; a one-use or session answer ends by an event,
    /// not at an instant.
    #[must_use]
    pub const fn is_live_at(self, now_ms: Option<u64>) -> bool {
        match (self, now_ms) {
            (Self::Until(at), Some(now)) => now < at,
            _ => true,
        }
    }

    /// Whether an answer with it is written down: one-use and session answers stay in memory,
    /// so keeping them can never turn them into lasting ones.
    #[must_use]
    pub const fn is_durable(self) -> bool {
        matches!(self, Self::Until(_) | Self::UntilRevoked)
    }

    /// The durations every permission prompt offers, in the order it offers them: once, this
    /// session, for an hour from `now_ms`, until taken back. A sheet's answer is an index into it.
    #[must_use]
    pub const fn offered(now_ms: u64) -> [Self; 4] {
        [
            Self::Once,
            Self::Session,
            Self::Until(now_ms.saturating_add(OFFERED_SPAN_MS)),
            Self::UntilRevoked,
        ]
    }

    /// The choice a sheet answers with for it: its place in [`Self::offered`].
    #[must_use]
    pub const fn choice(self) -> usize {
        match self {
            Self::Once => 0,
            Self::Session => 1,
            Self::Until(_) => 2,
            Self::UntilRevoked => 3,
        }
    }

    /// The offered duration a sheet's answer `choice` picks, at `now_ms`.
    #[must_use]
    pub fn chosen(choice: usize, now_ms: u64) -> Option<Self> {
        Self::offered(now_ms).get(choice).copied()
    }

    /// Its stable name, which labels and stored answers key on: `once`, `session`, `until` or
    /// `always`.
    #[must_use]
    pub const fn key(self) -> &'static str {
        match self {
            Self::Once => "once",
            Self::Session => "session",
            Self::Until(_) => "until",
            Self::UntilRevoked => "always",
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
    pub grantee: Grantee,
    pub node: TreePath,
    pub kinds: KindSet,
    pub answer: Answer,
    pub lifetime: Lifetime,
    pub granted_by: Grantee,
}

impl Grant {
    /// Whether this grant refuses reading something inside a workspace rather than the workspace
    /// itself: a rule every Tilo read path enforces, never a seal. Its grantee's devices hold the
    /// workspace's keys like every member's, so whoever sets one is told so ([`Answer::Deny`]).
    #[must_use]
    pub fn denies_reading_within_workspace(&self) -> bool {
        self.answer == Answer::Deny
            && self.kinds.contains(crate::ActionKind::Read)
            && self.node.segments().iter().any(|segment| {
                segment
                    .level()
                    .is_some_and(|level| level > crate::Level::Workspace)
            })
    }

    /// A grant until revoked, given by `granted_by`.
    #[must_use]
    pub fn new(
        id: impl Into<String>,
        grantee: Grantee,
        node: TreePath,
        kinds: KindSet,
        answer: Answer,
        granted_by: Grantee,
    ) -> Self {
        Self {
            id: GrantId::new(id),
            grantee,
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
        self.lifetime.is_live_at(Some(cx.now_unix_ms))
    }

    /// Whether this grant still stands as `policy` reads it at `cx`: live, and, when it names
    /// an app, given by a person who still reaches its node for every kind it carries.
    ///
    /// A person's grant to an app is theirs: losing the workspace, losing the node or taking a
    /// narrower role ends what they gave, with nothing to revoke.
    #[must_use]
    pub fn stands_in(&self, policy: &crate::WorkspacePolicy, cx: &Context<'_>) -> bool {
        if !self.is_live(cx) {
            return false;
        }
        if !matches!(self.grantee, Grantee::App(_)) {
            return true;
        }
        let Some(person) = self.granted_by.person_id() else {
            return false;
        };
        let now = cx.now_unix_ms / 1000;
        self.kinds
            .iter()
            .all(|kind| policy.answer_at(person, &self.node, kind, now) == Answer::Allow)
    }
}

/// What liveness depends on where a decision is made.
#[derive(Clone, Copy, Debug)]
pub struct Context<'a> {
    /// Wall clock, Unix milliseconds.
    pub now_unix_ms: u64,
    /// The ids apps were published under before: a grant written under one decides for the
    /// renamed app (`crate::earlier`). `None` reads what this process registered
    /// (`crate::earlier::register`).
    pub earlier: Option<&'a EarlierIds>,
}

impl<'a> Context<'a> {
    #[must_use]
    pub const fn at(now_unix_ms: u64) -> Self {
        Self {
            now_unix_ms,
            earlier: None,
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

    /// Deciding where the apps `earlier` names were renamed: grants under their earlier ids
    /// are theirs.
    #[must_use]
    pub const fn with_earlier_ids(mut self, earlier: &'a EarlierIds) -> Self {
        self.earlier = Some(earlier);
        self
    }

    /// Whether the grant node `pattern` covers `node`, under the ids its app had before too.
    #[must_use]
    pub fn covers(&self, pattern: &TreePath, node: &TreePath) -> bool {
        match self.earlier {
            Some(earlier) => earlier.covers(pattern, node),
            None => {
                pattern.covers(node)
                    || crate::earlier::with_held(|held| held.covers(pattern, node)).unwrap_or(false)
            }
        }
    }

    /// Whether a grant to `granted` speaks for one of `grantees`.
    #[must_use]
    pub fn grantee_matches(&self, granted: &Grantee, grantees: &[Grantee]) -> bool {
        grantees.contains(granted)
    }
}
