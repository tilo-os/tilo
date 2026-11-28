//! [`DynAction`] — a tilolang action as its invocation records it: what was invoked, with which
//! arguments, by whom.
//!
//! What the action wrote is not in it: the action ran, and its writes went to the device's
//! store, stamped with the action ([`crate::resources`]). The record is what the action API
//! answers an invocation with.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use tilo_actions::{Action, ActionError, ActionFacet, ActionRegistry, EncodedAction, ParamType};
use tilo_resource::Doc;

use tilolang_runtime::ids::{Actor, AppId};

/// The registry type id every [`DynAction`] is encoded under.
pub const ACTION_TYPE: &str = "tl.action";

/// The facet naming the authorship chain of a change, so history can say "you, via Alex's
/// view" (authority.md).
pub const AUTHORSHIP_FACET: &str = "tl.via";

/// The action id of a bound text input changing its field.
pub const EDIT_TEXT_ACTION: &str = tilolang_runtime::store::EDIT_TEXT_ACTION;

/// The action id of a bound checkbox changing its field.
pub const EDIT_FLAG_ACTION: &str = tilolang_runtime::store::EDIT_FLAG_ACTION;

/// The action ids of the three paragraph edits every resource of a type with a rich-text field
/// offers: one paragraph of it, named by the address block search publishes for it
/// (`tl:<app>:<id>#<field>.<place>`), is set, preceded or removed. Each is a splice of that
/// paragraph's text, never a write of the whole body: a concurrent edit of another paragraph
/// merges, and the change is one paragraph's in the note's history.
pub const EDIT_BLOCK_ACTION: &str = tilolang_runtime::store::EDIT_BLOCK_ACTION;
/// Puts paragraphs before the one the address names.
pub const INSERT_BLOCK_ACTION: &str = tilolang_runtime::store::INSERT_BLOCK_ACTION;
/// Removes the paragraph the address names, and the paragraphs it holds.
pub const DELETE_BLOCK_ACTION: &str = tilolang_runtime::store::DELETE_BLOCK_ACTION;

/// How a paragraph edit changes the paragraph its address names.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockEditKind {
    Replace,
    Insert,
    Delete,
}

/// One paragraph edit, as a resource of a type with a rich-text field offers it: its verb, what
/// the change reads as, whether it takes the paragraph's text (a deletion takes none), and how it
/// changes the paragraph ([`BlockEditKind`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockEdit {
    /// The verb a change of the paragraph names.
    pub id: &'static str,
    /// What the action reads as.
    pub label: &'static str,
    /// What the action does, for the catalogue and an agent reading the surface.
    pub description: &'static str,
    /// Whether it takes the paragraph's text, as Markdown.
    pub text: bool,
    /// How it changes the paragraph.
    pub kind: BlockEditKind,
}

/// The three paragraph edits ([`EDIT_BLOCK_ACTION`] and its two siblings).
pub const BLOCK_EDITS: [BlockEdit; 3] = [
    BlockEdit {
        id: EDIT_BLOCK_ACTION,
        label: "Edit a paragraph",
        description: "Set one paragraph of a rich-text field to what a Markdown text says.",
        text: true,
        kind: BlockEditKind::Replace,
    },
    BlockEdit {
        id: INSERT_BLOCK_ACTION,
        label: "Add a paragraph",
        description: "Put a paragraph before the one the address names.",
        text: true,
        kind: BlockEditKind::Insert,
    },
    BlockEdit {
        id: DELETE_BLOCK_ACTION,
        label: "Delete a paragraph",
        description: "Remove the paragraph the address names, and the paragraphs it holds.",
        text: false,
        kind: BlockEditKind::Delete,
    },
];

/// A bound control's edit of one field — a text input or a checkbox — as a resource offers it:
/// the API gives it an action ([`crate::publish`]), and the surface publishes it as the verb a
/// change of the field names ([`crate::surface`]). One declaration, so the arguments a change is
/// published with are the ones the surface's verb declares.
pub struct DirectEdit {
    /// The verb a change of the field names.
    pub id: &'static str,
    /// What the action reads as.
    pub label: &'static str,
    /// What the value it sets is.
    pub value: ParamType,
    /// Whether a change of the field names the value it set. A text edit's does not: what a
    /// person typed is the change itself, so its arguments name the field alone.
    pub names_value: bool,
}

/// The two direct edits every resource offers its fields.
pub const DIRECT_EDITS: [DirectEdit; 2] = [
    DirectEdit {
        id: EDIT_TEXT_ACTION,
        label: "Edit a text field",
        value: ParamType::Text,
        names_value: false,
    },
    DirectEdit {
        id: EDIT_FLAG_ACTION,
        label: "Set a flag field",
        value: ParamType::Boolean,
        names_value: true,
    },
];

/// The action id of a resource the host creates on its own, outside any action: a package's
/// per-workspace root, the first time it is wanted.
/// History reads it as the platform's creation of the resource.
pub const CREATE_ACTION: &str = tilo_actions::CREATE_ACTION;

/// The action id of what an outside origin writes: its service's answer, taken into the
/// resources it keeps in step ([`crate::origin`]).
pub const ORIGIN_ACTION: &str = tilolang_runtime::store::ORIGIN_ACTION;

/// The verb of what a package's scenario writes: its seed data, which no action of it makes.
pub const SCENARIO_ACTION: &str = tilolang_runtime::store::SCENARIO_ACTION;

/// The verb of what an `opened action` writes: what opening a resource does, which only the
/// platform runs.
pub const OPENED_ACTION: &str = tilolang_runtime::store::OPENED_ACTION;

/// The verb of what a `closed action` writes: what closing a window of a resource does, which only
/// the platform runs.
pub const CLOSED_ACTION: &str = tilolang_runtime::store::CLOSED_ACTION;

/// Who is behind the code that planned an action.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authorship {
    /// Who the code ran for; `None` for a change the app's package made by itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invoker: Option<Actor>,
    /// The content hashes of the code authors, outermost first: the module that wrote the
    /// expression that invoked the action, then the modules it passed through.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chain: Vec<String>,
    /// The module hash that declares the action.
    pub module: String,
    /// When the action was invoked, recorded like an argument: what reflection reads as the
    /// time of the versions it made (a scenario's `invoke(..., at: time)` sets it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub at: Option<String>,
    /// The resource the invocation was made from, when it was made from one: what the resources
    /// it created were made from ([`tilolang_runtime::store::Version::from`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from: Option<tilolang_runtime::value::ResourceRef>,
    /// How many items the change was made over ([`ChangeNote::count`]): one for an action
    /// applied to its target, as many as the items for one applied to many at once
    /// ([`tilolang_runtime::store::ChangeNote::count`]).
    #[serde(default = "one", skip_serializing_if = "crate::action::is_one")]
    pub count: u32,
    /// The run the change was made in, with every action it called: one name for the whole run,
    /// which the workspace commits as one action ([`invocation_id`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<String>,
    /// The gesture the change is one of: every change of one burst of
    /// typing into a field names the same one, which the workspace's history lists as one entry
    /// and undo takes back as one step. `None` for a change that is a step of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gesture: Option<tilo_actions::ActionId>,
    /// The members who alone read what the change wrote, when it wrote under a record that
    /// names its readers ([`tilolang_runtime::store::ChangeNote::readers`]): history lists the
    /// change to them alone ([`tilo_actions::READERS_FACET`]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readers: Option<Vec<tilo_actions::MemberId>>,
}

/// The invocation a run's changes name ([`Authorship::invocation`]) as the workspace writes it:
/// one id for every change of the run, the caller's and its callees', on whichever device made
/// it.
#[must_use]
pub fn invocation_id(run: &str) -> tilo_actions::InvocationId {
    tilo_actions::InvocationId(uuid::Uuid::new_v5(
        &uuid::Uuid::NAMESPACE_OID,
        run.as_bytes(),
    ))
}

/// The count of a change made over its one target.
fn one() -> u32 {
    1
}

/// Whether the change was made over its one target.
#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "`skip_serializing_if` calls it with the field by reference"
)]
pub(crate) fn is_one(count: &u32) -> bool {
    *count == 1
}

impl Authorship {
    /// The facets that carry this chain in the change's `ActionContext`, so history can say
    /// "you, via Alex's view".
    #[must_use]
    pub fn facets(&self) -> Vec<ActionFacet> {
        self.chain
            .iter()
            .map(|module| ActionFacet::new(AUTHORSHIP_FACET, module.as_str()))
            .collect()
    }
}

/// A tilolang action as its invocation records it: what was invoked, by whom.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DynAction {
    pub app: String,
    /// The resource kind the action is published on (`tl.<app>.<Type>`).
    pub kind: String,
    pub action_id: String,
    /// The invocation arguments, as plain JSON, for history and agents.
    pub args: Json,
    pub authorship: Authorship,
}

impl DynAction {
    /// Who made the action: its invoker, or its app acting by itself when it names nobody.
    #[must_use]
    pub fn invoker(&self) -> Actor {
        self.authorship
            .invoker
            .clone()
            .unwrap_or_else(|| Actor::App(AppId::from(self.app.as_str())))
    }

    /// Encode under [`ACTION_TYPE`], the framework's action encoding.
    ///
    /// # Errors
    /// When the arguments do not encode.
    pub fn encode(&self) -> Result<EncodedAction, ActionError> {
        EncodedAction::encode(ACTION_TYPE, self)
    }
}

impl Action for DynAction {
    /// Nothing: the action ran when it was planned, and its writes are in the device's store
    /// already. The invocation records what was invoked, for history and the API.
    fn apply(&self, _doc: &mut Doc) -> Result<(), ActionError> {
        Ok(())
    }

    fn readers(&self) -> Option<Vec<tilo_actions::MemberId>> {
        self.authorship.readers.clone()
    }
}

/// Register the decoder of [`DynAction`], so the API reads an invocation's record.
pub fn register_actions(registry: &mut ActionRegistry) {
    registry.register::<DynAction>(ACTION_TYPE);
}
