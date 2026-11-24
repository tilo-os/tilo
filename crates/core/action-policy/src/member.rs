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

/// The prefix of a direct conversation's id: the members it is with follow
/// it, sorted, as JSON.
pub const DIRECT_CONVERSATION_PREFIX: &str = "direct:";

impl MemberId {
    /// The id of the one direct conversation with exactly `members`, as every
    /// app that reads or writes conversations spells it: derived from the
    /// set, so two devices that open the same conversation at once make one,
    /// and neither has to create it first.
    #[must_use]
    pub fn direct_conversation(members: impl IntoIterator<Item = Self>) -> String {
        let mut members: Vec<Self> = members.into_iter().collect();
        members.sort();
        members.dedup();
        let members = serde_json::to_string(&members).unwrap_or_default();
        format!("{DIRECT_CONVERSATION_PREFIX}{members}")
    }

    /// The members the direct conversation `id` is with, or none when `id`
    /// names no direct conversation.
    #[must_use]
    pub fn direct_conversation_members(id: &str) -> Option<Vec<Self>> {
        let members = id.strip_prefix(DIRECT_CONVERSATION_PREFIX)?;
        serde_json::from_str::<Vec<Self>>(members)
            .ok()
            .filter(|members| !members.is_empty())
    }

    #[track_caller]
    #[must_use]
    pub fn person(id: impl Into<PersonId>) -> Self {
        Self::Person(id.into())
    }

    /// `id` as a person, when it is a person's bare root key: an id read from
    /// stored data or another device, which [`Self::person`] would panic on.
    #[must_use]
    pub fn parsed_person(id: &str) -> Option<Self> {
        PersonId::parse(id).ok().map(Self::Person)
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

    /// The member whose changes a document names by `bytes` ([`Self::actor_bytes`]); `None` for
    /// bytes no member writes as.
    #[must_use]
    pub fn of_actor_bytes(bytes: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?;
        let nonempty = |id: &str| (!id.is_empty()).then(|| id.to_owned());
        if let Some(id) = text.strip_prefix("agent:") {
            return nonempty(id).map(Self::Agent);
        }
        if let Some(id) = text.strip_prefix("bot:") {
            return nonempty(id).map(Self::Bot);
        }
        nonempty(text).and_then(|id| PersonId::parse(&id).ok().map(Self::Person))
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
    /// Someone with no identity in the workspace, by the key (hex) their page made and signs
    /// each call with: a visitor at a public entry. It is no member and holds only what the
    /// workspace opens to any key ([`crate::Grantee::AnyKey`]).
    Visitor(String),
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
            Self::Automation(_) | Self::App(_) | Self::Visitor(_) => None,
        }
    }

    /// The app, when the actor is one acting by itself.
    #[must_use]
    pub fn app(&self) -> Option<&AppId> {
        match self {
            Self::App(app) => Some(app),
            Self::Member(_) | Self::Automation(_) | Self::Visitor(_) => None,
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
            Self::Visitor(key) => [b"visitor:", key.as_bytes()].concat(),
        }
    }

    /// The actor whose changes a document names by `bytes` ([`Self::actor_bytes`]); `None` for
    /// bytes no actor writes as.
    #[must_use]
    pub fn of_actor_bytes(bytes: &[u8]) -> Option<Self> {
        let nonempty = |id: &[u8]| {
            std::str::from_utf8(id)
                .ok()
                .filter(|id| !id.is_empty())
                .map(str::to_owned)
        };
        if let Some(id) = bytes.strip_prefix(b"automation:") {
            return nonempty(id).map(|id| Self::Automation(AutomationId(id)));
        }
        if let Some(app) = bytes.strip_prefix(b"app:") {
            return nonempty(app).map(|app| Self::App(AppId::new(app)));
        }
        if let Some(key) = bytes.strip_prefix(b"visitor:") {
            return nonempty(key).map(Self::Visitor);
        }
        MemberId::of_actor_bytes(bytes).map(Self::Member)
    }

    /// The id inside the actor: a member's, an automation's or an app's.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Member(member) => member.id(),
            Self::Automation(id) => id.as_str(),
            Self::App(app) => app.as_str(),
            Self::Visitor(key) => key,
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
            handed: false,
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
            Self::Visitor(key) => write!(f, "visitor {}", key.get(..8).unwrap_or(key)),
        }
    }
}

/// Who made a change, and who they acted for, as a tree: an automation a person
/// set up, run by an agent, for two people, is
/// `Automation(1) for [Agent(1) for [Person(Q), Person(X)]]`.
///
/// A text format (JSON) writes only what the tree names, as every history entry and signed claim
/// already holds it; a positional format (the shell's link) writes every field, since it reads
/// field by field and cannot tell a left-out field from the next one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Deserialize)]
pub struct Acting {
    pub by: Actor,
    /// The app the change was made through.
    #[serde(default)]
    pub app: Option<AppId>,
    /// The agent session that acted, when an agent did.
    #[serde(default)]
    pub agent_session: Option<AgentSessionId>,
    /// What started the work: the chat message, the issue.
    #[serde(default)]
    pub cause: Option<ResourceId>,
    /// A person working from a device that is not theirs.
    #[serde(default)]
    pub foreign_device: bool,
    /// An agent a person's own device handed its session to, acting for that
    /// person: it carries the delegation a device gives its agents
    /// ([`Self::handed_preset`]). A claim the signer of a change makes,
    /// honoured only from the device of the person it acts for
    /// ([`Self::signed_by`]).
    #[serde(default)]
    pub handed: bool,
    /// Who this actor acted for. Empty for a person acting themselves.
    #[serde(default, rename = "for")]
    pub acted_for: Vec<Acting>,
}

impl Serialize for Acting {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct as _;
        // What a text format leaves out is what the tree does not name.
        let every = !serializer.is_human_readable();
        let mut out = serializer.serialize_struct("Acting", 7)?;
        out.serialize_field("by", &self.by)?;
        if every || self.app.is_some() {
            out.serialize_field("app", &self.app)?;
        } else {
            out.skip_field("app")?;
        }
        if every || self.agent_session.is_some() {
            out.serialize_field("agent_session", &self.agent_session)?;
        } else {
            out.skip_field("agent_session")?;
        }
        if every || self.cause.is_some() {
            out.serialize_field("cause", &self.cause)?;
        } else {
            out.skip_field("cause")?;
        }
        if every || self.foreign_device {
            out.serialize_field("foreign_device", &self.foreign_device)?;
        } else {
            out.skip_field("foreign_device")?;
        }
        if every || self.handed {
            out.serialize_field("handed", &self.handed)?;
        } else {
            out.skip_field("handed")?;
        }
        if every || !self.acted_for.is_empty() {
            out.serialize_field("for", &self.acted_for)?;
        } else {
            out.skip_field("for")?;
        }
        out.end()
    }
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
            handed: false,
            acted_for: Vec::new(),
        }
    }

    #[track_caller]
    #[must_use]
    pub fn person(id: impl Into<PersonId>) -> Self {
        Self::member(MemberId::person(id))
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
            handed: false,
            acted_for: Vec::new(),
        }
    }

    #[must_use]
    pub fn agent(id: impl Into<String>) -> Self {
        Self::member(MemberId::agent(id))
    }

    /// A visitor signing with `key`, a key no certificate in the workspace names
    /// ([`Actor::Visitor`]).
    #[must_use]
    pub fn visitor(key: &[u8; 32]) -> Self {
        Self::from(Actor::Visitor(hex::encode(key)))
    }

    /// The key of the visitor acting, when one is.
    #[must_use]
    pub fn visitor_key(&self) -> Option<[u8; 32]> {
        match &self.by {
            Actor::Visitor(key) => hex::decode(key).ok()?.try_into().ok(),
            _ => None,
        }
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
            handed: false,
            acted_for: Vec::new(),
        }
    }

    /// The same acting, for `who` too.
    #[must_use]
    pub fn acting_for(mut self, who: Acting) -> Self {
        self.acted_for.push(who);
        self
    }

    /// The same agent acting, as the agent of `owner`, the person it belongs to: where it
    /// answers someone else, `owner` bounds it as well, so a turn answering one person through
    /// another's agent does no more than both people and the agent may, on every resource. An
    /// acting that names `owner` already is returned as it is.
    #[must_use]
    pub fn agent_of(mut self, owner: impl Into<PersonId>) -> Self {
        let owner = owner.into();
        if !self.people().contains(&owner.as_str()) {
            self.acted_for.push(Self::person(owner));
        }
        self
    }

    /// The same acting, from a device that is not theirs when `foreign`.
    #[must_use]
    pub fn on_foreign_device(mut self, foreign: bool) -> Self {
        self.foreign_device = foreign;
        self
    }

    /// The agent `agent` acting for `person`, carrying the delegation the
    /// person handed it ([`Self::handed`]).
    #[must_use]
    #[track_caller]
    pub fn handed_agent(agent: impl Into<String>, person: impl Into<PersonId>) -> Self {
        Self::agent(agent).handed().acting_for(Self::person(person))
    }

    /// This work, handed on to the agent `agent` that its agent started (a sub-agent): one more
    /// actor in the chain. It acts for the same person, carrying the preset an agent gets for
    /// them ([`Self::handed`]), and through this whole tree, so every agent and person before
    /// it bounds what it does too: it does no more than its starter's turn may.
    #[must_use]
    pub fn handed_on(self, agent: impl Into<String>) -> Self {
        let mut on = Self::agent(agent).handed();
        if let Some(person) = self.acted_for_person() {
            on = on.acting_for(Self::person(person.to_owned()));
        }
        on.acting_for(self)
    }

    /// The same acting, by an agent a person's own device handed its session
    /// to ([`Self::handed`]).
    #[must_use]
    pub const fn handed(mut self) -> Self {
        self.handed = true;
        self
    }

    /// Who made a change that names this acting as its invoker, signed by the
    /// device acting as `signer`: this tree, for the signer, so the signer's
    /// grants bound it, unless the signer is the tree's own person on their
    /// own device.
    ///
    /// A handed agent keeps its delegation whoever's device signs, a person's
    /// own or a server's: a signer able to claim the agent's person could
    /// claim the person themselves, so the agent's preset, bounded by that
    /// person and by the signer, gives it no more. An agent's own device
    /// cannot claim a person, and keeps nothing it claims to have been
    /// handed, but for the work it hands on to an agent of its own (a
    /// sub-agent it started, [`Self::handed_on`]) whose chain names the
    /// signer's person: the signer is a node of that tree too, so the
    /// sub-agent's preset is bounded by the signer's own approval and gives
    /// it no more than its starter holds. A key's agent
    /// ([`key_agent_id`]) keeps nothing it claims either: it is decided by
    /// the grant its person signed, the key's certificate, which the
    /// workspace reads and its person revokes.
    #[must_use]
    pub fn signed_by(mut self, signer: &Self) -> Self {
        let person = signer
            .direct_person()
            .filter(|_| !signer.foreign_device)
            .map(str::to_owned);
        // The agent signing for its person, and that person: whom it may hand work to.
        let handing_agent = signer
            .agent_id()
            .filter(|_| person.is_none() && !signer.foreign_device && signer.app.is_none())
            .zip(signer.acted_for_person())
            .map(|(agent, person)| (agent.to_owned(), person.to_owned()));
        let hands = |node: &Self| {
            let Some(agent) = node
                .agent_id()
                .filter(|agent| key_of_agent(agent).is_none())
            else {
                return false;
            };
            person.is_some()
                || handing_agent.as_ref().is_some_and(|(signing, for_person)| {
                    agent != signing && node.people().contains(&for_person.as_str())
                })
        };
        self.unhand(&hands);
        match &person {
            // The tree names the signing person already: it is theirs as it is.
            Some(person) if self.people().contains(&person.as_str()) => self,
            _ => {
                if self.app.is_none() {
                    self.app.clone_from(&signer.app);
                }
                self.acting_for(signer.clone())
            }
        }
    }

    /// Clears [`Self::handed`] on every node `keeps` refuses.
    fn unhand(&mut self, keeps: &dyn Fn(&Self) -> bool) {
        if self.handed && !keeps(self) {
            self.handed = false;
        }
        for node in &mut self.acted_for {
            node.unhand(keeps);
        }
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

/// The agent member a key the person approved acts as wherever it is
/// admitted: `grant-` and its device key, hex. History names its changes by
/// it, and receivers find the key's certificate by it ([`key_of_agent`]).
#[must_use]
pub fn key_agent_id(device_public_key: &[u8; 32]) -> String {
    format!("{KEY_AGENT_PREFIX}{}", hex::encode(device_public_key))
}

/// The device key of the key whose agent `agent` is ([`key_agent_id`]);
/// `None` for any other agent.
#[must_use]
pub fn key_of_agent(agent: &str) -> Option<[u8; 32]> {
    let hex = agent.strip_prefix(KEY_AGENT_PREFIX)?;
    hex::decode(hex).ok()?.try_into().ok()
}

/// What [`key_agent_id`] starts with.
const KEY_AGENT_PREFIX: &str = "grant-";

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

        phase an_agent_answering_someone_else_is_bounded_by_its_own_person_too;
        {
            let for_bo = Acting::handed_agent("scout", "bo").agent_of("ada");
            assert_eq!(for_bo.people(), vec!["bo", "ada"], "Bo first, Ada bounding it");
            assert_eq!(for_bo.acted_for_person(), Some("bo"), "it answers Bo");
            let for_ada = Acting::handed_agent("scout", "ada").agent_of("ada");
            assert_eq!(for_ada, Acting::handed_agent("scout", "ada"), "its own person once");
        }

        phase a_sub_agent_is_one_more_actor_in_its_starters_chain;
        // Bo's turn on Ada's agent starts a sub-agent: it works for Bo, and everyone before it
        // in the chain bounds it, Ada's agent and Ada too.
        {
            let starter = Acting::agent("scout").acting_for(Acting::person("bo")).agent_of("ada");
            let helper = starter.clone().handed_on("helper");
            assert_eq!(helper.agent_id(), Some("helper"));
            assert_eq!(helper.acted_for_person(), Some("bo"), "it works for Bo");
            assert!(helper.handed, "with the preset an agent gets for Bo");
            assert!(helper.people().contains(&"ada"), "Ada bounds it: {helper:?}");
            assert!(
                helper.nodes().iter().any(|node| **node == starter),
                "its starter's whole tree bounds it: {helper:?}"
            );
            // The starter's own key signs the sub-agent's work: it keeps the preset, as work
            // handed on whose chain names the person the key belongs to.
            let signer = Acting::agent("scout").acting_for(Acting::person("ada"));
            let signed = helper.clone().signed_by(&signer);
            assert!(signed.handed, "{signed:?}");
            // A key of someone outside the chain hands it nothing.
            let stranger = Acting::agent("other").acting_for(Acting::person("cy"));
            assert!(!helper.signed_by(&stranger).handed);
        }

        phase a_member_is_read_back_from_the_bytes_a_document_names_it_by;
        {
            for member in [
                MemberId::person(PersonId::from_root([5; 32])),
                MemberId::agent("helper"),
                MemberId::bot("mod"),
            ] {
                assert_eq!(
                    MemberId::of_actor_bytes(&member.actor_bytes()),
                    Some(member)
                );
            }
            assert_eq!(MemberId::of_actor_bytes(b"automation:1"), None);
            assert_eq!(MemberId::of_actor_bytes(b"agent:"), None);
            assert_eq!(MemberId::of_actor_bytes(b""), None);
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
