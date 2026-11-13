//! Who a workspace's people, agents and bots are, and who made a change.

use serde::{Deserialize, Serialize};
use tilo_app_id::AppId;

/// A person, agent or bot in a workspace.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemberId {
    /// A person, by the lowercase hex of their root public key.
    Person(String),
    Agent(String),
    Bot(String),
}

impl MemberId {
    #[must_use]
    pub fn person(id: impl Into<String>) -> Self {
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
            Self::Person(id) | Self::Agent(id) | Self::Bot(id) => id,
        }
    }

    /// The person this is, when it is one.
    #[must_use]
    pub fn person_id(&self) -> Option<&str> {
        match self {
            Self::Person(id) => Some(id),
            Self::Agent(_) | Self::Bot(_) => None,
        }
    }

    #[must_use]
    pub const fn is_person(&self) -> bool {
        matches!(self, Self::Person(_))
    }
}

impl std::fmt::Display for MemberId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Person(id) => write!(f, "person {}", short(id)),
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
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// Who made a change: a member, or an automation running for one.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Actor {
    Member(MemberId),
    Automation(AutomationId),
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

    /// The grantee a grant names this actor by.
    #[must_use]
    pub fn grantee(&self) -> crate::Grantee {
        match self {
            Self::Member(member) => crate::granted_to(member),
            Self::Automation(id) => crate::Grantee::shortcut(id.as_str().to_owned()),
        }
    }

    /// The member, when the actor is one.
    #[must_use]
    pub fn member_id(&self) -> Option<&MemberId> {
        match self {
            Self::Member(member) => Some(member),
            Self::Automation(_) => None,
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

    #[must_use]
    pub fn person(id: impl Into<String>) -> Self {
        Self::member(MemberId::person(id))
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
        Some(person)
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

    #[test]
    fn a_tree_names_every_member_and_person_in_it() {
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

    #[test]
    fn the_tree_is_written_as_it_is_read() {
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
