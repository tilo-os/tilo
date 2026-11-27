//! Who a workspace's people, agents and bots are, and who made a change.

use crate::app_id::AppId;
use crate::person::PersonId;
use serde::{Deserialize, Serialize};

/// A person, agent or bot in a workspace.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberId {
    /// A person, by their root public key ([`PersonId`]).
    Person(PersonId),
    /// An agent: its id and the person it acts for, whom its certificate chains to. The pair is
    /// the agent: `scout` acting for one person and `scout` acting for another are two members,
    /// each with its own person's reach, answers and stops.
    Agent {
        id: String,
        person: PersonId,
    },
    Bot(String),
}

/// The prefix of a direct conversation's id: the members it is with follow
/// it, sorted, as JSON.
pub const DIRECT_CONVERSATION_PREFIX: &str = "direct:";

impl MemberId {
    /// The value a text mark naming this member holds, in the one form: the id as JSON
    /// (`{"person":…}`). A mention in any text is stored as this.
    #[must_use]
    pub fn mark_value(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The member a text mark's `value` names ([`Self::mark_value`]); `None` for a value
    /// naming anything else.
    #[must_use]
    pub fn of_mark_value(value: &str) -> Option<Self> {
        value
            .starts_with('{')
            .then(|| serde_json::from_str(value).ok())
            .flatten()
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

    /// The platform's own reserved name ([`PersonId::reserved`]), which no id read from data is:
    /// what a window that runs before anyone signed in names itself by.
    #[must_use]
    pub fn reserved_person(id: &str) -> Self {
        Self::Person(PersonId::reserved(id))
    }

    /// The agent `id` acting for `person`.
    #[track_caller]
    #[must_use]
    pub fn agent(id: impl Into<String>, person: impl Into<PersonId>) -> Self {
        Self::Agent {
            id: id.into(),
            person: person.into(),
        }
    }

    #[must_use]
    pub fn bot(id: impl Into<String>) -> Self {
        Self::Bot(id.into())
    }

    /// The id inside the kind: an agent's own id, without its person.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Person(id) => id.as_str(),
            Self::Agent { id, .. } | Self::Bot(id) => id,
        }
    }

    #[must_use]
    pub fn agent_id(&self) -> Option<&str> {
        match self {
            Self::Agent { id, .. } => Some(id),
            Self::Person(_) | Self::Bot(_) => None,
        }
    }

    /// The person an agent acts for, when this is an agent.
    #[must_use]
    pub fn agent_person(&self) -> Option<&PersonId> {
        match self {
            Self::Agent { person, .. } => Some(person),
            Self::Person(_) | Self::Bot(_) => None,
        }
    }

    /// The agent's id and its person, when this is an agent.
    #[must_use]
    pub fn as_agent(&self) -> Option<(&str, &PersonId)> {
        match self {
            Self::Agent { id, person } => Some((id, person)),
            Self::Person(_) | Self::Bot(_) => None,
        }
    }

    #[must_use]
    pub fn bot_id(&self) -> Option<&str> {
        match self {
            Self::Bot(id) => Some(id),
            Self::Person(_) | Self::Agent { .. } => None,
        }
    }

    /// The person this is, when it is one.
    #[must_use]
    pub fn as_person(&self) -> Option<&PersonId> {
        match self {
            Self::Person(id) => Some(id),
            Self::Agent { .. } | Self::Bot(_) => None,
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
        matches!(self, Self::Agent { .. })
    }

    #[must_use]
    pub const fn is_bot(&self) -> bool {
        matches!(self, Self::Bot(_))
    }

    /// The bytes a document names this member's changes by: a person's root key as written, an
    /// agent's `agent:<person>/<id>`, a bot's `bot:<id>`, as history reads them back.
    #[must_use]
    pub fn actor_bytes(&self) -> Vec<u8> {
        match self {
            Self::Person(id) => id.as_str().as_bytes().to_vec(),
            Self::Agent { id, person } => {
                [b"agent:", person.as_str().as_bytes(), b"/", id.as_bytes()].concat()
            }
            Self::Bot(id) => [b"bot:", id.as_bytes()].concat(),
        }
    }

    /// The member whose changes a document names by `bytes` ([`Self::actor_bytes`]); `None` for
    /// bytes no member writes as.
    #[must_use]
    pub fn of_actor_bytes(bytes: &[u8]) -> Option<Self> {
        let text = std::str::from_utf8(bytes).ok()?;
        let nonempty = |id: &str| (!id.is_empty()).then(|| id.to_owned());
        if let Some(agent) = text.strip_prefix("agent:") {
            let (person, id) = agent.split_once('/')?;
            let person = PersonId::parse(&nonempty(person)?).ok()?;
            return nonempty(id).map(|id| Self::Agent { id, person });
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
            Self::Agent { id, person } => write!(f, "agent {id} of {}", short(person.as_str())),
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
    /// workspace opens to any key (the policy's `Grantee::AnyKey`).
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

    /// The member, when the actor is one.
    #[must_use]
    pub fn member_id(&self) -> Option<&MemberId> {
        match self {
            Self::Member(member) => Some(member),
            Self::Automation(_) | Self::App(_) | Self::Visitor(_) => None,
        }
    }

    /// The person, when the actor is one.
    #[must_use]
    pub fn person_id(&self) -> Option<&str> {
        self.member_id().and_then(MemberId::person_id)
    }

    /// The agent, when the actor is one.
    #[must_use]
    pub fn agent_id(&self) -> Option<&str> {
        self.member_id().and_then(MemberId::agent_id)
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
/// Every format writes every field. A signed claim keeps the text its signer wrote, so an entry
/// written by a build that left unnamed fields out still checks against its own bytes.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    /// (the policy's `MemberAccess::may_write_for`).
    #[serde(default)]
    pub handed: bool,
    /// Who this actor acted for. Empty for a person acting themselves.
    #[serde(default, rename = "for")]
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

    /// The agent `id` of `person` acting. Whom it acts for in this tree is its
    /// [`Self::acting_for`]: its person, or someone it answers as well.
    #[track_caller]
    #[must_use]
    pub fn agent(id: impl Into<String>, person: impl Into<PersonId>) -> Self {
        Self::member(MemberId::agent(id, person))
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
        let person = person.into();
        Self::agent(agent, person.clone())
            .handed()
            .acting_for(Self::member(MemberId::Person(person)))
    }

    /// This work, handed on to the agent `agent` that its agent started (a sub-agent): one more
    /// actor in the chain. It is an agent of its starter's person, acting for the person its
    /// starter acts for and carrying the preset an agent gets for them ([`Self::handed`]), and
    /// through this whole tree, so every agent and person before it bounds what it does too: it
    /// does no more than its starter's turn may. `None` when no agent acts here to start it.
    #[must_use]
    pub fn handed_on(self, agent: impl Into<String>) -> Option<Self> {
        let starter = self.member_id()?.agent_person()?.clone();
        let mut on = Self::member(MemberId::Agent {
            id: agent.into(),
            person: starter,
        })
        .handed();
        if let Some(person) = self.acted_for_person() {
            on = on.acting_for(Self::person(person.to_owned()));
        }
        Some(on.acting_for(self))
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
    /// grants bound it. Only a member the signer may write for reaches here
    /// (the policy's `MemberAccess::may_write_for`): the signer's own person, or an
    /// agent it runs. A handed agent keeps its delegation from a device of the
    /// person it acts for; a device that is not that person's own hands nothing on.
    #[must_use]
    pub fn signed_by(mut self, signer: &Self) -> Self {
        // A device that is not the person's own hands nothing on: the preset a handed agent
        // carries is its own device's gift, so a change that device's person signs from
        // somewhere else carries none of it.
        if signer.foreign_device {
            self.unhand();
        }
        let person = signer
            .direct_person()
            .filter(|_| !signer.foreign_device)
            .map(str::to_owned);
        // The tree names the signing person already: it is theirs as it is.
        if let Some(person) = person
            && self.people().contains(&person.as_str())
        {
            return self;
        }
        if self.app.is_none() {
            self.app.clone_from(&signer.app);
        }
        self.acting_for(signer.clone())
    }

    /// Clears [`Self::handed`] on every node: what a device hands on is the delegation of the
    /// device it is, and one that is nobody's own hands nothing.
    fn unhand(&mut self) {
        self.handed = false;
        for node in &mut self.acted_for {
            node.unhand();
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

    /// The person this acting is for, when it is not the actor itself: for an automation acting
    /// through an agent (the run of a Shortcut an agent saved), the person that agent acts for.
    #[must_use]
    pub fn acted_for_person(&self) -> Option<&str> {
        self.person_node().and_then(Self::person_id)
    }

    /// The node of the member this acting is for: the one an automation acts for, and for an
    /// automation acting through an agent, that agent's person.
    fn person_node(&self) -> Option<&Self> {
        let node = self.acted_for.first()?;
        node.member_id()?;
        if node.is_person() || !self.is_automation() {
            return node.is_person().then_some(node);
        }
        Some(
            node.acted_for
                .first()
                .filter(|person| node.is_agent() && person.is_person())
                .unwrap_or(node),
        )
    }

    /// The agent acting here: the actor, when it is one, or the agent an automation acts through
    /// (the one that saved the Shortcut whose run it is). Its own grants bound the whole acting,
    /// as they bound the agent's own calls, and what holds an agent to its person's word holds
    /// the run too.
    #[must_use]
    pub fn acting_agent(&self) -> Option<&MemberId> {
        let node = if self.is_automation() {
            self.acted_for.first()?
        } else {
            self
        };
        node.member_id().filter(|member| member.is_agent())
    }

    /// Whether this acting is a Shortcut's run acting for `actor`: the automation whose principal
    /// they are, never the actor's own act.
    #[must_use]
    pub fn runs_for(&self, actor: &Actor) -> bool {
        self.is_automation() && self.principal() == actor
    }

    /// Whether this acting speaks for `actor`: the actor itself, or a Shortcut's run acting for
    /// them.
    #[must_use]
    pub fn stands_for(&self, actor: &Actor) -> bool {
        &self.by == actor || self.runs_for(actor)
    }

    /// On whose behalf this acting writes, as an app reads `invoker.principal()`: the actor
    /// itself, or, for an automation acting for a member (a Shortcut's run, through the agent
    /// that saved it or not), that member. Only what an app addresses and groups by follows it
    /// (whose conversation a run writes in); who may do what, and who history says did it, are
    /// decided on the acting itself, where the automation stays the actor and nobody is at a
    /// screen.
    #[must_use]
    pub fn principal(&self) -> &Actor {
        match self.person_node() {
            Some(node) if self.is_automation() => &node.by,
            _ => &self.by,
        }
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

/// An agent, a bot or a person wears the hue its member derives.
impl tilo_presence::WearsHue for MemberId {
    fn wearer(&self) -> tilo_presence::Wearer<'_> {
        match self {
            Self::Agent { id, .. } | Self::Bot(id) => tilo_presence::Wearer::Software(id),
            Self::Person(person) => tilo_presence::Wearer::Person(person.as_str()),
        }
    }
}

/// An automation or an app wears the hue its id derives.
impl tilo_presence::WearsHue for Actor {
    fn wearer(&self) -> tilo_presence::Wearer<'_> {
        match self.member_id() {
            Some(member) => tilo_presence::WearsHue::wearer(member),
            None => tilo_presence::Wearer::Person(self.id()),
        }
    }
}

/// The colour the identity `identity` (an actor's bytes as text) wears with no choice of its
/// own: the member it spells ([`tilo_presence::member_color`]), or else the palette slot the
/// string hashes to ([`tilo_presence::user_color`]). A person's key and an actor nobody named
/// keep their slot; an agent's change wears what the agent is, never the slot its `agent:<id>`
/// spelling would hash to.
#[must_use]
pub fn identity_color(identity: &str) -> (u8, u8, u8) {
    match MemberId::of_actor_bytes(identity.as_bytes()) {
        Some(member) => tilo_presence::member_color(&member),
        None => tilo_presence::user_color(identity),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An automation acting for a person (a Shortcut's unattended run, through an app or not, on
    /// a device of theirs or not) is never that person's own act: every accessor a gate reads
    /// says it is not the person themselves, and none of them reads it as an agent either, so
    /// "not an agent" never stands in for "the person". Only the principal, whom an app shows its
    /// writes as for, is the person.
    #[test]
    fn an_automation_acting_for_a_person_is_never_their_own_act() {
        let runs = [
            Acting::automation("reminder").acting_for(Acting::person("q")),
            Acting::automation("reminder")
                .acting_for(Acting::person("q"))
                .in_app("so.tilo.chat"),
            Acting::automation("reminder")
                .acting_for(Acting::person("q"))
                .on_foreign_device(true),
            Acting::automation("reminder")
                .acting_for(Acting::agent("scout", "q").acting_for(Acting::person("q"))),
        ];
        for run in &runs {
            assert!(!run.is_person(), "{run:?}");
            assert_eq!(run.person_id(), None, "{run:?}");
            assert!(!run.direct(), "{run:?}");
            assert_eq!(run.direct_person(), None, "{run:?}");
            assert!(run.delegated(), "{run:?}");
            assert!(!run.is_agent() && !run.is_bot(), "{run:?}");
            assert_eq!(run.member_id(), None, "{run:?}");
            assert!(run.is_automation(), "{run:?}");
        }
        for run in &runs {
            assert_eq!(run.acted_for_person(), Some("q"), "{run:?}");
            assert_eq!(
                run.principal(),
                &Actor::Member(MemberId::person("q")),
                "the run writes for its person: {run:?}"
            );
        }
        assert_eq!(
            runs[0].acting_agent(),
            None,
            "her own Shortcut runs through no agent"
        );
        assert_eq!(
            runs[3].acting_agent(),
            Some(&MemberId::agent("scout", "q")),
            "the Shortcut her agent saved runs through it, and its grants bound the run"
        );
        let person = Acting::person("q");
        assert!(person.is_person() && person.direct() && !person.is_automation());
        assert_eq!(person.principal(), &person.by);
    }

    tilo_test_env::scenario! {
        fn a_member_tree_names_and_writes_its_members;

        phase a_tree_names_every_member_and_person_in_it;
        {
            let acting =
                Acting::agent("claude", "q").acting_for(Acting::person("q").acting_for(Acting::person("x")));
            assert!(acting.delegated());
            assert_eq!(acting.direct_person(), None);
            assert_eq!(
                acting.members(),
                [
                    &MemberId::agent("claude", "q"),
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
            let starter = Acting::agent("scout", "ada").acting_for(Acting::person("bo")).agent_of("ada");
            let helper = starter.clone().handed_on("helper").expect("an agent starts it");
            assert_eq!(helper.member_id(), Some(&MemberId::agent("helper", "ada")), "Ada's agent's");
            assert_eq!(helper.acted_for_person(), Some("bo"), "it works for Bo");
            assert!(helper.handed, "with the preset an agent gets for Bo");
            assert!(helper.people().contains(&"ada"), "Ada bounds it: {helper:?}");
            assert!(
                helper.nodes().iter().any(|node| **node == starter),
                "its starter's whole tree bounds it: {helper:?}"
            );
            // The starter's own key signs the sub-agent's work: it keeps the preset, as work
            // handed on whose chain names the person the key belongs to.
            let signer = Acting::agent("scout", "ada").acting_for(Acting::person("ada"));
            let signed = helper.signed_by(&signer);
            assert!(signed.handed, "{signed:?}");
        }

        phase a_member_is_read_back_from_the_bytes_a_document_names_it_by;
        {
            for member in [
                MemberId::person(PersonId::from_root([5; 32])),
                MemberId::agent("helper", PersonId::from_root([5; 32])),
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
                    Acting::agent("claude", "q")
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
