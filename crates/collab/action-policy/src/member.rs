//! Who a workspace's people, agents and bots are, and who made a change.

use serde::{Deserialize, Serialize};
use tilo_app_id::AppId;

use crate::person::PersonId;

/// A person, agent or bot in a workspace.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberId {
    /// A person, by their root public key ([`PersonId`]).
    Person(PersonId),
    Agent(String),
    Bot(String),
}

impl MemberId {
    #[track_caller]
    #[must_use]
    pub fn person(id: impl Into<PersonId>) -> Self {
        Self::Person(id.into())
    }

    #[must_use]
    pub fn agent(id: impl Into<String>) -> Self {
        Self::Agent(id.into())
    }

    #[must_use]
    pub fn bot(id: impl Into<String>) -> Self {
        Self::Bot(id.into())
    }

    /// The id inside the kind.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Person(id) => id.as_str(),
            Self::Agent(id) | Self::Bot(id) => id,
        }
    }

    #[must_use]
    pub fn agent_id(&self) -> Option<&str> {
        match self {
            Self::Agent(id) => Some(id),
            Self::Person(_) | Self::Bot(_) => None,
        }
    }

    #[must_use]
    pub fn bot_id(&self) -> Option<&str> {
        match self {
            Self::Bot(id) => Some(id),
            Self::Person(_) | Self::Agent(_) => None,
        }
    }

    /// The person this is, when it is one.
    #[must_use]
    pub fn as_person(&self) -> Option<&PersonId> {
        match self {
            Self::Person(id) => Some(id),
            Self::Agent(_) | Self::Bot(_) => None,
        }
    }

    /// The person this is, by their root key, when it is one.
    #[must_use]
    pub fn person_id(&self) -> Option<&str> {
        self.as_person().map(PersonId::as_str)
    }

    #[must_use]
    pub const fn is_person(&self) -> bool {
        matches!(self, Self::Person(_))
    }

    #[must_use]
    pub const fn is_agent(&self) -> bool {
        matches!(self, Self::Agent(_))
    }

    #[must_use]
    pub const fn is_bot(&self) -> bool {
        matches!(self, Self::Bot(_))
    }

    /// The bytes a document names this member's changes by: a person's root key as written, an
    /// agent's `agent:<id>`, a bot's `bot:<id>`, as history reads them back.
    #[must_use]
    pub fn actor_bytes(&self) -> Vec<u8> {
        match self {
            Self::Person(id) => id.as_str().as_bytes().to_vec(),
            Self::Agent(id) => [b"agent:", id.as_bytes()].concat(),
            Self::Bot(id) => [b"bot:", id.as_bytes()].concat(),
        }
    }

    /// The spelling a stored id uses: a person's root key as it is, an agent's
    /// `agent:<id>`, a bot's `bot:<id>`.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::Person(id) => id.as_str().to_owned(),
            Self::Agent(id) => format!("agent:{id}"),
            Self::Bot(id) => format!("bot:{id}"),
        }
    }

    /// The member [`Self::key`] spells.
    ///
    /// `None` for a key carrying a prefix this workspace does not know: a
    /// person's id has none, so `person:<root>` names nobody.
    #[must_use]
    pub fn parse(key: &str) -> Option<Self> {
        fn nonempty(id: &str) -> Option<String> {
            (!id.is_empty()).then(|| id.to_owned())
        }
        if let Some(id) = key.strip_prefix("agent:") {
            return nonempty(id).map(Self::Agent);
        }
        if let Some(id) = key.strip_prefix("bot:") {
            return nonempty(id).map(Self::Bot);
        }
        if key.is_empty() {
            return None;
        }
        PersonId::parse(key).ok().map(Self::Person)
    }
}

impl std::fmt::Display for MemberId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Person(id) => write!(f, "person {}", short(id.as_str())),
            Self::Agent(id) => write!(f, "agent {id}"),
            Self::Bot(id) => write!(f, "bot {}", short(id)),
        }
    }
}

fn short(id: &str) -> &str {
    id.get(..12).unwrap_or(id)
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if passes the field by reference"
)]
fn is_false(value: &bool) -> bool {
    !*value
}

/// An automation: saved steps with triggers, running for the member who owns
/// it. Not a member.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(transparent)]
pub struct AutomationId(pub String);

impl AutomationId {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// One context of one agent: a conversation with a Tilo agent, or one Claude
/// Code session.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AgentSessionId(pub String);

impl AgentSessionId {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The resource a change was made for: a chat message, an issue.
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResourceId(pub String);

impl ResourceId {
    #[must_use]
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ResourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Who made a change: a member, an automation running for one, or an app acting by itself.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    Member(MemberId),
    Automation(AutomationId),
    /// An app changing its own resources: a trigger it declares, or a migration of its
    /// package.
    App(AppId),
}

impl Actor {
    #[must_use]
    pub fn member(member: MemberId) -> Self {
        Self::Member(member)
    }

    #[must_use]
    pub fn automation(id: impl Into<String>) -> Self {
        Self::Automation(AutomationId::new(id))
    }

    /// The grantee a grant names this actor by, when it is a member: an
    /// automation is its owner's.
    #[must_use]
    pub fn grantee(&self) -> Option<crate::Grantee> {
        Some(crate::granted_to(self.member_id()?))
    }

    /// The member, when the actor is one.
    #[must_use]
    pub fn member_id(&self) -> Option<&MemberId> {
        match self {
            Self::Member(member) => Some(member),
            Self::Automation(_) | Self::App(_) => None,
        }
    }

    /// The spelling a stored id uses: a member's [`MemberId::key`], an
    /// automation's `automation:<id>`, an app's `app:<id>`.
    #[must_use]
    pub fn key(&self) -> String {
        match self {
            Self::Member(member) => member.key(),
            Self::Automation(id) => format!("automation:{}", id.as_str()),
            Self::App(app) => format!("app:{app}"),
        }
    }

    /// The app, when the actor is one acting by itself.
    #[must_use]
    pub fn app(&self) -> Option<&AppId> {
        match self {
            Self::App(app) => Some(app),
            Self::Member(_) | Self::Automation(_) => None,
        }
    }

    /// The bytes a document names this actor's changes by: a member's
    /// [`MemberId::actor_bytes`], an automation's `automation:<id>`.
    #[must_use]
    pub fn actor_bytes(&self) -> Vec<u8> {
        match self {
            Self::Member(member) => member.actor_bytes(),
            Self::Automation(id) => [b"automation:", id.as_str().as_bytes()].concat(),
            Self::App(app) => [b"app:", app.as_str().as_bytes()].concat(),
        }
    }

    /// The id inside the actor: a member's, an automation's or an app's.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Member(member) => member.id(),
            Self::Automation(id) => id.as_str(),
            Self::App(app) => app.as_str(),
        }
    }
}

impl From<Actor> for Acting {
    fn from(actor: Actor) -> Self {
        Self {
            by: actor,
            agent_session: None,
            app: None,
            cause: None,
            foreign_device: false,
            acted_for: Vec::new(),
        }
    }
}

impl From<MemberId> for Actor {
    fn from(member: MemberId) -> Self {
        Self::Member(member)
    }
}

impl std::fmt::Display for Actor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Member(member) => member.fmt(f),
            Self::Automation(id) => write!(f, "automation {}", id.as_str()),
            Self::App(app) => write!(f, "app {app}"),
        }
    }
}

/// Who made a change, and who they acted for, as a tree: an automation a person
/// set up, run by an agent, for two people, is
/// `Automation(1) for [Agent(1) for [Person(Q), Person(X)]]`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Acting {
    pub by: Actor,
    /// The app the change was made through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app: Option<AppId>,
    /// The agent session that acted, when an agent did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<AgentSessionId>,
    /// What started the work: the chat message, the issue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<ResourceId>,
    /// A person working from a device that is not theirs.
    #[serde(default, skip_serializing_if = "is_false")]
    pub foreign_device: bool,
    /// Who this actor acted for. Empty for a person acting themselves.
    #[serde(default, rename = "for", skip_serializing_if = "Vec::is_empty")]
    pub acted_for: Vec<Acting>,
}

impl Acting {
    /// A member acting for themselves.
    #[must_use]
    pub fn member(member: MemberId) -> Self {
        Self {
            by: Actor::Member(member),
            agent_session: None,
            app: None,
            cause: None,
            foreign_device: false,
            acted_for: Vec::new(),
        }
    }

    #[track_caller]
    #[must_use]
    pub fn person(id: impl Into<PersonId>) -> Self {
        Self::member(MemberId::person(id))
    }

    /// The actor an [`Actor::key`] spells, acting for itself. A key that names
    /// nobody — one carrying a prefix this code does not know, such as the old
    /// `person:<root>` — acts for nobody: it holds no one's authority.
    #[must_use]
    pub fn of_key(key: &str) -> Self {
        if let Some(id) = key.strip_prefix("automation:") {
            return Self::automation(id);
        }
        if let Some(id) = key.strip_prefix("agent:") {
            return Self::agent(id);
        }
        if let Some(id) = key.strip_prefix("bot:") {
            return Self::bot(id);
        }
        if let Some(id) = key.strip_prefix("app:") {
            return Self::by_app(id);
        }
        match PersonId::parse(key) {
            Ok(person) => Self::person(person),
            Err(_) => Self::person(""),
        }
    }

    /// An app acting by itself: a trigger it declares, or a migration of its
    /// package.
    #[must_use]
    pub fn by_app(id: impl Into<AppId>) -> Self {
        Self {
            by: Actor::App(id.into()),
            agent_session: None,
            app: None,
            cause: None,
            foreign_device: false,
            acted_for: Vec::new(),
        }
    }

    #[must_use]
    pub fn agent(id: impl Into<String>) -> Self {
        Self::member(MemberId::agent(id))
    }

    #[must_use]
    pub fn bot(id: impl Into<String>) -> Self {
        Self::member(MemberId::bot(id))
    }

    /// The same acting, through the app `app`.
    #[must_use]
    pub fn in_app(mut self, app: impl Into<AppId>) -> Self {
        self.app = Some(app.into());
        self
    }

    #[must_use]
    pub fn automation(id: impl Into<String>) -> Self {
        Self {
            by: Actor::automation(id),
            agent_session: None,
            app: None,
            cause: None,
            foreign_device: false,
            acted_for: Vec::new(),
        }
    }

    /// The same acting, for `who` too.
    #[must_use]
    pub fn acting_for(mut self, who: Acting) -> Self {
        self.acted_for.push(who);
        self
    }

    /// The same acting, from a device that is not theirs when `foreign`.
    #[must_use]
    pub fn on_foreign_device(mut self, foreign: bool) -> Self {
        self.foreign_device = foreign;
        self
    }

    /// The same acting, caused by the resource `cause`.
    #[must_use]
    pub fn caused_by(mut self, cause: impl Into<String>) -> Self {
        self.cause = Some(ResourceId::new(cause));
        self
    }

    /// The same acting, in the agent session `session`.
    #[must_use]
    pub fn in_session(mut self, session: impl Into<String>) -> Self {
        self.agent_session = Some(AgentSessionId::new(session));
        self
    }

    /// Whether someone acted for a person here: software, a device that is not
    /// theirs, or another person on whose behalf this one acted.
    #[must_use]
    pub fn delegated(&self) -> bool {
        self.foreign_device
            || !self.acted_for.is_empty()
            || !matches!(self.by, Actor::Member(MemberId::Person(_)))
    }

    /// Whether this is a person acting themselves, from their own device.
    #[must_use]
    pub fn direct(&self) -> bool {
        !self.delegated()
    }

    /// The person acting themselves, from their own device.
    #[must_use]
    pub fn direct_person(&self) -> Option<&str> {
        if !self.direct() {
            return None;
        }
        let Actor::Member(MemberId::Person(person)) = &self.by else {
            return None;
        };
        Some(person.as_str())
    }

    /// The app the change went through.
    #[must_use]
    pub fn app(&self) -> Option<&AppId> {
        self.app.as_ref()
    }

    /// The actor, when it is a member.
    #[must_use]
    pub fn member_id(&self) -> Option<&MemberId> {
        self.by.member_id()
    }

    #[must_use]
    pub fn is_person(&self) -> bool {
        self.by.member_id().is_some_and(MemberId::is_person)
    }

    #[must_use]
    pub fn is_agent(&self) -> bool {
        self.by.member_id().is_some_and(MemberId::is_agent)
    }

    #[must_use]
    pub fn is_bot(&self) -> bool {
        self.by.member_id().is_some_and(MemberId::is_bot)
    }

    #[must_use]
    pub fn is_automation(&self) -> bool {
        matches!(self.by, Actor::Automation(_))
    }

    #[must_use]
    pub fn person_id(&self) -> Option<&str> {
        self.by.member_id().and_then(MemberId::person_id)
    }

    #[must_use]
    pub fn agent_id(&self) -> Option<&str> {
        self.by.member_id().and_then(MemberId::agent_id)
    }

    #[must_use]
    pub fn bot_id(&self) -> Option<&str> {
        self.by.member_id().and_then(MemberId::bot_id)
    }

    /// The person this acting is for, when it is not the actor itself.
    #[must_use]
    pub fn acted_for_person(&self) -> Option<&str> {
        self.acted_for
            .first()
            .and_then(|node| node.by.member_id())
            .and_then(MemberId::person_id)
    }

    /// Every member in the tree, the actor first.
    pub fn members(&self) -> Vec<&MemberId> {
        let mut members: Vec<&MemberId> = self.by.member_id().into_iter().collect();
        for node in &self.acted_for {
            members.extend(node.members());
        }
        members
    }

    /// Every node of the tree, itself first.
    pub fn nodes(&self) -> Vec<&Self> {
        std::iter::once(self)
            .chain(self.acted_for.iter().flat_map(Self::nodes))
            .collect()
    }

    /// The people in the tree, in tree order.
    pub fn people(&self) -> Vec<&str> {
        self.nodes()
            .into_iter()
            .filter_map(|node| node.by.member_id().and_then(MemberId::person_id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        fn a_member_tree_names_and_writes_its_members;

        phase a_tree_names_every_member_and_person_in_it;
        {
            let acting =
                Acting::agent("claude").acting_for(Acting::person("q").acting_for(Acting::person("x")));
            assert!(acting.delegated());
            assert_eq!(acting.direct_person(), None);
            assert_eq!(
                acting.members(),
                [
                    &MemberId::agent("claude"),
                    &MemberId::person("q"),
                    &MemberId::person("x")
                ]
            );
            assert_eq!(acting.people(), vec!["q", "x"]);
            let person = Acting::person("q");
            assert!(person.direct());
            assert_eq!(person.direct_person(), Some("q"));
            assert!(Acting::person("q").on_foreign_device(true).delegated());
        }

        phase a_persons_key_is_their_bare_root_and_a_prefixed_one_names_nobody;
        {
            let root = PersonId::from_root([5; 32]);
            assert_eq!(
                MemberId::parse(&root),
                Some(MemberId::person(&root)),
                "a person's stored key is their bare root key"
            );
            assert_eq!(
                MemberId::person(&root).key(),
                root.as_str(),
                "a person is written bare"
            );
            assert_eq!(
                MemberId::parse(&format!("person:{root}")),
                None,
                "the prefixed spelling names no member"
            );
            assert_eq!(
                MemberId::parse("agent:helper"),
                Some(MemberId::agent("helper")),
                "software keeps the prefix that says what it is"
            );
        }

        phase the_tree_is_written_as_it_is_read;
        {
            let acting = Acting::automation("1")
                .acting_for(
                    Acting::agent("claude")
                        .acting_for(Acting::person("q"))
                        .acting_for(Acting::person("x")),
                )
                .caused_by("chat.message:m1");
            let json = serde_json::to_string(&acting).unwrap();
            assert_eq!(serde_json::from_str::<Acting>(&json).unwrap(), acting);
            assert!(json.contains("\"for\""), "{json}");
        }
    }
}
