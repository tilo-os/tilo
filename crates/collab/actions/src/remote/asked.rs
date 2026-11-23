//! What a call that waits for a person's yes says about itself, and how the
//! questions that wait for a person are listed.
//!
//! A call the caller's grants leave to a person is refused by its provider
//! with [`super::ActionCallError::ConfirmationRequired`], carrying
//! [`AskedFacts`]: what the person is shown. The dispatch keeps the call as
//! its own `tilo.invocations` record in status asked, readable by the caller
//! and the person asked, and that record is the question: there is no store
//! of asks beside it. The person answers it with the record's `resolve`
//! action; on approve the dispatch admits the same call again, bound to that
//! record, and a provider reads that it was approved
//! ([`super::InvokeRequest::approval`]).
//!
//! [`WAITING`]`#`[`LIST`] lists the asked records a person may answer, each as
//! an [`Asked`] row.

use serde::{Deserialize, Serialize};

use super::{ActionAddress, RemoteActionTarget};

/// The provider the questions that wait for a person are listed under.
pub const PROVIDER: &str = "tilo.asking";
/// The resource the questions that wait for the person are read from: its
/// [`LIST`] answers [`AskedList`].
pub const WAITING: &str = "waiting";
/// Every question that waits for the person, as [`AskedList`].
pub const LIST: &str = "list";
/// The phase of an invocation's progress while it waits in its asked record: its message
/// names the record (`<workspace>/<record>`).
pub const ASKED_PHASE: &str = "asked";

/// The target of action `action` of the resource `resource`.
#[must_use]
pub fn target(resource: &str, action: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: PROVIDER.to_owned(),
        address: ActionAddress::new(resource, action),
    }
}

/// One fact the person reads before answering: a label and its value, as a
/// reviewer reads it, long values cut and secrets withheld.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Fact {
    pub label: String,
    pub value: String,
}

impl Fact {
    #[must_use]
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            value: value.into(),
        }
    }
}

/// Who a member is, by id and by the name the person knows them under.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Named {
    pub id: String,
    pub name: String,
}

/// Who asks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum AskedBy {
    /// An agent, working in a turn or on its own.
    Agent(Named),
    /// An app's code, by the app and its title, and who wrote it.
    App {
        app: String,
        title: String,
        #[serde(default)]
        author: Option<Named>,
    },
    /// A device of the person's, or the server that keeps their workspaces.
    Device(Named),
}

impl Default for AskedBy {
    fn default() -> Self {
        Self::Device(Named::default())
    }
}

/// What a call that waits for a person's yes shows them: what the asked
/// record keeps beside the call itself, and what a [`Asked`] row lists.
/// Every part is written, empty or not: a refusal travels by position.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskedFacts {
    /// Who asks.
    pub by: AskedBy,
    /// What is asked, in the asker's words where it says something of its
    /// own (a turn's question); empty where the action's name says it.
    #[serde(default)]
    pub words: String,
    /// The record the call acts on, by its name, where it names one.
    #[serde(default)]
    pub on: Option<String>,
    /// What the person reads before answering, in order.
    #[serde(default)]
    pub facts: Vec<Fact>,
    /// Whether the call takes away what Undo does not bring back.
    #[serde(default)]
    pub destructive: bool,
    /// A question's choices, where the person answers with one of them
    /// rather than yes or no.
    #[serde(default)]
    pub choices: Vec<String>,
    /// The action's verb alone (`set`), or the phrase it declares for always:
    /// "Always let {asker} {verb} {plural}".
    #[serde(default)]
    pub verb: String,
    /// The plural of the resource type the action is on (`alarms`), which
    /// "always" is offered for; `None` where it is not offered.
    #[serde(default)]
    pub plural: Option<String>,
    /// The conversation a turn's question belongs to, by its address.
    #[serde(default)]
    pub conversation: Option<String>,
}

impl AskedFacts {
    /// Facts about a call by `by` that show nothing beyond its action.
    #[must_use]
    pub fn by(by: AskedBy) -> Self {
        Self {
            by,
            ..Self::default()
        }
    }

    /// With `fact` added after the others.
    #[must_use]
    pub fn with_fact(mut self, label: impl Into<String>, value: impl Into<String>) -> Self {
        self.facts.push(Fact::new(label, value));
        self
    }

    /// What a run's failure for want of the person's answer states in its
    /// `details`: these facts, as JSON.
    #[must_use]
    pub fn to_details(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// The facts a run's failure states in its `details`; none where it
    /// states none.
    #[must_use]
    pub fn from_details(details: Option<&serde_json::Value>) -> Self {
        details
            .cloned()
            .and_then(|details| serde_json::from_value(details).ok())
            .unwrap_or_default()
    }

    /// The value of the fact labelled `label`.
    #[must_use]
    pub fn fact(&self, label: &str) -> Option<&str> {
        self.facts
            .iter()
            .find(|fact| fact.label == label)
            .map(|fact| fact.value.as_str())
    }
}

/// A `tilo.invocations` record, by the workspace it is in and its address there.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RecordRef {
    pub workspace: String,
    pub record: String,
}

/// What writes a call that waits for a person's yes as its own invocation
/// record in status asked: the workspace's store, which the host hands
/// whoever answers callers.
pub trait AskedRecords: Send + Sync {
    /// Write the call `request` that `caller` made, which needs the yes of
    /// `facts.by`'s person, as its `tilo.invocations` record in status asked
    /// in `workspace`, read by the caller and the person asked: its address.
    ///
    /// # Errors
    /// Why it could not be written, in words its caller reads.
    fn write_asked(
        &self,
        workspace: &str,
        caller: &super::Caller,
        request: &super::InvokeRequest,
        facts: &AskedFacts,
    ) -> Result<RecordRef, String>;

    /// Every asked record `person` (their root key) may answer, across the
    /// workspaces here, oldest first.
    fn asked_of(&self, person: &[u8; 32]) -> Vec<Asked>;

    /// Answered once an asked record here changes: what a live listing of
    /// [`Self::asked_of`] waits on before it reads again.
    fn changed(&self) -> super::Pending<()>;
}

/// A person's approve of an asked record, as the call kept in it reads it
/// when the dispatch admits it again ([`super::InvokeRequest::approval`]).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Approval {
    /// The asked record.
    pub record: RecordRef,
    /// The choice they picked, by its place among [`AskedFacts::choices`],
    /// for a question that offers choices.
    pub choice: Option<usize>,
}

/// An answer a person may give an asked record.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Offer {
    Approve,
    Decline,
    /// Approve, and grant the asker the action on every resource of its type.
    ApproveAndGrant,
}

impl Offer {
    /// What a question showing `facts` offers, in the order they are shown: approve, approve
    /// and grant where there is a resource type to name, and decline.
    #[must_use]
    pub fn of(facts: &AskedFacts) -> Vec<Self> {
        let mut offers = vec![Self::Approve];
        if facts.plural.is_some() && facts.choices.is_empty() {
            offers.push(Self::ApproveAndGrant);
        }
        offers.push(Self::Decline);
        offers
    }
}

/// Where the call was pressed, when a person's press asked it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pressed {
    /// The device the press was on, by its id.
    pub device: String,
    pub app: String,
    /// The window's tab key, as the session publishes it (`device.Window.key`).
    pub instance: String,
    pub resource: String,
}

/// One question that waits for the person, as [`WAITING`]`#`[`LIST`] lists it:
/// an asked invocation record they may answer with `<record>#resolve`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Asked {
    /// The record: what `resolve` is called on.
    pub record: RecordRef,
    /// The workspace the asked call acts in.
    pub workspace: String,
    /// That workspace by its name.
    pub workspace_name: String,
    /// The app whose action is asked.
    pub app: String,
    /// The action, `<app>#<action>`.
    pub action: String,
    /// The action by its published label.
    pub name: String,
    /// Since when it waits, in milliseconds since the Unix epoch.
    pub asked_at_ms: u64,
    #[serde(default)]
    pub pressed: Option<Pressed>,
    pub offers: Vec<Offer>,
    #[serde(flatten)]
    pub facts: AskedFacts,
}

/// Every question that waits for the person, oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AskedList {
    pub questions: Vec<Asked>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, reason = "in a test this is the assertion")]

    use super::*;

    /// A row reads as a client binds it: camel case, its facts beside the
    /// record's own names.
    #[test]
    fn a_row_reads_as_a_client_binds_it() {
        let row = Asked {
            record: RecordRef {
                workspace: "acme".into(),
                record: "invocations/7".into(),
            },
            workspace: "acme".into(),
            workspace_name: "Acme".into(),
            app: "clocks".into(),
            action: "clocks#set-alarm".into(),
            name: "Set alarm".into(),
            asked_at_ms: 9,
            pressed: None,
            offers: Vec::new(),
            facts: AskedFacts {
                verb: "set".into(),
                plural: Some("alarms".into()),
                ..AskedFacts::by(AskedBy::Agent(Named {
                    id: "scout".into(),
                    name: "Scout".into(),
                }))
            }
            .with_fact("At", "07:00"),
        };
        let row = Asked {
            offers: Offer::of(&row.facts),
            ..row
        };
        let json = serde_json::to_value(&row).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "record": {"workspace": "acme", "record": "invocations/7"},
                "workspace": "acme",
                "workspaceName": "Acme",
                "app": "clocks",
                "action": "clocks#set-alarm",
                "name": "Set alarm",
                "askedAtMs": 9,
                "pressed": null,
                "offers": ["approve", "approve_and_grant", "decline"],
                "by": {"agent": {"id": "scout", "name": "Scout"}},
                "words": "",
                "on": null,
                "facts": [{"label": "At", "value": "07:00"}],
                "destructive": false,
                "choices": [],
                "verb": "set",
                "plural": "alarms",
                "conversation": null,
            })
        );
        assert_eq!(serde_json::from_value::<Asked>(json).unwrap(), row);
    }

    /// The facts travel by position as a refusal does: every part is written
    /// whether or not it holds anything.
    #[test]
    fn facts_travel_by_position() {
        let facts = AskedFacts::by(AskedBy::Device(Named {
            id: "d".into(),
            name: "Laptop".into(),
        }))
        .with_fact("To", "https://visa.example");
        let wire = postcard::to_allocvec(&facts).unwrap();
        assert_eq!(postcard::from_bytes::<AskedFacts>(&wire).unwrap(), facts);
    }
}
