//! What a person's agents ask them, and what they need not ask any more.
//!
//! An agent that works for a person with nobody watching asks before it does
//! what that person's grants leave to them: in the conversation, where the
//! question waits for their answer. Where the person answers that it may
//! always do that, the agent's server keeps a lasting permission:
//! this agent, this action, in this workspace, on this thing or on any thing
//! of its kind ([`Asked`]). From then on that action goes ahead unasked, and the
//! agent reports it in one line.
//!
//! A lasting permission is the person's own. It is kept in their own store
//! on the server that keeps their workspaces, beside their vault, so the server
//! decides with every device of theirs off. Only the person, on a device of
//! theirs, reads the list ([`LIST`]) and takes one back ([`TAKE_BACK`]): an
//! agent's request is refused, their own agent's included, and so is every
//! other member's. No action writes one. The server writes it when it reads
//! the person's own answer to a question it put itself, and what it writes
//! is what it kept when it asked, never what the question's words say.
//!
//! It lifts the asking and nothing else: what the workspace's policy denies
//! stays denied, and what the person may not do themselves no permission of
//! theirs lets their agent do.

use serde::{Deserialize, Serialize};

use super::{ActionAddress, RemoteActionTarget};

/// The provider the server answers what a person is asked under.
pub const PROVIDER: &str = "tilo.asking";
/// The resource the lasting permissions are actions of.
pub const PERMISSIONS: &str = "permissions";
/// The resource the questions that wait for the person are read from: its
/// [`LIST`] answers a [`WaitingQuestions`].
pub const WAITING: &str = "waiting";
/// Every permission that stands for the person, as a
/// [`Permissions`].
pub const LIST: &str = "list";
/// Take one back ([`TakeBack`]), answering what still stands as a
/// [`Permissions`]. From that moment its action asks again.
pub const TAKE_BACK: &str = "take-back";

/// The longest name a scope holds (a workspace, an agent, a provider, a
/// kind, an action, a document, a resource), in bytes.
pub const MAX_NAME_BYTES: usize = 512;
/// The longest of the words a permission is listed with, in bytes.
pub const MAX_WORDS_BYTES: usize = 240;

/// The key of the detail a failure carries where its invocation was not done because its
/// person is asked about it first and their answer does it. Such an end is no failure of
/// the action: whoever words it tells it by this from an end where nobody was asked.
pub const ASKED_DETAIL: &str = "person_asked";

/// The detail of such a failure ([`ASKED_DETAIL`]).
#[must_use]
pub fn asked_detail() -> serde_json::Value {
    let mut detail = serde_json::Map::new();
    detail.insert(ASKED_DETAIL.to_owned(), serde_json::Value::Bool(true));
    serde_json::Value::Object(detail)
}

/// Whether a failure's `details` say that its person is asked about it ([`ASKED_DETAIL`]).
#[must_use]
pub fn says_asked(details: Option<&serde_json::Value>) -> bool {
    details
        .and_then(|details| details.get(ASKED_DETAIL))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
}

/// The target of action `action` of the resource `resource`.
#[must_use]
pub fn target(resource: &str, action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(resource, action),
    }
}

/// One kind of action: what is done, to which kind of resource, of which
/// app or provider.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ActionName {
    /// The app that publishes it, by its id, or the server's own provider.
    pub provider: String,
    /// The kind of resource it acts on.
    pub kind: String,
    /// The action, as its resource names it.
    pub action: String,
}

/// One thing an action acts on.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Thing {
    /// The document that holds it; empty for the document an app keeps once in
    /// a workspace.
    pub document: String,
    /// The resource in that document.
    pub resource: String,
}

/// What an agent asks to do, and what a lasting permission covers: one
/// action, by one agent of the person's, in one workspace, on one thing or
/// on any thing of its kind. Nothing of it is words: every part is a name
/// the system gave.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Asked {
    /// The workspace it is done in.
    pub workspace: String,
    /// The agent that does it, by its id in that workspace.
    pub agent: String,
    pub action: ActionName,
    /// The thing it is done to. `None` asks about an action that names no
    /// thing of its own, and as a permission covers every thing of the
    /// action's kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<Thing>,
}

impl Asked {
    /// Whether a permission for `self` covers `asked`: the same agent, the
    /// same workspace, the same action, and the same thing unless `self`
    /// names none.
    #[must_use]
    pub fn covers(&self, asked: &Self) -> bool {
        self.workspace == asked.workspace
            && self.agent == asked.agent
            && self.action == asked.action
            && self
                .on
                .as_ref()
                .is_none_or(|thing| Some(thing) == asked.on.as_ref())
    }

    /// The same action on any thing of its kind.
    #[must_use]
    pub fn on_any(&self) -> Self {
        Self {
            on: None,
            ..self.clone()
        }
    }

    /// Whether every name it holds is one: not empty where it must name
    /// something, short, and on one line.
    #[must_use]
    pub fn is_named(&self) -> bool {
        let name = |text: &str| {
            !text.is_empty() && text.len() <= MAX_NAME_BYTES && !text.chars().any(char::is_control)
        };
        name(&self.workspace)
            && name(&self.agent)
            && name(&self.action.provider)
            && name(&self.action.kind)
            && name(&self.action.action)
            && self.on.as_ref().is_none_or(|thing| {
                name(&thing.resource)
                    && thing.document.len() <= MAX_NAME_BYTES
                    && !thing.document.chars().any(char::is_control)
            })
    }
}

/// What a permission is listed with, in the system's own words: the
/// action's label, the name of the thing and the workspace's name. Never
/// what its asker wrote.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Words {
    /// What the action is called.
    pub action: String,
    /// What the thing is called; empty where the permission names none.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub on: String,
    /// What the workspace is called.
    pub place: String,
}

/// The question whose answer let a permission stand: a message of a
/// conversation.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskedBy {
    pub workspace: String,
    pub conversation: String,
    pub message: String,
}

/// An agent, by its id and by the name its person knows it under.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentName {
    pub id: String,
    pub name: String,
}

/// A typed action a client calls as it is written here.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Call {
    pub provider: String,
    pub resource: String,
    pub action: String,
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

/// Where a permission is kept.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    /// Here: the person answered a question with always.
    Asking,
    /// With an item of the person's vault: a use of that item at its
    /// places.
    Vault,
}

/// One permission that stands, as [`LIST`] answers it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Permission {
    /// Its id, as [`TAKE_BACK`] names it.
    pub id: String,
    pub source: Source,
    /// The agent whose asking it lifts.
    pub agent: AgentName,
    /// The workspace it holds in.
    pub workspace: String,
    pub action: ActionName,
    /// The one thing it covers; absent where it covers every thing of the
    /// action's kind.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub on: Option<Thing>,
    pub words: Words,
    /// Since when, in milliseconds since the Unix epoch.
    pub since_ms: u64,
    /// The question it was answered on, where one was.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asked_by: Option<AskedBy>,
    /// How often its action ran unasked.
    pub used_times: u64,
    /// When it last did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_ms: Option<u64>,
    /// The action that takes it back.
    pub take_back: Call,
}

/// Every permission that stands for the person, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Permissions {
    pub permissions: Vec<Permission>,
}

/// An action of a document, as a client calls it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActionTarget {
    /// The app that keeps the document.
    pub app: String,
    pub document: String,
    pub resource: String,
    pub action: String,
}

/// One answer a waiting question offers, as the typed action that gives it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitingAnswer {
    pub id: String,
    /// What its button says; empty for an answer its kind words itself.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
    pub target: ActionTarget,
    pub arguments: serde_json::Map<String, serde_json::Value>,
}

/// One question that waits for the person: a message of a conversation,
/// asked by an agent their server has for them.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Waiting {
    pub workspace: String,
    /// The conversation it stands in.
    pub conversation: String,
    /// The document that holds the message.
    pub document: String,
    pub message: String,
    /// The question's kind, as the conversation's document names it.
    pub kind: String,
    /// What is asked, in its asker's words.
    pub words: String,
    /// Who asked.
    pub by: AgentName,
    /// Since when it waits, in milliseconds since the Unix epoch.
    pub asked_at_ms: u64,
    /// The answers it offers, in order; none for a question a protected
    /// field answers.
    pub answers: Vec<WaitingAnswer>,
}

/// Every question that waits for the person, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitingQuestions {
    pub questions: Vec<Waiting>,
}

/// The permission to take back.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TakeBack {
    /// Its id, as the listing names it.
    pub permission: String,
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )]

    use super::*;

    fn asked(agent: &str, action: &str, on: Option<&str>) -> Asked {
        Asked {
            workspace: "w".into(),
            agent: agent.into(),
            action: ActionName {
                provider: "app.example".into(),
                kind: "letters.box".into(),
                action: action.into(),
            },
            on: on.map(|resource| Thing {
                document: String::new(),
                resource: resource.into(),
            }),
        }
    }

    /// A permission for one thing covers that thing and nothing beside it;
    /// one that names no thing covers every thing of the action's kind and
    /// no other action.
    #[test]
    fn a_permission_covers_what_it_names_and_nothing_wider() {
        let one = asked("scout", "send", Some("letters.box/out"));
        assert!(one.covers(&one));
        assert!(!one.covers(&asked("scout", "send", Some("letters.box/other"))));
        assert!(!one.covers(&asked("scout", "send", None)));
        assert!(!one.covers(&asked("scout", "purge", Some("letters.box/out"))));
        assert!(!one.covers(&asked("other", "send", Some("letters.box/out"))));
        let elsewhere = Asked {
            workspace: "v".into(),
            ..one.clone()
        };
        assert!(!one.covers(&elsewhere));
        let other_kind = Asked {
            action: ActionName {
                kind: "letters.archive".into(),
                ..one.action.clone()
            },
            ..one.clone()
        };
        assert!(!one.covers(&other_kind));
        let other_app = Asked {
            action: ActionName {
                provider: "app.other".into(),
                ..one.action.clone()
            },
            ..one.clone()
        };
        assert!(!one.covers(&other_app));

        let any = one.on_any();
        assert!(any.covers(&one));
        assert!(any.covers(&asked("scout", "send", Some("letters.box/other"))));
        assert!(any.covers(&asked("scout", "send", None)));
        assert!(!any.covers(&asked("scout", "purge", None)));
        assert!(!any.covers(&asked("other", "send", None)));
    }

    /// A scope is made of names: an empty one, one over the limit and one
    /// that breaks the line are none.
    #[test]
    fn a_scope_is_made_of_names() {
        assert!(asked("scout", "send", Some("letters.box/out")).is_named());
        assert!(asked("scout", "send", None).is_named());
        assert!(!asked("", "send", None).is_named());
        assert!(!asked("scout", "", None).is_named());
        assert!(!asked("scout", "send", Some("")).is_named());
        assert!(!asked("scout\nadmin", "send", None).is_named());
        assert!(!asked(&"a".repeat(MAX_NAME_BYTES + 1), "send", None).is_named());
    }

    /// The listing is written as a client reads it: camel case, a thing and
    /// a question only where there is one.
    #[test]
    fn a_listing_reads_as_a_client_binds_it() {
        let scope = asked("scout", "send", None);
        let permissions = Permission {
            id: "0".repeat(32),
            source: Source::Asking,
            agent: AgentName {
                id: scope.agent.clone(),
                name: "Scout".into(),
            },
            workspace: scope.workspace.clone(),
            action: scope.action,
            on: None,
            words: Words {
                action: "Send a letter".into(),
                on: String::new(),
                place: "Team".into(),
            },
            since_ms: 7,
            asked_by: None,
            used_times: 0,
            last_used_ms: None,
            take_back: Call {
                provider: PROVIDER.into(),
                resource: PERMISSIONS.into(),
                action: TAKE_BACK.into(),
                arguments: serde_json::Map::new(),
            },
        };
        let json = serde_json::to_value(&permissions).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "id": "0".repeat(32),
                "source": "asking",
                "agent": {"id": "scout", "name": "Scout"},
                "workspace": "w",
                "action": {"provider": "app.example", "kind": "letters.box", "action": "send"},
                "words": {"action": "Send a letter", "place": "Team"},
                "sinceMs": 7,
                "usedTimes": 0,
                "takeBack": {
                    "provider": "tilo.asking",
                    "resource": "permissions",
                    "action": "take-back",
                    "arguments": {},
                },
            })
        );
        assert_eq!(
            serde_json::from_value::<Permission>(json).unwrap(),
            permissions
        );
    }
}
