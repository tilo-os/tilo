//! What a resource store's change names in its message: the action that made it, which every
//! holder judges the change against, and which history and undo group it by.

use serde::{Deserialize, Serialize};

/// The action a change was made by, and the membership its author saw: what a holder judges
/// the change against. It travels in the change itself.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stamp {
    /// The verb invoked.
    pub verb: String,
    /// Who invoked it: a person, or an agent acting for one, as [`Stamp::by`] writes the actor.
    /// History names the change's author by it, so it is the actor every reader parses back
    /// ([`Stamp::invoked_by`]), never a bare id.
    pub invoker: String,
    /// The person the invoker acted for, when someone acted for a person and the invoker is not
    /// one (an agent): the records it makes are that person's, so a record under
    /// `readers: author` is read by them and their devices, as one made by them directly is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acted_for: Option<String>,
    /// The invocation: every change it made, in every document, names it, so history and undo
    /// take them as one step.
    pub invocation: String,
    /// The heads of the membership the author saw when it wrote the change, as the membership
    /// names them: who may do what is judged as of there, not as of the receiver's now.
    pub membership: Vec<String>,
    /// The invocation this one takes back, when it is an undo: it names that invocation's verb.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undoes: Option<String>,
    /// When it was invoked, ISO-8601 UTC, recorded like an argument so every holder reads the
    /// same; empty where the invoker named no time.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub at: String,
    /// The resource the invocation was made from, when it was made from one (a task made from a
    /// message): what the resources it made read as made from, on every holder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<Origin>,
    /// The work the invocation is one step of, when it is one of several taken together (an
    /// agent's session): what a review of that work lists, and what reading its changes groups.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<Group>,
    /// Who proposed the invocation, when the invoker accepted someone's suggestion: the change
    /// is the invoker's, and it says who proposed it.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub suggested_by: String,
    /// The arguments the action was invoked with, in the order it declares them, as the runtime
    /// recorded them: what a program following the change reads as the verb's arguments. Empty
    /// where the change names no arguments of its own (an edit, an undo).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<serde_json::Value>,
    /// When the writer wrote it, by its clock, as a number that orders: the order a resource's
    /// versions read in across the documents it is kept in, and what tells two writes of one
    /// invocation apart. Zero where the writer kept none.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub moment: u64,
    /// The invocation as its caller signed it, when a host ran the action for someone who asked
    /// it under their own key: the device that wrote the change signs it as its own, and this
    /// says who asked, for what, with what arguments. A holder checks it against the person's
    /// root ([`tilo_actions::remote::SignedCall`]); the host never makes one up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signed: Option<Box<tilo_actions::remote::SignedCall>>,
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde passes the field by reference"
)]
const fn is_zero(moment: &u64) -> bool {
    *moment == 0
}

/// A resource an invocation was made from: its kind and id.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Origin {
    pub kind: String,
    pub id: String,
}

/// Several invocations taken together, as one piece of work: its id, and what it was called
/// when it ran.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Group {
    pub id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub label: String,
}

impl Stamp {
    /// The verb of a change an app's outside origin wrote (`$origin`): what its service said,
    /// taken into the resources of the types it keeps. A store keeps no facets, so a change
    /// stamped with it is how a received message reaches a trigger.
    pub const ORIGIN_VERB: &str = "$origin";

    /// The text a stamp names `actor` by as its invoker.
    #[must_use]
    pub fn by(actor: &tilo_action_policy::Actor) -> String {
        serde_json::to_string(actor).unwrap_or_default()
    }

    /// The message a change carries it as.
    #[must_use]
    pub fn message(&self) -> String {
        serde_json::to_string(self).unwrap_or_default()
    }

    /// The stamp a change's message `message` names; `None` for one that names none.
    #[must_use]
    pub fn of_message(message: &str) -> Option<Self> {
        serde_json::from_str(message).ok()
    }

    /// Who invoked it: the actor the writer named, or the person the workspace's resource route
    /// stamped on a person's own write (their person id alone). Nobody for a stamp naming
    /// neither.
    #[must_use]
    pub fn invoked_by(&self) -> Option<tilo_action_policy::Actor> {
        // An empty invoker names nobody: it must not read as a person with no id.
        if self.invoker.is_empty() {
            return None;
        }
        if let Ok(actor) = tilo_failure::ignored!(
            "a stamp the route wrote holds a person id, which is read next",
            serde_json::from_str::<tilo_action_policy::Actor>(&self.invoker)
        ) {
            return Some(actor);
        }
        tilo_failure::ignored!(
            "a stamp naming neither an actor nor a person names nobody",
            tilo_action_policy::PersonId::parse(&self.invoker)
        )
        .ok()
        .map(|person| {
            tilo_action_policy::Actor::Member(tilo_action_policy::MemberId::person(person))
        })
    }

    /// The stamp `change` carries; `None` for a change that carries none.
    #[must_use]
    pub fn of(change: &automerge::Change) -> Option<Self> {
        Self::of_message(change.message()?)
    }
}
