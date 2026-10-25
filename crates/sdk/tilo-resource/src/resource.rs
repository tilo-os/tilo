//! The resource contract: what any resource type provides so that it gets
//! history, undo, collaboration, permissions, palette/agent/script exposure,
//! search and replay without writing any of them.

use crate::search_preview::SearchPreview;
use std::any::TypeId;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use serde::Serialize;
use serde::de::DeserializeOwned;
use tilo_actions::{
    ActionArgs, ActionError, ActionNeed, ActionParam, ActionRegistry, ActionSpec, EncodedAction,
    ExecutionLocus, OperationKind, ParamType,
};

use crate::action::ActionKind;
use crate::delta::Delta;
use crate::model::{Doc, ModelError};
use crate::path::Edit;
use crate::reader::Demand;
use crate::vocabulary::{Builds, Built, Declaration, Vocabulary, vocabulary};
use tilo_lock::MutexRecover as _;

mod head;
pub use head::{HeadInputs, head_inputs, publish_head, publish_head_of};

/// A resource type. `#[resource]` implements it for a `#[derive(Model)]`
/// record, which is how every app declares its own. The SDK implements it by
/// hand for the shared types with a storage layout of their own (a rich-text
/// document, plain text), which apps hold as fields; an app never does
/// (docs/architecture/resources.md §8).
///
/// The contract is deliberately small:
///
/// * [`read`](Self::read) — materialise the state views render, from the live
///   document *or* from any historical version (time travel is just `read` on
///   an older document);
/// * [`refresh`](Self::refresh) — optionally, bring a state up to date after a
///   change by re-reading only what the change touched, so an edit costs as
///   much as the edit rather than as much as the resource;
/// * [`load`](Self::load) — optionally, read on demand the parts of a large
///   resource that views are looking at (the pages of a long document on
///   screen), so opening or refreshing it never reads all of it;
/// * [`Action`](Self::Action) — the replayable mutation vocabulary: a
///   serializable value whose `apply` is a pure function of the document;
/// * [`declarations`](Self::declarations) — which mutations are offered by
///   name to the palette, agents and scripts, with typed parameters.
///
/// Everything else has a default.
pub trait Resource: Sized + 'static {
    /// Stable kind of record this resource lives in, e.g. `"notes.notebook"`.
    const KIND: &'static str;
    /// Human name of one resource of this kind, e.g. `"Notebook"`.
    const LABEL: &'static str;
    /// The registry type id every action of this resource is replayed under.
    const ACTION_TYPE: &'static str = Self::KIND;

    /// What views render. For a record resource this is the record itself.
    type State: 'static;
    /// The replayable mutation. Must be deterministic: no clock, no RNG, no
    /// local state — use [`ActionCx`](crate::ActionCx) for ids and time.
    type Action: tilo_actions::Action + Serialize + DeserializeOwned + Clone + 'static;

    /// Read the state from a document.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold this resource.
    fn read(doc: &Doc) -> Result<Self::State, ModelError>;

    /// Bring `state`, read earlier from this document, up to date after the
    /// changes in `delta`. Re-read only what they touched: this runs on every
    /// keystroke, local or a collaborator's. The default reads everything
    /// again, which is right for small resources.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold this resource.
    fn refresh(state: &mut Self::State, doc: &Doc, delta: &Delta) -> Result<(), ModelError> {
        if !delta.is_empty() {
            *state = Self::read(doc)?;
        }
        Ok(())
    }

    /// A copy of `state` for [`refresh`](Self::refresh) to bring up to date
    /// while the state the reader holds is shared (a view still holds the
    /// last one): refreshing a copy reads only what a change touched, where
    /// without one the state is read whole. `#[resource]` copies a `Clone`
    /// record; the default has none.
    fn copy_state(_state: &Self::State) -> Option<Self::State> {
        None
    }

    /// Opt in to reusing the document-owned publication between changes.
    ///
    /// Return an epoch for every non-document input used by declarations,
    /// children, views or local operations (for example the local date of a
    /// daily-note action). The framework also invalidates on state and locale
    /// revisions; surface bindings and effect availability remain live.
    /// `None` preserves publication on every request for dynamic resources.
    fn publication_cache_epoch(_state: &Self::State) -> Option<String> {
        None
    }

    /// Where `obj` sits in the document, when the state already knows: the
    /// path from the root to it. A change to an object the state has read
    /// (the paragraph being typed in) is then placed without asking the
    /// document, which for a large document is the costly part of reading a
    /// change. The default knows nothing, and the document is asked.
    fn locate(state: &Self::State, obj: &automerge::ObjId) -> Option<crate::delta::ObjPath> {
        let _ = (state, obj);
        None
    }

    /// Make sure the parts of the state views asked for are read, and drop
    /// parts nobody asks for any more. For a resource too large to read
    /// whole, [`read`](Self::read) reads a spine (the order of a document's
    /// paragraphs, the rows of a sheet) and this reads the parts on screen.
    /// Called after every read and refresh and whenever the demand changes,
    /// so it must be cheap when everything demanded is already there. The
    /// default reads nothing: the state is read whole.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold this resource.
    fn load(state: &mut Self::State, doc: &Doc, demand: &Demand) -> Result<(), ModelError> {
        let _ = (state, doc, demand);
        Ok(())
    }

    /// Write the initial content of a brand-new record: its genesis.
    ///
    /// The genesis is made under an actor derived from the record's identity,
    /// at time zero, so it is the same change byte for byte on every device:
    /// two people who each create the same record before they sync (a room's
    /// default record, the record of one file) converge on one record rather
    /// than two competing roots. It must therefore depend on nothing but the
    /// resource type — not on who creates it, the device, the clock or a
    /// file. Content that does depend on those (the creator as owner, a
    /// first-run import of settings that already exist) is the app's seed,
    /// applied as actions right after the genesis (`ResourceApp::seed` in
    /// `tilo-app`).
    ///
    /// # Errors
    /// Returns the document's write error.
    fn initialize(_doc: &mut Doc) -> Result<(), ModelError> {
        Ok(())
    }

    /// Whether `next`, applied right after `previous` by the same person,
    /// continues it as one undo step — the characters of a word typed one
    /// keystroke at a time, the values a slider passes through in one drag.
    /// The default keeps every action its own step.
    ///
    /// Immediate edits (`Dispatcher::apply_now` in `tilo-app`) and native
    /// keyboard/IME edits from action-bound text fields may continue one
    /// another. Programmatic/agent invocations remain separate steps.
    /// A surface ends an immediate pointer gesture
    /// (`Dispatcher::settle`) when the pointer is let go, so the next drag
    /// of the same slider is a step of its own.
    fn coalesces(previous: &Self::Action, next: &Self::Action) -> bool {
        let _ = (previous, next);
        false
    }

    /// Whether a later action may continue `action` ([`coalesces`](Self::coalesces)),
    /// so its undo step stays open for it. One that never continues — a
    /// paragraph split after a typed word, a block moved — is a step of its
    /// own, recorded at once. The default keeps every step open for what may
    /// follow.
    fn continuable(action: &Self::Action) -> bool {
        let _ = action;
        true
    }

    /// Where `action`, just applied, leaves the participant who took it, read
    /// off the state after it: the paragraph an append wrote and the caret
    /// at its end. Every collaborator's view draws that participant there,
    /// so one with no pointer or caret of its own (an agent) shows where it
    /// works. The default places it nowhere in particular.
    fn lands(state: &Self::State, action: &Self::Action) -> Option<Place> {
        let _ = (state, action);
        None
    }

    /// The one field of the record `action` writes, when it writes only one:
    /// a reply or a note writes the `comments`, so a grant on that field
    /// (what someone who may only comment holds) reaches it. `None` by
    /// default. A declared action that builds it names the same field with
    /// [`ActionDecl::below_field`].
    fn field_of(action: &Self::Action) -> Option<&'static str> {
        let _ = action;
        None
    }

    /// The least any of the resource's actions is ([`kind_of`](Self::kind_of),
    /// [`field_of`](Self::field_of)): what the generic [`APPLY`] and group
    /// actions are offered under, each invocation then decided by what its
    /// value is. An undoable change of the record by default; an undoable
    /// change of its `comments` for a resource someone who may only comment
    /// can still reply in.
    fn least_need() -> ActionNeed {
        least_need_declared(&vocabulary::<Self>().declarations())
    }

    /// Mutations offered by name. The generic [`APPLY`] action is always
    /// offered in addition, so any typed value can reach the host.
    fn declarations() -> Vec<ActionDecl<Self>> {
        Vec::new()
    }

    /// The JSON Schema of [`Action`](Self::Action)'s serde form, published as
    /// the schema of the generic [`APPLY`] operation's argument, so agents
    /// and scripts can read the edit vocabulary from the descriptor and
    /// binding refuses a malformed edit by name. A variant that only
    /// restates a declaration is published as a reference to it. Return `Some(<Self::Action as
    /// JsonSchema>::json_schema())` for an action type deriving
    /// [`JsonSchema`](crate::JsonSchema); `#[resource]` describes its
    /// operations from their declarations. The default publishes no schema.
    fn action_schema() -> Option<serde_json::Value> {
        None
    }

    /// Resources nested inside this one (a track's clips, a list's items,
    /// a page's rich-text body), each exporting actions with their arguments
    /// already bound. Published beneath the root resource for the palette,
    /// agents and scripts — the same door as the root, not a second tree.
    fn children(_state: &Self::State) -> Vec<tilo_actions::Resource> {
        Vec::new()
    }

    /// The effects offered on the item `item` of the collection `field`,
    /// bound to it (`#[resource(effects = E)]` with `#[effect(item = field)]`
    /// variants): published on the item resource beside its item verbs.
    fn item_effects(_field: &str, _item: &crate::Id) -> Vec<tilo_actions::ActionSpec> {
        Vec::new()
    }

    /// The views the item `item` of the collection `field` offers
    /// (`#[resource(item_views = path)]`): published on the item resource,
    /// so a link to the item opens it in one, each opened by one of the
    /// item's own actions ([`item_effects`](Self::item_effects)).
    fn item_views(_field: &str, _item: &crate::Id) -> Vec<tilo_actions::ResourceViewSpec> {
        Vec::new()
    }

    /// [`children`](Self::children) split into the children that are not
    /// items and the items of the record's collections, so a publisher can
    /// keep the item children it built for an earlier version and build
    /// only those whose label changed. `others` followed by
    /// [`item_children`](crate::items::item_children) of the rest must be
    /// exactly `children`. The default puts every child in `others`;
    /// `#[resource]` splits off its item collections.
    fn children_parts(state: &Self::State) -> crate::items::ChildrenParts {
        crate::items::ChildrenParts {
            others: Self::children(state),
            kept: std::rc::Rc::default(),
            items: Vec::new(),
            item_actions: Vec::new(),
        }
    }

    /// The ids of the [`declarations`](Self::declarations) that
    /// [`children`](Self::children) offer on some child with arguments
    /// already bound (an item action on each item of a list), which the
    /// resource then publishes for scripts rather than the palette.
    ///
    /// [`publish_head`] asks this to publish the resource's own actions
    /// without building its children, so a resource with many children
    /// answers from what it knows about them: `#[resource]` from how many
    /// items each collection holds. The default builds the children and
    /// looks.
    fn children_offer_bound(state: &Self::State) -> Vec<&'static str> {
        offered_bound_ids(&Self::children(state), &vocabulary::<Self>().declarations())
    }

    /// The nested resource `id`, when [`children`](Self::children) leaves it
    /// out. A resource with more children than a published tree should carry
    /// (a book of contacts, a long history) publishes a bounded page from
    /// `children` and resolves the rest here, so an action addressed to any
    /// of them still reaches it through the same door. The default resolves
    /// nothing beyond `children`.
    fn child(_state: &Self::State, _id: &str) -> Option<tilo_actions::Resource> {
        None
    }

    /// View and effect operations on this resource: local presentation and
    /// jobs that have no document delta (show a week, start a sync drain,
    /// open a system file picker). They are published on the same tree as
    /// [`declarations`](Self::declarations). A document mutation does not
    /// belong here — it is a [`declarations`](Self::declarations) entry or
    /// [`APPLY`]. `publish` drops a `Document` spec so it cannot become a
    /// second write path.
    fn local_operations() -> Vec<tilo_actions::ActionSpec> {
        Vec::new()
    }

    /// Named presentations of this resource (editor, reader) with stable ids.
    /// Not an action called `open`: ordinary presentation toggles never become
    /// composable views by accident. Defaults to none.
    fn views(_state: &Self::State) -> Vec<tilo_actions::ResourceViewSpec> {
        Vec::new()
    }

    /// Entries for the system search index.
    fn search_scopes(_state: &Self::State) -> Vec<ScopeDecl> {
        Vec::new()
    }

    fn search(_state: &Self::State) -> Vec<SearchEntry> {
        Vec::new()
    }

    /// Whether [`read_content`](Self::read_content) answers. A resource that
    /// does is published with the framework's `read` operation, so a script,
    /// an agent or an assistive tool reads what it holds as typed data through
    /// the same door, permissions and audit as every other action — never
    /// from a screenshot.
    const READABLE: bool = false;

    /// Whether this exact child is part of the public document projection
    /// answered by `read_content`. Defaults to false even for readable parents.
    /// This is a structural declaration, not a permission check: it must not
    /// perform IO, inspect private view state, or probe by executing a read.
    /// Eligibility must remain stable while the child's identity exists.
    fn document_child(_state: &Self::State, _id: &str) -> bool {
        false
    }

    /// What the resource holds, as JSON a caller can act on: with no `item`,
    /// the resource itself (a notebook's pages); with `item`, the nested
    /// resource of that id from [`children`](Self::children) (one page's
    /// title and blocks). Read from `doc`, the committed document, so the
    /// answer is never older than the action that preceded it. Called only
    /// when [`READABLE`](Self::READABLE) is set.
    ///
    /// # Errors
    /// A message naming why: no such item, or a document that cannot be read.
    fn read_content(
        state: &Self::State,
        doc: &Doc,
        item: Option<&str>,
    ) -> Result<serde_json::Value, String> {
        let _ = (state, doc, item);
        Err(format!("a {} does not answer reads", Self::LABEL))
    }

    /// Whether [`facts`](Self::facts) answers. A resource that does is
    /// published with the framework's `facts` read, which an agent's host
    /// shows when it checks work done on the resource.
    const HAS_FACTS: bool = false;

    /// The record's own table, for a resource that is a record: what
    /// generic code reads a field's value through without the record's type
    /// (the framework, a window's [`Reactions`](crate::Reactions) with
    /// [`record_reactions`](crate::reactions::record_reactions)).
    /// `#[resource]` names the record's; none otherwise.
    const RECORD_TABLE: Option<&'static crate::table::ModelTable> = None;

    /// What the resource holds as a compact summary of plain values, meant
    /// for checking it against a request: names, times, counts and text as
    /// a person would state them (a cut's shots with their source, in and
    /// out points; a deck's slide titles), not ids or storage. Two answers
    /// compare field by field, so keep a list's order meaningful and its
    /// items' keys stable. Called only when [`HAS_FACTS`](Self::HAS_FACTS)
    /// is set.
    ///
    /// # Errors
    /// A message naming why the facts could not be read.
    fn facts(state: &Self::State, doc: &Doc) -> Result<serde_json::Value, String> {
        let _ = (state, doc);
        Err(format!("a {} states no facts", Self::LABEL))
    }

    /// What declared action `action` (one [`ActionDecl::answering`]) made,
    /// answered to its caller once it applied to `doc` as `changes`: the
    /// ids it minted, the area it selected, a warning that what it drew is
    /// hidden. A compact summary, so the caller needs no second read to
    /// see what happened. The default answers nothing.
    fn answer(
        _doc: &Doc,
        _action: &str,
        _changes: &[automerge::Patch],
    ) -> Option<serde_json::Value> {
        None
    }

    /// The stored layout of this resource's content, for services that act
    /// on it generically: History restores a past version through it, field
    /// by field and item by item. `#[resource]` returns the record's
    /// [`Model::SHAPE`](crate::Model::SHAPE); the default opts out.
    fn shape() -> Option<crate::Shape> {
        None
    }

    /// The state as its record's JSON ([`Model::to_json`](crate::Model::to_json)):
    /// what a field that weakens one way holds now. `#[resource]` returns
    /// the record's; the default knows none, so any change of such a field
    /// counts as weakening.
    fn state_json(state: &Self::State) -> Option<serde_json::Value> {
        let _ = state;
        None
    }

    /// The storage version documents of this resource are written at;
    /// `#[resource(version = N)]` raises it with each incompatible change
    /// and [`Self::migrate`] brings older documents up to it.
    const VERSION: u32 = 1;

    /// Bring a document stored at version `from` up to [`Self::VERSION`],
    /// reading its old shape with the model helpers. Called once per
    /// document by [`crate::evolution::migrate`].
    ///
    /// # Errors
    /// Returns why the document cannot be migrated.
    fn migrate(doc: &mut Doc, from: u32) -> Result<(), ModelError> {
        let _ = doc;
        Err(ModelError::invalid(format!(
            "{} has no migration from storage version {from}",
            Self::LABEL
        )))
    }

    /// The public surface: verbs, their parameters and kinds, and the
    /// stored fields ([`crate::surface`]).
    #[must_use]
    fn surface() -> crate::surface::Surface {
        crate::surface::surface_of::<Self>()
    }

    /// Whether the resource's actions include field edits ([`Edit`]),
    /// published as [`FIELD_EDIT`]. `#[resource]` records have them.
    const EDITS: bool = false;

    /// The action a field edit of this resource is, when it has field edits
    /// ([`Self::EDITS`]).
    fn edit_action(edit: Edit<Self>) -> Option<Self::Action> {
        let _ = edit;
        None
    }

    /// Stamp the origin's clock on an action as the door admits it. A caller
    /// never supplies the time an action carries.
    fn stamp(action: &mut Self::Action, now_ms: i64) {
        let _ = (action, now_ms);
    }

    /// The schemas of the own vocabularies of the record's fields
    /// ([`Operations`](crate::Operations)), by field path pattern (`*` for
    /// every item): what a field edit in that vocabulary carries.
    fn op_schemas() -> Vec<(String, serde_json::Value)> {
        Vec::new()
    }

    /// Whether the declared action `action` can be taken in `state`, and why
    /// not when it cannot. `#[resource(available = path)]` answers it.
    fn availability(state: &Self::State, action: &str) -> tilo_actions::Availability {
        let _ = (state, action);
        tilo_actions::Availability::Available
    }

    /// The first content of a new record, written by its creator once it is
    /// created: nobody's undo step. `#[resource(seed = path)]` provides it.
    fn seed() -> Option<Self::Action> {
        None
    }

    /// What kind of action `action` is: whether it only reads, changes only
    /// the document, or reaches outside it. Derived by `#[resource]` from
    /// what each verb is able to do, never declared.
    fn kind_of(action: &Self::Action) -> ActionKind {
        let _ = action;
        ActionKind::Undoable
    }

    /// Register this resource's replay decoder. Override to register an
    /// existing vocabulary under additional type ids.
    fn register(registry: &mut ActionRegistry) {
        registry.register::<Self::Action>(Self::ACTION_TYPE);
    }
}

/// The id of the action every resource publishes that applies one typed
/// [`Resource::Action`] given as JSON. It is how a custom surface, an agent or
/// a script submits an edit that has no named declaration (a drag with its
/// final geometry) through the same permission-checked, undoable path.
///
/// It is for agents ([`tilo_actions::Audience::Agents`]): never offered in
/// the palette or a menu, where a
/// person picks named actions. Its risk is the value's own
/// ([`tilo_actions::Action::risk`]), so a destructive value — deleting a clip —
/// needs the same confirmation a declared destructive action does, while an
/// ordinary edit applies directly.
pub const APPLY: &str = "apply";
/// The parameter of [`APPLY`] carrying the JSON-encoded action.
pub const APPLY_PARAM: &str = "action";

/// The id of the action a resource with field edits ([`Resource::EDITS`])
/// publishes for them: one [`Edit`] given as JSON, a path and an operation.
pub const FIELD_EDIT: &str = "field-edit";
/// The parameter of [`FIELD_EDIT`] carrying the edit.
pub const EDIT_PARAM: &str = "edit";

/// Read a field edit given as JSON into the action it is.
fn read_edit(vocabulary: &Vocabulary, value: serde_json::Value) -> Result<Built, String> {
    let edit = Edit::<()>::from_json(value)?;
    match (vocabulary.edit)(edit) {
        Some(action) => Ok(action),
        None => Err(format!("{} has no field edits", vocabulary.label)),
    }
}

/// The schema of a field edit of `R`: one alternative per editable path of
/// its stored shape — the path's steps, the operations its type takes, the
/// value it holds, its title and flags — so an agent builds a valid edit
/// from the published descriptor alone. A resource without a shape gets
/// the generic one, with its fields' own operations named for `rich`.
pub(crate) fn edit_schema(vocabulary: &Vocabulary) -> serde_json::Value {
    let ops = (vocabulary.op_schemas)();
    if let Some(shape) = (vocabulary.shape)()
        && !shape.fields().is_empty()
    {
        let mut alternatives = Vec::new();
        edit_paths(shape, &[], "", &ops, &mut alternatives);
        if !alternatives.is_empty() {
            return described_edit(&alternatives, &ops);
        }
    }
    generic_edit_schema(&ops)
}

/// The schema a field edit is checked against, with every editable path
/// described: its template, the operations it takes, the value's schema,
/// its title and flags (the description), and one example edit per path
/// (the examples) — what an agent builds a field edit from.
fn described_edit(
    alternatives: &[serde_json::Value],
    ops: &[(String, serde_json::Value)],
) -> serde_json::Value {
    let mut schema = generic_edit_schema(ops);
    let mut lines = vec![
        "A field edit: `path` from the record (fields by name, items by id, map entries by key) and `op` with its arguments. Editable paths:".to_owned(),
    ];
    let mut examples = Vec::new();
    let mut shapes: Vec<Vec<String>> = Vec::new();
    for alternative in alternatives {
        let at = |pointer: &str| alternative.pointer(pointer).cloned().unwrap_or_default();
        let template = at("/properties/path/examples/0");
        let ops: Vec<String> = at("/properties/op/enum")
            .as_array()
            .map(|ops| {
                ops.iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let value = at("/properties/value");
        let text = |key: &str| at(key).as_str().unwrap_or_default().to_owned();
        let kind = value_kind(&value);
        lines.push(format!(
            "- `{}`: {} — {}{}",
            text("/title"),
            ops.join(", "),
            text("/description"),
            kind.map_or_else(String::new, |kind| format!(" ({kind})")),
        ));
        // One example per shape of path (a field, an item's field, an
        // entry): the listing names every path of each shape.
        let shape: Vec<String> = template
            .as_array()
            .map(|steps| {
                steps
                    .iter()
                    .filter_map(|step| step.as_object()?.keys().next().cloned())
                    .collect()
            })
            .unwrap_or_default();
        if shapes.contains(&shape) {
            continue;
        }
        shapes.push(shape);
        let mut example = serde_json::Map::new();
        example.insert("path".into(), template);
        if let Some(op) = ops.first() {
            example.insert("op".into(), op.clone().into());
        }
        examples.push(serde_json::Value::Object(example));
    }
    if let Some(object) = schema.as_object_mut() {
        object.insert("description".into(), lines.join("\n").into());
        object.insert("examples".into(), examples.into());
    }
    schema
}

/// The kind of value a schema describes, in a word or two, when it says one
/// (`string`, `number`, `one of a, b`): what a path's line in the field-edit
/// listing tells beside its operations. Objects are described by their own
/// paths.
fn value_kind(schema: &serde_json::Value) -> Option<String> {
    if let Some(options) = schema.get("enum").and_then(serde_json::Value::as_array) {
        let options: Vec<String> = options
            .iter()
            .map(|option| {
                option
                    .as_str()
                    .map_or_else(|| option.to_string(), str::to_owned)
            })
            .collect();
        return Some(format!("one of {}", options.join(", ")));
    }
    match schema.get("type")? {
        serde_json::Value::String(kind) if kind != "object" => Some(kind.clone()),
        serde_json::Value::Array(kinds) => Some(
            kinds
                .iter()
                .filter_map(serde_json::Value::as_str)
                .collect::<Vec<_>>()
                .join(" or "),
        ),
        _ => None,
    }
}

/// The generic field-edit schema, with the vocabularies of the record's
/// fields' own operations named for `rich`.
fn generic_edit_schema(ops: &[(String, serde_json::Value)]) -> serde_json::Value {
    let ops = ops.to_vec();
    let mut schema = <Edit<()> as crate::JsonSchema>::json_schema();
    if !ops.is_empty()
        && let Some(properties) = schema
            .get_mut("properties")
            .and_then(serde_json::Value::as_object_mut)
    {
        // A field type's own vocabulary (rich text's) can outweigh the
        // whole listing: it is summarised by its operations and their
        // arguments, and checked when the edit applies.
        let alternatives = ops
            .into_iter()
            .map(|(path, schema)| {
                serde_json::json!({
                    "description": format!(
                        "An edit of `{path}` in its own vocabulary, one of: {}",
                        crate::schema::summary(&schema)
                    ),
                })
            })
            .collect();
        properties.insert("rich".into(), crate::schema::one_of(alternatives));
    }
    schema
}

fn step_schema(kind: &str, value: serde_json::Value) -> serde_json::Value {
    let mut properties = serde_json::Map::new();
    properties.insert(kind.to_owned(), value);
    serde_json::json!({
        "type": "object",
        "properties": properties,
        "required": [kind],
    })
}

/// The operations a field of `field`'s type takes, by its stored shape and
/// type.
fn field_ops(field: &crate::FieldShape, rich: bool) -> Vec<&'static str> {
    let ty = field.ty;
    match field.shape {
        crate::Shape::Collection(_) | crate::Shape::Keyed(_) => {
            vec![
                "upsert-item",
                "insert-item",
                "remove-item",
                "move-to",
                "set",
            ]
        }
        _ if rich => vec!["rich"],
        _ if ty.starts_with("Counter") => vec!["add", "set"],
        _ if ty.starts_with("Set<") => vec!["insert", "remove", "set"],
        _ if ty.starts_with("Grid<") => vec![
            "set-cell",
            "insert-row",
            "insert-column",
            "remove-row",
            "remove-column",
        ],
        _ if ty.starts_with("Outbox") => vec!["settle", "forget"],
        crate::Shape::Map(_) => vec!["set", "remove-key"],
        _ if ty == "String" => vec!["set", "splice", "replace"],
        _ => vec!["set"],
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one walk over the stored shape, an alternative per kind of path"
)]
fn edit_paths(
    shape: crate::Shape,
    steps: &[serde_json::Value],
    dotted: &str,
    rich: &[(String, serde_json::Value)],
    out: &mut Vec<serde_json::Value>,
) {
    let alternative = |steps: &[serde_json::Value],
                       open: bool,
                       ops: &[&str],
                       value: serde_json::Value,
                       rich: Option<&serde_json::Value>,
                       title: &str,
                       description: String| {
        // The path as an agent writes it, items by a placeholder id.
        let template: Vec<serde_json::Value> = steps
            .iter()
            .map(|step| {
                let properties = step.get("properties").and_then(serde_json::Value::as_object);
                match properties.and_then(|properties| properties.iter().next()) {
                    Some((kind, value)) if kind == "field" => {
                        serde_json::json!({ "field": value.get("const").cloned().unwrap_or_default() })
                    }
                    Some((kind, _)) => serde_json::json!({ kind.clone(): format!("<{kind}>") }),
                    None => serde_json::Value::Null,
                }
            })
            .collect();
        let dotted: Vec<String> = template
            .iter()
            .map(
                |step| match step.get("field").and_then(serde_json::Value::as_str) {
                    Some(field) => field.to_owned(),
                    None => "*".to_owned(),
                },
            )
            .collect();
        let mut path = serde_json::json!({
            "type": "array",
            "prefixItems": steps,
            "minItems": steps.len(),
            "examples": [template],
        });
        if let Some(path) = path.as_object_mut() {
            if open {
                // What is inside the entry or cell, by any steps.
                path.insert("items".into(), serde_json::json!({ "type": "object" }));
            } else {
                path.insert("maxItems".into(), steps.len().into());
            }
        }
        let mut properties = serde_json::Map::new();
        properties.insert("path".into(), path);
        properties.insert("op".into(), serde_json::json!({ "enum": ops }));
        properties.insert("value".into(), value);
        if ops.contains(&"upsert-item") {
            properties.insert(
                "match".into(),
                serde_json::json!({
                    "type": "object",
                    "description": "upsert-item: the fields that identify the item, e.g. {\"name\": \"Ana\"}; it updates the one item holding them with `value`'s fields, else appends `value` as a new item"
                }),
            );
            properties.insert(
                "at".into(),
                serde_json::json!({
                    "description": "insert-item, move-to: \"end\" (the default), {\"after\": id} or {\"before\": id}",
                    "anyOf": [
                        { "const": "end" },
                        { "type": "object", "properties": { "after": { "type": "string" } }, "required": ["after"] },
                        { "type": "object", "properties": { "before": { "type": "string" } }, "required": ["before"] },
                        { "type": "integer", "minimum": 0 }
                    ]
                }),
            );
        }
        if let Some(rich) = rich {
            properties.insert("rich".into(), rich.clone());
        }
        serde_json::json!({
            "title": dotted.join("."),
            "description": if description.is_empty() {
                title.to_owned()
            } else {
                format!("{title}; {description}")
            },
            "type": "object",
            "properties": properties,
            "required": ["path", "op"],
        })
    };
    let item = step_schema("item", serde_json::json!({ "type": "string" }));
    // A read-only field is changed only by the record's verbs: no path an
    // edit could take.
    for field in shape.fields().iter().filter(|field| !field.readonly) {
        let mut here = steps.to_vec();
        here.push(step_schema(
            "field",
            serde_json::json!({ "const": field.key }),
        ));
        let path = format!("{dotted}{}", field.key);
        let rich_schema = rich
            .iter()
            .find(|(at, _)| *at == path)
            .map(|(_, schema)| schema);
        let ops = field_ops(field, rich_schema.is_some());
        let collection = matches!(
            field.shape,
            crate::Shape::Collection(_) | crate::Shape::Keyed(_)
        );
        // A value field's `set` takes its value; containers' operations
        // carry items, elements or keys of their own.
        let simple = ops.first() == Some(&"set") && !collection;
        let value = if simple {
            (field.schema)()
        } else {
            serde_json::json!({})
        };
        let mut flags = Vec::new();
        if field.unit {
            flags.push("its own storage unit".to_owned());
        }
        match field.trust {
            crate::Trust::Ordinary => {}
            crate::Trust::Sensitive => flags.push("changing it asks for confirmation".to_owned()),
            crate::Trust::Weakens(_) => flags.push(
                "making it weaker can weaken security: such a change asks for confirmation and needs the person's own grant; making it stricter does not"
                    .to_owned(),
            ),
            crate::Trust::Weakening => flags.push(
                "changing it can weaken security: it asks for confirmation and needs the person's own grant"
                    .to_owned(),
            ),
        }
        if collection {
            // A field edit changes only the record, so Undo takes a removed
            // item back and nobody is asked first.
            flags.push(if field.quiet_remove {
                "removing an item asks nothing".to_owned()
            } else {
                "removing an item asks nothing; Undo takes it back".to_owned()
            });
        }
        let description = flags.join("; ");
        out.push(alternative(
            &here,
            false,
            &ops,
            value,
            rich_schema,
            field.title,
            description,
        ));
        match field.shape {
            crate::Shape::Record(_) => {
                edit_paths(field.shape, &here, &format!("{path}."), rich, out);
            }
            crate::Shape::Collection(inner) | crate::Shape::Keyed(inner) => {
                let mut items = here.clone();
                items.push(item.clone());
                let item_value = (field.schema)().get("items").map_or_else(
                    || serde_json::json!({}),
                    |items| {
                        // A collection's JSON lists `{id, value}` entries.
                        items
                            .get("properties")
                            .and_then(|properties| properties.get("value"))
                            .cloned()
                            .unwrap_or_else(|| items.clone())
                    },
                );
                out.push(alternative(
                    &items,
                    false,
                    &["set"],
                    item_value,
                    None,
                    field.title,
                    "one item, by its id".to_owned(),
                ));
                edit_paths(*inner, &items, &format!("{path}.*."), rich, out);
            }
            crate::Shape::Value if field.ty.starts_with("Grid<") => {
                let mut cells = here.clone();
                cells.push(step_schema(
                    "cell",
                    serde_json::json!({
                        "type": "object",
                        "properties": {
                            "row": { "type": "string" },
                            "column": { "type": "string" },
                        },
                        "required": ["row", "column"],
                    }),
                ));
                out.push(alternative(
                    &cells,
                    true,
                    &[
                        "set", "splice", "replace", "add", "insert", "remove", "rich",
                    ],
                    serde_json::json!({}),
                    None,
                    field.title,
                    "a cell, by its row and column ids, and what is inside it".to_owned(),
                ));
            }
            crate::Shape::Map(_) => {
                let mut entries = here.clone();
                entries.push(step_schema("key", serde_json::json!({ "type": "string" })));
                out.push(alternative(
                    &entries,
                    true,
                    &["set", "splice", "replace", "add", "insert", "remove"],
                    serde_json::json!({}),
                    None,
                    field.title,
                    "an entry, by its key, and what is inside it".to_owned(),
                ));
            }
            crate::Shape::Value => {}
        }
    }
}

mod field_edit;
use field_edit::rooted_edit_spec;

/// The published spec of [`FIELD_EDIT`], where `now` holds the values of
/// the fields that weaken one way ([`weakening_now`]).
fn field_edit_spec(vocabulary: &'static Vocabulary, now: Vec<(String, String)>) -> ActionSpec {
    // Built once per type: it describes every editable path, which only
    // the type decides.
    let schema = per_type(vocabulary, TypeSchema::Edit, &|| {
        Some(edit_schema(vocabulary))
    })
    .unwrap_or_else(|| Arc::new(edit_schema(vocabulary)));
    rooted_edit_spec(
        vocabulary,
        (Vec::new(), (vocabulary.shape)()),
        schema,
        format!("Edit a {} field", vocabulary.label),
        "The path of the field from the record's root, and the operation",
        now,
    )
}

/// The values now, as JSON text by dotted path, of `R`'s fields that weaken
/// one way ([`Trust::Weakens`](crate::Trust::Weakens)) outside any item:
/// whether a change of one weakens depends on them.
fn weakening_now<R: Resource>(state: &R::State) -> Vec<(String, String)> {
    weakening_in(R::shape(), &|| R::state_json(state))
}

/// [`weakening_now`] of the resource whose stored layout is `shape` and
/// whose state's JSON `json` reads. Not generic, so it is compiled once
/// rather than for every resource type.
#[inline(never)]
fn weakening_in(
    shape: Option<crate::Shape>,
    json: &dyn Fn() -> Option<serde_json::Value>,
) -> Vec<(String, String)> {
    let Some(shape) = shape else {
        return Vec::new();
    };
    let fields: Vec<String> = shape
        .guarded_fields()
        .into_iter()
        .filter(|(path, trust)| {
            matches!(trust, crate::Trust::Weakens(_)) && !path.split('.').any(|key| key == "*")
        })
        .map(|(path, _)| path)
        .collect();
    if fields.is_empty() {
        return Vec::new();
    }
    let Some(json) = json() else {
        return Vec::new();
    };
    fields
        .into_iter()
        .filter_map(|path| {
            let value = path
                .split('.')
                .try_fold(&json, |value, key| value.get(key))?
                .to_string();
            Some((path, value))
        })
        .collect()
}

/// The field edit offered on the item `item` of the collection `field`:
/// paths from the item, its editable paths described as the record's are.
#[must_use]
pub fn item_field_edit_spec<R: Resource>(field: &str, item: &crate::Id) -> Option<ActionSpec> {
    item_field_edit_spec_of(vocabulary::<R>(), field, item)
}

/// [`item_field_edit_spec`] of the resource `vocabulary`.
pub(crate) fn item_field_edit_spec_of(
    vocabulary: &'static Vocabulary,
    field: &str,
    item: &crate::Id,
) -> Option<ActionSpec> {
    field_edit_spec_at_of(
        vocabulary,
        vec![
            crate::Segment::Field(field.to_owned()),
            crate::Segment::Item(item.clone()),
        ],
    )
}

/// The field edit offered on the item at `prefix` (fields and item ids from
/// the record, ending at an item): paths from the item, its editable paths
/// described as the record's are.
#[must_use]
pub fn field_edit_spec_at<R: Resource>(prefix: Vec<crate::Segment>) -> Option<ActionSpec> {
    field_edit_spec_at_of(vocabulary::<R>(), prefix)
}

/// [`field_edit_spec_at`] of the resource `vocabulary`.
pub(crate) fn field_edit_spec_at_of(
    vocabulary: &'static Vocabulary,
    prefix: Vec<crate::Segment>,
) -> Option<ActionSpec> {
    let mut shape = (vocabulary.shape)()?;
    let mut dotted = String::new();
    let mut readonly = false;
    for segment in &prefix {
        match (shape, segment) {
            (crate::Shape::Record(fields), crate::Segment::Field(key)) => {
                let field = fields.iter().find(|candidate| candidate.key == key)?;
                readonly |= field.readonly;
                shape = field.shape;
                dotted.push_str(key);
                dotted.push('.');
            }
            (
                crate::Shape::Collection(inner) | crate::Shape::Keyed(inner),
                crate::Segment::Item(_),
            ) => {
                shape = *inner;
                dotted.push_str("*.");
            }
            _ => return None,
        }
    }
    if shape.fields().is_empty() || readonly {
        return None;
    }
    // The same for every item: built once per type and path.
    let schema = per_type_at(vocabulary, TypeSchema::Edit, &dotted, &|| {
        let ops: Vec<(String, serde_json::Value)> = (vocabulary.op_schemas)()
            .into_iter()
            .filter_map(|(path, schema)| {
                path.strip_prefix(&dotted)
                    .map(|rest| (rest.to_owned(), schema))
            })
            .collect();
        let mut alternatives = Vec::new();
        edit_paths(shape, &[], "", &ops, &mut alternatives);
        (!alternatives.is_empty()).then(|| described_edit(&alternatives, &ops))
    })?;
    Some(rooted_edit_spec(
        vocabulary,
        (prefix, Some(shape)),
        schema,
        "Edit a field of this item".to_owned(),
        "The path of the field from the item, and the operation",
        Vec::new(),
    ))
}

/// The edits of a resource whose document holds nothing. An app whose data
/// lives entirely outside any document — credentials in this device's
/// encrypted vault, a device's hardware — publishes that data as live rows
/// and effects, and its resource is only the door they are published on.
/// No value exists, so nothing is ever applied, undone or synced.
#[derive(Clone, Copy, Debug, serde::Serialize, serde::Deserialize)]
pub enum NoAction {}

impl tilo_actions::Action for NoAction {
    // No value exists to apply.
    fn apply(&self, _doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
        Ok(())
    }
}

/// A search index entry. `anchor` addresses what a hit opens (an item id);
/// `action`, when set, is the published action a hit invokes, so choosing it
/// goes through the same door, permissions and history as the palette.
///
/// How a hit reads is the entry's too: the result group it is listed under
/// (the resource's label unless `group` names another, such as "Timeline
/// markers"), a dimmer `subtitle` line, a named `icon` (a lucide kebab name,
/// `"star"`), and a `boost` that floats pinned or recent items over an equal
/// match. Long prose an entry is found by — a book's chapter, a long note —
/// is its `body`, which the index reads in place rather than the entry
/// copying it, and whose segments may each be a hit of their own.
// `Clone` and `PartialEq` are written out: derived, they are inlined into
// every app that copies or compares an entry.
#[derive(Debug, Default, Eq)]
pub struct SearchEntry {
    /// The containing resource scope for a flat anchor.
    pub within: Option<String>,
    /// Presentation for the platform-rendered result row.
    pub row: Option<SearchRow>,
    pub anchor: Option<String>,
    pub title: String,
    pub text: String,
    pub action: Option<tilo_actions::ActionAddress>,
    pub group: Option<String>,
    pub subtitle: Option<String>,
    pub icon: Option<String>,
    /// What the palette shows of the hit while a person is on it; `None`
    /// shows its `text`, and an entry with neither shows no preview.
    pub preview: Option<SearchPreview>,
    pub boost: i32,
    pub body: Option<SearchBody>,
    /// More short text the entry is found by, each ranked by its own
    /// weight: a sender's address, a book's subject, a message's labels.
    pub fields: Vec<(String, SearchWeight)>,
    /// Structured filter values: token key, localized group label, value.
    pub facets: Vec<(String, String, String)>,
    /// The entry describes a secret (a vault's login): the system index
    /// keeps its metadata but finds it only while the app reports its
    /// secrets unlocked ([`SearchEntry::secret_bearing`]).
    pub secret: bool,
}

impl Clone for SearchEntry {
    fn clone(&self) -> Self {
        Self {
            within: self.within.clone(),
            row: self.row.clone(),
            anchor: self.anchor.clone(),
            title: self.title.clone(),
            text: self.text.clone(),
            action: self.action.clone(),
            group: self.group.clone(),
            subtitle: self.subtitle.clone(),
            icon: self.icon.clone(),
            preview: self.preview.clone(),
            boost: self.boost,
            body: self.body.clone(),
            fields: self.fields.clone(),
            facets: self.facets.clone(),
            secret: self.secret,
        }
    }
}

impl PartialEq for SearchEntry {
    fn eq(&self, other: &Self) -> bool {
        self.within == other.within
            && self.row == other.row
            && self.anchor == other.anchor
            && self.title == other.title
            && self.text == other.text
            && self.action == other.action
            && self.group == other.group
            && self.subtitle == other.subtitle
            && self.icon == other.icon
            && self.preview == other.preview
            && self.boost == other.boost
            && self.body == other.body
            && self.fields == other.fields
            && self.facets == other.facets
            && self.secret == other.secret
    }
}

/// How much a match in one of a [`SearchEntry`]'s fields counts. The title
/// ranks as [`Self::Title`], the subtitle as [`Self::Subtitle`] and the text
/// as [`Self::Body`]; [`SearchEntry::field`] adds others.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SearchWeight {
    /// The name a person looks for.
    Title,
    /// A stable identity typed verbatim: an address, an id, a path.
    Identity,
    /// A secondary line: an author, a sender, a folder.
    Subtitle,
    /// Other short fields: labels, tags, a subject.
    Metadata,
    /// Long prose.
    Body,
    /// Found, but never reorders hits.
    Unranked,
}

/// One run of a [`SearchText`] segment: styled text, say.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchPiece<'a> {
    pub text: &'a str,
    /// Whether a word break separates this piece from the text before it.
    pub space_before: bool,
}

/// Long text a search entry is found by, owned by the app: the index reads
/// it while indexing and never copies it. It is a sequence of segments (the
/// unit a hit can point at: a paragraph, a block), each a sequence of
/// pieces. It must not change once handed to an entry; changed prose is a
/// new body.
pub trait SearchText: Send + Sync {
    fn segment_count(&self) -> usize;

    fn piece_count(&self, segment: usize) -> usize;

    fn piece(&self, segment: usize, piece: usize) -> Option<SearchPiece<'_>>;

    /// Whether a match in `segment` is a hit of its own ([`Self::location`])
    /// rather than the entry's.
    fn locates(&self, segment: usize) -> bool {
        let _ = segment;
        false
    }

    /// The hit a match in `segment` is — a reading location, invoking the
    /// action that opens the text there — when [`Self::locates`] says so.
    fn location(&self, segment: usize) -> Option<SearchEntry> {
        let _ = segment;
        None
    }
}

/// A [`SearchText`] an entry carries. Cloning one is cheap, and two bodies
/// are the same body only when they share one text, so an app that rebuilds
/// its entries over mostly unchanged prose keeps its bodies and hands the
/// same ones back.
#[derive(Clone)]
pub struct SearchBody(Arc<dyn SearchText>);

impl SearchBody {
    #[must_use]
    pub fn new(text: Arc<dyn SearchText>) -> Self {
        Self(text)
    }

    #[must_use]
    pub fn text(&self) -> &dyn SearchText {
        self.0.as_ref()
    }

    /// The shared text, for an index that reads it later.
    #[must_use]
    pub fn shared(&self) -> Arc<dyn SearchText> {
        Arc::clone(&self.0)
    }
}

impl PartialEq for SearchBody {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for SearchBody {}

impl std::fmt::Debug for SearchBody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SearchBody")
            .field("segments", &self.0.segment_count())
            .finish()
    }
}

impl SearchEntry {
    /// Offer this entry under one value of a platform search facet.
    #[must_use]
    pub fn facet(
        mut self,
        key: impl Into<String>,
        label: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.facets.push((key.into(), label.into(), value.into()));
        self
    }

    /// Whether the entry lies in `scope`: its anchor, or the scope it is
    /// [`within`](Self::within), is the scope or lies beneath it.
    #[must_use]
    pub fn in_scope(&self, scope: &str) -> bool {
        [self.within.as_deref(), self.anchor.as_deref()]
            .into_iter()
            .flatten()
            .any(|anchor| scope_contains(scope, anchor))
    }

    /// An entry titled `title` whose body text is `text`.
    pub fn new(title: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            text: text.into(),
            ..Self::default()
        }
    }

    /// List the hit inside the scope `scope` (an item's anchor) though its
    /// own anchor lies elsewhere: an article in its feed's scope, related by
    /// a field rather than by nesting.
    #[must_use]
    pub fn inside(mut self, scope: impl Into<String>) -> Self {
        self.within = Some(scope.into());
        self
    }

    /// The item a hit opens.
    #[must_use]
    pub fn anchored(mut self, anchor: impl Into<String>) -> Self {
        self.anchor = Some(anchor.into());
        self
    }

    /// The published action a hit invokes.
    #[must_use]
    pub fn invoking(mut self, action: tilo_actions::ActionAddress) -> Self {
        self.action = Some(action);
        self
    }

    /// Anchored at the live row `row`, opened by running `effect`, the
    /// view effect that row offers (go to a bookmark, show a find match):
    /// for a hit whose row is the window's own state rather than an item
    /// of the record.
    #[must_use]
    pub fn opening<E: crate::Effect>(mut self, row: impl Into<String>, effect: &E) -> Self {
        let row = row.into();
        self.action = Some(tilo_actions::ActionAddress::new(row.clone(), effect.id()));
        self.anchor = Some(row);
        self
    }

    /// List hits under `group` rather than the resource's label.
    #[must_use]
    pub fn grouped(mut self, group: impl Into<String>) -> Self {
        self.group = Some(group.into());
        self
    }

    /// A secondary line under the title: where the item is, what it is.
    #[must_use]
    pub fn subtitled(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// The glyph a hit leads with, by its lucide kebab name (`"star"`).
    #[must_use]
    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    /// What the palette shows of the hit while a person is on it, in place
    /// of its text.
    #[must_use]
    pub fn previewed(mut self, preview: SearchPreview) -> Self {
        self.preview = Some(preview);
        self
    }

    /// Rank the hit `boost` above an equal match: a nudge, never an
    /// override of how well it matches.
    #[must_use]
    pub fn boosted(mut self, boost: i32) -> Self {
        self.boost = boost;
        self
    }

    /// Find the entry by `text` too, a match in it ranked as `weight`.
    #[must_use]
    pub fn field(mut self, text: impl Into<String>, weight: SearchWeight) -> Self {
        let text = text.into();
        if !text.trim().is_empty() {
            self.fields.push((text, weight));
        }
        self
    }

    /// Find the entry by `body` too, read in place.
    #[must_use]
    pub fn with_body(mut self, body: SearchBody) -> Self {
        self.body = Some(body);
        self
    }

    /// Mark the entry as describing a secret. The app publishes metadata
    /// only (a name, a username, a site, never the secret), and the
    /// system's search finds the entry only while the app reports its
    /// secrets unlocked; while they are locked a match offers to unlock
    /// the app instead.
    #[must_use]
    pub fn secret_bearing(mut self) -> Self {
        self.secret = true;
        self
    }
}

/// Where a participant is inside a resource, in the resource's own terms:
/// the same on every collaborator's screen whatever its layout.
///
/// Also what an action is told to act at (`#[param(place)] at:
/// Option<Place>`): it travels as `{"item": id}`, `{"item": id, "at": n}` or
/// `{"item": id, "start": n, "end": n}`.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(into = "PlaceWire")]
pub enum Place {
    /// An item (a paragraph, a clip, a row).
    Item(String),
    /// A caret at a character position inside an item.
    Caret { item: String, at: usize },
    /// Characters `start..end` of an item.
    Range {
        item: String,
        start: usize,
        end: usize,
    },
}

impl Place {
    /// The item the place is in.
    #[must_use]
    pub fn item(&self) -> &str {
        match self {
            Self::Item(item) | Self::Caret { item, .. } | Self::Range { item, .. } => item,
        }
    }
}

// Decoded through a JSON value whatever the deserializer, by one function.
impl<'de> serde::Deserialize<'de> for Place {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
        match place_from_json(value) {
            Ok(place) => Ok(place),
            Err(error) => Err(serde::de::Error::custom(error)),
        }
    }
}

/// The place in `value`, its JSON form.
fn place_from_json(value: serde_json::Value) -> Result<Place, String> {
    match serde_json::from_value::<PlaceWire>(value) {
        Ok(wire) => Place::try_from(wire),
        Err(error) => Err(error.to_string()),
    }
}

impl crate::JsonSchema for Place {
    fn json_schema() -> serde_json::Value {
        tilo_actions::place_schema()
    }
}

/// [`Place`] as it travels.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PlaceWire {
    item: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    at: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    start: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    end: Option<usize>,
}

impl From<Place> for PlaceWire {
    fn from(place: Place) -> Self {
        match place {
            Place::Item(item) => Self {
                item,
                at: None,
                start: None,
                end: None,
            },
            Place::Caret { item, at } => Self {
                item,
                at: Some(at),
                start: None,
                end: None,
            },
            Place::Range { item, start, end } => Self {
                item,
                at: None,
                start: Some(start),
                end: Some(end),
            },
        }
    }
}

impl TryFrom<PlaceWire> for Place {
    type Error = String;

    fn try_from(wire: PlaceWire) -> Result<Self, String> {
        if wire.item.is_empty() {
            return Err("a place names its item".into());
        }
        match (wire.at, wire.start, wire.end) {
            (None, None, None) => Ok(Self::Item(wire.item)),
            (Some(at), None, None) => Ok(Self::Caret {
                item: wire.item,
                at,
            }),
            (None, Some(start), Some(end)) if start <= end => Ok(Self::Range {
                item: wire.item,
                start,
                end,
            }),
            _ => {
                Err("a place is an item, a caret `at` in it, or a range `start..end` of it".into())
            }
        }
    }
}

// A selection carried to an agent is the place an action takes, in the
// same wire form, so the agent can hand it back unchanged.
impl From<Place> for tilo_actions::SelectionPlace {
    fn from(place: Place) -> Self {
        match place {
            Place::Item(item) => Self::item(item),
            Place::Caret { item, at } => Self::caret(item, at),
            Place::Range { item, start, end } => Self::range(item, start, end),
        }
    }
}

impl TryFrom<tilo_actions::SelectionPlace> for Place {
    type Error = String;

    fn try_from(place: tilo_actions::SelectionPlace) -> Result<Self, String> {
        Self::try_from(PlaceWire {
            item: place.item,
            at: place.at,
            start: place.start,
            end: place.end,
        })
    }
}

/// Builds a resource's action from arguments already bound against the
/// declaration's parameters. `now_ms` is the origin's clock, captured once so
/// replay is deterministic.
pub type BuildAction<R> =
    Arc<dyn Fn(&ActionArgs, i64) -> Result<<R as Resource>::Action, String> + Send + Sync>;

/// One mutation offered by name: a [`Declaration`] of the resource `R`,
/// whose fields it derefs to.
pub struct ActionDecl<R: Resource> {
    decl: Declaration,
    marker: std::marker::PhantomData<fn() -> R>,
}

impl<R: Resource> Clone for ActionDecl<R> {
    fn clone(&self) -> Self {
        Self {
            decl: self.decl.clone(),
            marker: std::marker::PhantomData,
        }
    }
}

impl<R: Resource> std::ops::Deref for ActionDecl<R> {
    type Target = Declaration;

    fn deref(&self) -> &Declaration {
        &self.decl
    }
}

impl<R: Resource> AsRef<Declaration> for ActionDecl<R> {
    fn as_ref(&self) -> &Declaration {
        &self.decl
    }
}

impl<R: Resource> std::ops::DerefMut for ActionDecl<R> {
    fn deref_mut(&mut self) -> &mut Declaration {
        &mut self.decl
    }
}

impl<R: Resource> ActionDecl<R> {
    /// A field edit whose one value its caller supplies, offered as the
    /// action `id` labelled `label`: the parameter `param` (described by
    /// `description`, typed by `V`'s schema) becomes the edit `make(value)`.
    /// What a row offers when the edit is bound to it but its value is not
    /// ("Rename playlist", "Add a song to this playlist").
    pub fn edit_asking<V>(
        id: &'static str,
        label: impl Into<String>,
        param: &'static str,
        description: &'static str,
        make: impl Fn(V) -> crate::Edit<R> + Send + Sync + 'static,
    ) -> Self
    where
        V: DeserializeOwned + crate::JsonSchema,
    {
        let schema = V::json_schema();
        let declared = ActionParam::required_owned(
            param,
            crate::__private::param_type_of(&schema),
            description,
        )
        .with_schema(schema);
        let vocabulary = vocabulary::<R>();
        let build: Builds = Arc::new(move |args, now_ms| {
            let value = taken_arg(args, param)?;
            let value: V = match serde_json::from_value(value) {
                Ok(value) => value,
                Err(error) => return Err(format!("`{param}`: {error}")),
            };
            stamped_edit(vocabulary, make(value).retyped(), now_ms)
        });
        Self::declared(Declaration::new(
            vocabulary,
            id,
            label.into(),
            vec![declared],
            build,
        ))
    }

    /// The field edit `edit` offered as the action `id` labelled `label`,
    /// its value bound: what a place or a live row offers ("Use Proton for
    /// Adobe Reader"), checked and recorded as the edit itself.
    pub fn edit(id: &'static str, label: impl Into<String>, edit: crate::Edit<R>) -> Self
    where
        crate::Edit<R>: Send + Sync,
    {
        Self::declared(edit_declaration(
            vocabulary::<R>(),
            id,
            label.into(),
            edit.retyped(),
        ))
    }

    pub fn new(
        id: &'static str,
        label: impl Into<String>,
        params: Vec<ActionParam>,
        build: impl Fn(&ActionArgs, i64) -> Result<R::Action, String> + Send + Sync + 'static,
    ) -> Self {
        let build: Builds = Arc::new(move |args, now_ms| match build(args, now_ms) {
            Ok(action) => Ok(Built::of::<R>(action)),
            Err(reason) => Err(reason),
        });
        Self::declared(Declaration::new(
            vocabulary::<R>(),
            id,
            label.into(),
            params,
            build,
        ))
    }

    pub(crate) fn declared(decl: Declaration) -> Self {
        Self {
            decl,
            marker: std::marker::PhantomData,
        }
    }

    /// The declaration, of whichever resource.
    #[must_use]
    pub fn into_declaration(self) -> Declaration {
        self.decl
    }

    /// The action it builds from `args` at `now_ms`.
    ///
    /// # Errors
    /// Why the arguments do not make the action.
    pub fn build_action(&self, args: &ActionArgs, now_ms: i64) -> Result<R::Action, String> {
        typed::<R>((self.decl.build)(args, now_ms)?)
    }

    /// Mark a bound edit as refining the resource's declared action of the
    /// same id ([`bind_declaration`]).
    #[must_use]
    pub fn refining(mut self) -> Self {
        self.decl.refines = true;
        self
    }

    /// Tag the action's contract (`ContextRequirement::Caret.tag()`, an
    /// incremental-text declaration) for the callers that read it.
    #[must_use]
    pub fn tagged(mut self, tag: impl Into<String>) -> Self {
        self.decl.tag(tag.into());
        self
    }

    #[must_use]
    pub fn describe(mut self, description: impl Into<String>) -> Self {
        self.decl.description = Some(description.into());
        self
    }

    /// Declare what the action answers its caller with (an outline of the
    /// state it left), which the hosting surface supplies after it applies.
    #[must_use]
    pub fn returns(mut self, result: tilo_actions::ResultSpec) -> Self {
        self.decl.result = Some(result);
        self
    }

    /// Set what the action is able to do.
    #[must_use]
    pub fn of_kind(mut self, kind: ActionKind) -> Self {
        self.decl.kind = kind;
        self
    }

    /// Place the action below the one field it writes.
    #[must_use]
    pub fn below_field(mut self, field: &'static str) -> Self {
        self.decl.field = Some(field);
        self
    }

    #[must_use]
    pub fn destructive(mut self) -> Self {
        self.decl.destructive = true;
        self
    }

    /// The units the action writes, as the surface lists them
    /// ([`crate::footprint`]).
    #[must_use]
    pub fn writing(mut self, units: Vec<String>) -> Self {
        self.decl.writes = units;
        self
    }

    /// Answer the caller with what the action made, from
    /// [`Resource::answer`].
    #[must_use]
    pub fn answering(mut self) -> Self {
        self.decl.answer();
        self
    }
}

impl Declaration {
    fn tag(&mut self, tag: String) {
        if !self.tags.contains(&tag) {
            self.tags.push(tag);
        }
    }

    fn answer(&mut self) {
        self.result = Some(tilo_actions::ResultSpec::Json {
            schema: serde_json::json!({"type": "object"}),
        });
    }
}

/// The action of `R` that `built` is.
fn typed<R: Resource>(built: Built) -> Result<R::Action, String> {
    built
        .into_action::<R>()
        .ok_or_else(|| format!("not an action of a {}", R::LABEL))
}

/// The argument `param` of `args`, taken out.
fn taken_arg(args: &ActionArgs, param: &str) -> Result<serde_json::Value, String> {
    match args.to_json().remove(param) {
        Some(value) => Ok(value),
        None => Err(format!("`{param}` is missing")),
    }
}

/// The action the field edit `edit` of the resource `vocabulary` is,
/// stamped at `now_ms`.
pub(crate) fn stamped_edit(
    vocabulary: &Vocabulary,
    edit: Edit<()>,
    now_ms: i64,
) -> Result<Built, String> {
    let Some(mut action) = (vocabulary.edit)(edit) else {
        return Err(format!("{} has no field edits", vocabulary.label));
    };
    action.stamp(now_ms);
    Ok(action)
}

/// [`ActionDecl::edit`] of the resource `vocabulary`.
fn edit_declaration(
    vocabulary: &'static Vocabulary,
    id: &'static str,
    label: String,
    edit: Edit<()>,
) -> Declaration {
    let destructive = (vocabulary.edit)(edit.clone())
        .is_some_and(|action| action.risk() == tilo_actions::RiskLevel::Destructive);
    let invokes = serde_json::to_value(&edit).ok().map(|edit| {
        (
            FIELD_EDIT.to_owned(),
            serde_json::Map::from_iter([(EDIT_PARAM.to_owned(), edit)]),
        )
    });
    let build: Builds = Arc::new(move |_, now_ms| stamped_edit(vocabulary, edit.clone(), now_ms));
    let mut decl = Declaration::new(vocabulary, id, label, Vec::new(), build);
    decl.destructive = destructive;
    decl.invokes = invokes;
    decl
}

thread_local! {
    /// The origin clock of a [`Replica`](crate::Replica) applying an action
    /// on this thread, in place of the machine's.
    static ORIGIN_CLOCK: std::cell::Cell<Option<i64>> = const { std::cell::Cell::new(None) };
}

/// Milliseconds since the Unix epoch on this machine (or the clock of the
/// replica applying an action here). Only ever read on the origin, when an
/// action is built; never inside `apply`.
#[must_use]
pub fn origin_now_ms() -> i64 {
    ORIGIN_CLOCK
        .with(std::cell::Cell::get)
        .unwrap_or_else(tilo_clock::unix_now_ms_i64)
}

/// Run `f` with the origin clock reading `now_ms`: every action built
/// inside it is stamped with that time, so encoding the same invocation
/// twice (once to ask for confirmation, once to check it) gives the same
/// action.
pub fn with_origin_clock<T>(now_ms: i64, f: impl FnOnce() -> T) -> T {
    struct Restore(Option<i64>);
    impl Drop for Restore {
        fn drop(&mut self) {
            ORIGIN_CLOCK.with(|clock| clock.set(self.0));
        }
    }
    let _restore = Restore(ORIGIN_CLOCK.with(|clock| clock.replace(Some(now_ms))));
    f()
}

/// Encode a resource action under its registry type id.
///
/// # Errors
/// Returns [`ActionError::Encode`] when the action does not serialize.
pub fn encode<R: Resource>(action: &R::Action) -> Result<EncodedAction, ActionError> {
    EncodedAction::encode(R::ACTION_TYPE, action)
}

/// The encoded form of a built action, or a refusal carrying why it does
/// not encode.
pub(crate) fn encoded(action: &Built) -> EncodedAction {
    match action.encode() {
        Ok(encoded) => encoded,
        Err(error) => refused(&error.to_string()),
    }
}

/// The encoded form of a builder's result: the action, or a refusal that
/// carries the builder's reason.
pub(crate) fn encode_built(built: Result<Built, String>) -> EncodedAction {
    match built {
        Ok(action) => encoded(&action),
        Err(reason) => refused(&reason),
    }
}

/// The nested resource `id` of `state`, read from `doc` with `id` demanded
/// when `state` has not loaded it: an item past what a view shows or a
/// tree pages still resolves by id ([`Resource::child`], [`Resource::load`]).
pub fn child_by_id<R: Resource>(
    state: &R::State,
    doc: &Doc,
    id: &str,
) -> Option<tilo_actions::Resource> {
    if let Some(child) = R::child(state, id) {
        return Some(child);
    }
    let mut fresh = R::read(doc).ok()?;
    R::load(&mut fresh, doc, &child_demand(id)).ok()?;
    R::child(&fresh, id)
}

/// What reading the child published at `id` demands: compiled once rather
/// than for every resource.
#[inline(never)]
fn child_demand(id: &str) -> Demand {
    // A child is published as `<kind>/<field>/<key>`; its record knows
    // it by the key.
    let key = id.rsplit('/').next().unwrap_or(id);
    Demand::none().with_keys([id, key])
}

/// An action whose payload cannot be decoded, so the host refuses it with
/// the builder's reason instead of applying anything.
pub(crate) fn refused(reason: &str) -> EncodedAction {
    EncodedAction {
        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.to_owned(),
        payload: reason.as_bytes().to_vec(),
    }
}

/// The ids of `declarations` some child among `children` offers bound.
fn offered_bound_ids(
    children: &[tilo_actions::Resource],
    declarations: &[Declaration],
) -> Vec<&'static str> {
    let mut ids = Vec::new();
    for decl in declarations {
        if crate::items::offered_bound(children, decl) {
            ids.push(decl.id);
        }
    }
    ids
}

/// The least a caller needs to take any of `declarations`.
fn least_need_declared(declarations: &[Declaration]) -> ActionNeed {
    let mut needs = Vec::with_capacity(declarations.len());
    for decl in declarations {
        needs.push((decl.kind, decl.field));
    }
    least_need_of(&needs)
}

/// The least a caller needs to take any of the actions `needs` (kind and
/// field each): the one that asks least.
#[must_use]
pub fn least_need_of(needs: &[(ActionKind, Option<&'static str>)]) -> ActionNeed {
    needs
        .iter()
        .min_by_key(|(kind, field)| (*kind, field.is_none()))
        .map_or_else(
            || tilo_actions::ActionContract::default().need(),
            |(kind, field)| ActionNeed {
                kind: *kind,
                field: field.map(str::to_owned),
                privileged: false,
            },
        )
}

/// The published spec of one declared or bound mutation: what the palette,
/// agents and scripts invoke, building the resource's action from their
/// arguments.
///
/// The action is built when it is invoked, never when it is published: one
/// that takes no arguments still mints its ids and reads the clock at the
/// moment it is taken, so two invocations make two distinct actions.
#[must_use]
pub fn declaration_spec<D: AsRef<Declaration> + ?Sized>(decl: &D) -> ActionSpec {
    spec_of_declaration(decl.as_ref())
}

/// [`declaration_spec`] of a declaration of whichever resource.
fn spec_of_declaration(decl: &Declaration) -> ActionSpec {
    let build = Arc::clone(&decl.build);
    let encoded = move |args: &tilo_actions::ActionArgs| encode_built(build(args, origin_now_ms()));
    let spec = if let Some(prompt) = &decl.content {
        let param = ActionParam::required_owned(
            tilo_actions::PROMPTED_PARAM,
            ParamType::Text,
            prompt.clone(),
        );
        ActionSpec::prompted_content(
            decl.id,
            decl.label.clone(),
            prompt.clone(),
            "content hash",
            ExecutionLocus::ClientLocal,
            move |hash| {
                let supplied = [(
                    tilo_actions::PROMPTED_PARAM.to_owned(),
                    tilo_actions::ArgValue::Text(hash.to_owned()),
                )];
                match ActionArgs::bind(std::slice::from_ref(&param), supplied) {
                    Ok(args) => encoded(&args),
                    Err(error) => refused(&error.to_string()),
                }
            },
        )
    } else if decl.params.is_empty() {
        // Built when taken, so it reads the clock and mints its ids then; a
        // destructive one is held for confirmation as the same verb of the
        // same kind of record.
        let domain = format!("{}#{}", decl.resource, decl.id);
        let action_type = decl.action_type;
        ActionSpec::deferred(
            decl.id,
            decl.label.clone(),
            ExecutionLocus::ClientLocal,
            action_type,
            move || encoded(&tilo_actions::ActionArgs::none()),
        )
        .with_continuation_identity(move |query| match query {
            tilo_actions::ContinuationQuery::Draft => {
                Some(tilo_actions::ContinuationEvidence::Draft {
                    resolution_domain: tilo_actions::ContinuationKey::new(
                        "verb",
                        [domain.as_bytes()],
                    ),
                    handler: tilo_actions::ContinuationKey::new(
                        "action-type",
                        [action_type.as_bytes()],
                    ),
                })
            }
            tilo_actions::ContinuationQuery::Exact(_) => None,
        })
    } else {
        ActionSpec::typed(
            decl.id,
            decl.label.clone(),
            decl.label.clone(),
            ExecutionLocus::ClientLocal,
            decl.params.clone(),
            encoded,
        )
    };
    let mut spec = spec.of_kind(decl.kind);
    // A document action is undone by the host's undo; one that reached
    // outside the document is not.
    if decl.kind == ActionKind::Undoable {
        spec = spec.reversible();
    }
    // One whose arguments decide whether it reaches outside is what the
    // action built from them is; arguments that build none are taken as
    // reaching outside.
    if decl.by_arguments {
        let classify = Arc::clone(&decl.build);
        spec = spec.with_need_of(move |args| match classify(args, 0) {
            Ok(action) => action.need(),
            Err(_) => tilo_actions::ActionNeed::of(ActionKind::Outside),
        });
    }
    if let Some(field) = decl.field {
        spec = spec.below_field(field);
    }
    if let Some(description) = &decl.description {
        spec = spec.with_description(description.clone());
    }
    if decl.destructive {
        spec = spec.destructive();
    }
    if let Some(result) = &decl.result {
        spec.contract.result = result.clone();
    }
    for tag in &decl.tags {
        if !spec.contract.tags.contains(tag) {
            spec.contract.tags.push(tag.clone());
        }
    }
    // A compensated undo arrives as a tag: once it is there, a deletion it
    // takes back is no longer asked about.
    spec.contract.settle_confirmation();
    spec
}

/// Each declared action's arguments object, as serde reads it.
fn declared_args(
    declarations: &[Declaration],
) -> std::collections::BTreeMap<&'static str, serde_json::Value> {
    let mut args = std::collections::BTreeMap::new();
    for decl in declarations {
        args.insert(decl.id, crate::schema::args_schema(&decl.params));
    }
    args
}

/// The parameter of [`APPLY`]: one action value, described by
/// [`Resource::action_schema`] when the resource has one, with what only
/// restates a named action folded into a reference to it.
pub(crate) fn apply_param(vocabulary: &Vocabulary) -> ActionParam {
    let param = ActionParam::required_owned(
        APPLY_PARAM,
        ParamType::Object,
        format!("A {} action, as JSON", vocabulary.label),
    );
    let schema = per_type(vocabulary, TypeSchema::Apply, &|| {
        let schema = (vocabulary.action_schema)()?;
        Some(crate::schema::fold_declared(
            &schema,
            &declared_args(&vocabulary.declarations()),
        ))
    });
    match schema {
        Some(schema) => param.with_shared_schema(schema),
        None => param,
    }
}

/// The schemas a publication derives from the resource type alone.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum TypeSchema {
    Apply,
    Group,
    /// The field edit's, at the paths under a dotted prefix (`""` for the
    /// record's root, `notes.*.` for an item of `notes`).
    Edit,
}

#[derive(Default)]
struct CompiledSchemas {
    epoch: u64,
    entries: HashMap<(TypeId, TypeSchema, String), Option<Arc<serde_json::Value>>>,
}

static SCHEMAS: OnceLock<Mutex<CompiledSchemas>> = OnceLock::new();

/// Forget schemas derived by the previous app code before an in-place reload.
/// Hosts also call this when a reload is rejected, so the retained app rebuilds
/// its own definitions. Document and view changes do not invalidate type schemas.
#[doc(hidden)]
pub fn reset_type_schemas() {
    if let Some(cache) = SCHEMAS.get() {
        let mut store = cache.lock_recover();
        store.epoch = store.epoch.wrapping_add(1);
        store.entries.clear();
    }
}

/// Which generation of app code built what is cached from the resource
/// types: it changes when [`reset_type_schemas`] forgets them.
pub(crate) fn type_epoch() -> u64 {
    SCHEMAS.get().map_or(0, |cache| cache.lock_recover().epoch)
}

/// `build`'s schema for the resource `vocabulary`, retained for this
/// generation of app code.
fn per_type(
    vocabulary: &Vocabulary,
    schema: TypeSchema,
    build: &dyn Fn() -> Option<serde_json::Value>,
) -> Option<Arc<serde_json::Value>> {
    per_type_at(vocabulary, schema, "", build)
}

/// [`per_type`] for the paths under the dotted prefix `at`.
fn per_type_at(
    vocabulary: &Vocabulary,
    schema: TypeSchema,
    at: &str,
    build: &dyn Fn() -> Option<serde_json::Value>,
) -> Option<Arc<serde_json::Value>> {
    let cache = SCHEMAS.get_or_init(Mutex::default);
    cached_schema(
        cache,
        ((vocabulary.type_id)(), schema, at.to_owned()),
        build,
    )
}

fn cached_schema(
    cache: &Mutex<CompiledSchemas>,
    key: (TypeId, TypeSchema, String),
    build: &dyn Fn() -> Option<serde_json::Value>,
) -> Option<Arc<serde_json::Value>> {
    let epoch = {
        let store = cache.lock_recover();
        if let Some(found) = store.entries.get(&key) {
            let _phase = crate::profiling::Phase::start("schema.cached_clone");
            return found.clone();
        }
        store.epoch
    };
    // Build outside the lock: a schema may publish another resource's.
    let value = build().map(Arc::new);
    let mut store = cache.lock_recover();
    // A build started before a code reload cannot repopulate the new cache.
    if store.epoch == epoch {
        store.entries.insert(key, value.clone());
    }
    value
}

/// A JSON action value as [`APPLY`] reads it. A value naming a declared
/// action is checked against that declaration first, so it is refused with
/// the reason the named action would give.
fn read_apply(vocabulary: &Vocabulary, value: serde_json::Value) -> Result<Built, String> {
    let declarations = vocabulary.declarations();
    let mut ids: Vec<&str> = Vec::with_capacity(declarations.len());
    for decl in declarations.iter() {
        ids.push(decl.id);
    }
    if let Some((id, fields)) = crate::schema::declared_invocation(&value, &ids)
        && let Some(decl) = declarations.iter().find(|decl| decl.id == id)
        && let Err(error) = ActionArgs::bind_json(&decl.params, &fields)
    {
        return Err(format!("{id}: {error}"));
    }
    // Group members refer to this schema instead of copying it into the
    // group's parameter. Validate here as well as at the direct apply door,
    // including constraints stricter than the action's serde representation.
    if let Some(schema) = apply_param(vocabulary).schema {
        tilo_actions::validate_json_schema(&value, &schema)?;
    }
    (vocabulary.read)(value)
}

/// The parameter of [`GROUP`](crate::GROUP): invocations of the resource's
/// declared actions and of [`APPLY`]. A declared member's arguments are
/// checked against its own action's schema when the group is built, so this
/// names those actions rather than repeating their schemas. An `apply`
/// member is read exactly as [`APPLY`] reads its value when the group is
/// built, so it refers to `apply`'s schema rather than copying it: a
/// resource with a large action vocabulary (rich text's) published that
/// schema twice in every surface.
pub(crate) fn group_param(vocabulary: &Vocabulary) -> ActionParam {
    let schema = per_type(vocabulary, TypeSchema::Group, &|| {
        let declarations = vocabulary.declarations();
        let mut ids: Vec<&str> = Vec::with_capacity(declarations.len());
        for decl in declarations.iter() {
            ids.push(decl.id);
        }
        Some(group_schema(&ids, vocabulary.edits))
    })
    .unwrap_or_else(|| Arc::new(serde_json::json!({"type": "array"})));
    ActionParam::required_owned(
        crate::GROUP_PARAM,
        ParamType::Array,
        format!(
            "{} actions to apply in order as one step, each {{\"action\": id, \"args\": {{..}}}}",
            vocabulary.label
        ),
    )
    .with_shared_schema(schema)
}

/// What the value of an `apply` member of a group is: the schema it is
/// checked against is published once, on `apply` itself.
pub const APPLY_MEMBER: &str =
    "One action value, as the `apply` action's `apply` parameter describes it";

/// The schema of a group of the actions `ids`, `apply`, and field edits when
/// `edits`: one function for every resource.
fn group_schema(ids: &[&str], edits: bool) -> serde_json::Value {
    use crate::schema::{
        array, constant, describe, nullable, object, one_of, string_enum, undescribed,
    };
    let mut members = Vec::new();
    if !ids.is_empty() {
        members.push(describe(
            object(vec![
                ("action", string_enum(ids), true),
                (
                    "args",
                    nullable(serde_json::json!({"type": "object"})),
                    false,
                ),
            ]),
            "A named action, args as it takes them",
        ));
    }
    let action = describe(serde_json::json!({"type": "object"}), APPLY_MEMBER);
    if edits {
        members.push(describe(
            object(vec![
                ("action", constant(FIELD_EDIT), true),
                (
                    "args",
                    object(vec![(
                        EDIT_PARAM,
                        undescribed(&<Edit<()> as crate::JsonSchema>::json_schema()),
                        true,
                    )]),
                    true,
                ),
            ]),
            "A field edit",
        ));
    }
    members.push(object(vec![
        ("action", constant(APPLY), true),
        ("args", object(vec![(APPLY_PARAM, action, true)]), true),
    ]));
    array(one_of(members))
}

/// The JSON object of the argument `param` of `args`.
fn object_arg(
    args: &tilo_actions::ActionArgs,
    param: &str,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    match args.get(param) {
        Some(tilo_actions::ArgValue::Object(value)) => Some(value.clone()),
        _ => None,
    }
}

fn apply_spec(vocabulary: &'static Vocabulary) -> ActionSpec {
    #[cfg(all(feature = "test-support", not(target_arch = "wasm32")))]
    let _phase = crate::profiling::Phase::start("resource.apply_spec");
    let read = move |args: &tilo_actions::ActionArgs| match object_arg(args, APPLY_PARAM) {
        Some(value) => read_apply(vocabulary, serde_json::Value::Object(value)),
        None => Err("missing action".to_owned()),
    };
    ActionSpec::typed(
        APPLY,
        format!("Apply {} edit", vocabulary.label),
        "Action as JSON",
        ExecutionLocus::ClientLocal,
        vec![apply_param(vocabulary)],
        move |args| match read(args) {
            Ok(mut action) => {
                action.stamp(origin_now_ms());
                encoded(&action)
            }
            Err(reason) => refused(&reason),
        },
    )
    .with_description(
        "One edit as a typed JSON value: {action: id, ...its params}. The named action does the same and says more.",
    )
    // A typed edit is how a surface, an agent or a script says what should
    // happen without a named declaration; a person picks named actions.
    .for_audience(tilo_actions::Audience::Agents)
    // A typed value of the resource's own vocabulary changes only the
    // document, and the host's undo reverts it.
    .reversible()
    // History records the edit it carried, not `apply`.
    .carrying()
    // Offered to whoever may take the least of its edits; each value is
    // what that edit is.
    .with_need((vocabulary.least_need)())
    .with_need_of(move |args| match read(args) {
        Ok(action) => action.need(),
        Err(_) => tilo_actions::ActionContract::default().need(),
    })
    // Each value is classified: deleting is destructive, trimming is not, so
    // a destructive value gets the confirmation a declared one would.
    .with_risk(move |args| {
        let value = object_arg(args, APPLY_PARAM).map(serde_json::Value::Object);
        match value.map(vocabulary.read) {
            Some(Ok(action)) => action.risk(),
            _ => tilo_actions::RiskLevel::Low,
        }
    })
}

/// What kind of action a published spec is: a view only reads, an effect
/// reaches outside the document, and a document action changes only the
/// document when the host's undo reverts it, else it reaches outside.
#[must_use]
pub fn spec_kind(spec: &ActionSpec) -> ActionKind {
    if spec
        .contract
        .tags
        .iter()
        .any(|tag| tag == crate::effect::EDITS_TAG)
    {
        return ActionKind::Undoable;
    }
    match spec.kind {
        OperationKind::View => ActionKind::Read,
        OperationKind::Document if spec.contract.reversible => ActionKind::Undoable,
        OperationKind::Effect | OperationKind::Document => ActionKind::Outside,
    }
}

/// What `action` is, for the grants deciding it.
pub fn need_of<R: Resource>(action: &R::Action) -> ActionNeed {
    ActionNeed {
        kind: R::kind_of(action),
        field: R::field_of(action).map(str::to_owned),
        privileged: false,
    }
}

/// What several actions applied as one are: the strongest kind among them,
/// below a field only when every one writes that same field. Nothing to
/// apply is an undoable change of the record.
pub(crate) fn group_need(needs: impl IntoIterator<Item = ActionNeed>) -> ActionNeed {
    let mut needs = needs.into_iter();
    let Some(mut group) = needs.next() else {
        return tilo_actions::ActionContract::default().need();
    };
    for need in needs {
        group = ActionNeed {
            kind: group.kind.max(need.kind),
            field: group
                .field
                .filter(|field| need.field.as_ref() == Some(field)),
            privileged: group.privileged || need.privileged,
        };
    }
    group
}

/// The encoded group of the built `members`, of the resource `vocabulary`:
/// what [`ActionGroup`](crate::ActionGroup) of them encodes as. A group of
/// one action is that action, as `Dispatcher::group` submits it: a control
/// whose gesture is sometimes several actions (create a draft, then edit
/// it) and usually one edit records that edit, which continues the undo
/// step of the edit before it as the edit alone would.
pub(crate) fn encode_group(vocabulary: &Vocabulary, members: &[Built]) -> EncodedAction {
    if let [only] = members {
        return encoded(only);
    }
    let mut payload = Vec::with_capacity(128);
    payload.push(b'[');
    for (index, member) in members.iter().enumerate() {
        if index > 0 {
            payload.push(b',');
        }
        if let Err(error) = member.write_json(&mut payload) {
            return refused(&error.to_string());
        }
    }
    payload.push(b']');
    EncodedAction::written(crate::group::group_type_of(vocabulary.action_type), payload)
}

fn group_spec(vocabulary: &'static Vocabulary) -> ActionSpec {
    #[cfg(all(feature = "test-support", not(target_arch = "wasm32")))]
    let _phase = crate::profiling::Phase::start("resource.group_spec");
    let build = move |args: &tilo_actions::ActionArgs, now_ms: i64| {
        crate::group::build_members(vocabulary, &args.to_json(), now_ms)
    };
    ActionSpec::typed(
        crate::GROUP,
        format!("Apply several {} edits as one", vocabulary.label),
        "Invocations as JSON",
        ExecutionLocus::ClientLocal,
        vec![group_param(vocabulary)],
        move |args| match build(args, origin_now_ms()) {
            Ok(members) => encode_group(vocabulary, &members),
            Err(reason) => refused(&reason),
        },
    )
    .with_description(
        "Several actions as one all-or-nothing step: one undo, and if any member refuses nothing is applied. actions: [{action: id, args: {..}}].",
    )
    // One group is one undo step, like any other document action.
    .reversible()
    // A gesture's edits, from a surface, a script or an agent; a person picks
    // the named actions it is made of.
    .for_audience(tilo_actions::Audience::Agents)
    // History records the members, not the group.
    .carrying()
    // Is what its most demanding member is.
    .with_need((vocabulary.least_need)())
    .with_need_of(move |args| match build(args, 0) {
        Ok(members) => group_need(members.iter().map(Built::need)),
        Err(_) => tilo_actions::ActionContract::default().need(),
    })
    // As consequential as its most consequential member.
    .with_risk(move |args| match build(args, 0) {
        Ok(members) => crate::group::group_risk(members.iter().map(Built::risk)),
        Err(_) => tilo_actions::RiskLevel::Low,
    })
    // Undo takes it back only when it takes back every member: one that
    // reaches outside the document makes a deleting group ask.
    .with_undo_of(move |args| match build(args, 0) {
        Ok(members) => members.iter().all(Built::undoable),
        Err(_) => false,
    })
}

/// The default search entries of a record: one per item with a
/// `#[model(label)]`, anchored at its item resource, or else all of its text
/// in one entry, found by its text. Items of a record holding several collections are listed
/// under each collection's title (its doc comment's first line: "Folders",
/// "Articles"), so the kinds read apart. An item is found by all of its
/// text, which is no preview of it: the palette shows none for it. A
/// `#[resource(search = path)]` function extends it by calling it.
#[must_use]
pub fn default_search<R: crate::Resource + crate::Fields>(
    label: &str,
    record: &R,
) -> Vec<crate::SearchEntry> {
    let items = crate::Fields::item_search(record);
    let mut text = Vec::new();
    if items.is_empty() {
        record.collect_text(&mut text);
    }
    let titles: &[crate::shape::FieldShape] = match <R as crate::Model>::SHAPE {
        crate::Shape::Record(fields) => fields,
        _ => &[],
    };
    default_search_of(R::KIND, label, items, &text, titles)
}

/// [`default_search`] of a record of the kind `kind` holding `items`, or
/// else the text `text`; `fields` names its collections.
fn default_search_of(
    kind: &str,
    label: &str,
    items: Vec<crate::items::ItemText>,
    text: &[String],
    fields: &[crate::shape::FieldShape],
) -> Vec<crate::SearchEntry> {
    if !items.is_empty() {
        let several = items
            .iter()
            .any(|item| items.first().is_some_and(|first| first.field != item.field));
        let title_of = |field: &str| {
            fields
                .iter()
                .find(|shape| shape.name == field)
                .map(|shape| shape.title)
        };
        let mut entries = Vec::with_capacity(items.len());
        for item in items {
            let anchor = crate::items::item_resource_id_in(kind, item.field, &item.id);
            let mut entry = crate::SearchEntry::new(item.label, String::new()).anchored(anchor);
            entry.fields.push((item.text, crate::SearchWeight::Body));
            if several && let Some(title) = title_of(item.field) {
                entry = entry.grouped(title);
            }
            entries.push(entry);
        }
        return entries;
    }
    if text.is_empty() {
        return Vec::new();
    }
    vec![crate::SearchEntry {
        anchor: None,
        title: text.first().cloned().unwrap_or_else(|| label.to_owned()),
        fields: vec![(text.join("\n"), crate::SearchWeight::Body)],
        ..Default::default()
    }]
}

/// The resource tree published for one open resource: every declared and
/// state-bound action, the generic [`APPLY`] and [`GROUP`](crate::GROUP)
/// actions, the named [`Resource::views`], and the nested resources.
///
/// This is the only bridge from a [`Resource`] type to the
/// [`tilo_actions::Resource`] tree the palette, agents, scripts and the shell
/// door see. Apps do not construct a second tree for the same state.
///
/// An action offered on the items of a collection (see [`crate::items`])
/// stays published on the record, with its id parameter, for scripts and
/// agents; a person picks it on the item, so it is not a palette row here.
pub fn publish<R: Resource>(id: &str, label: &str, state: &R::State) -> tilo_actions::Resource {
    publish_parts::<R>(id, label, Some(state))
}

/// [`publish`], or without a state only what does not depend on one: the
/// declared actions, [`APPLY`], [`GROUP`](crate::GROUP) and the local
/// operations.
pub(crate) fn publish_parts<R: Resource>(
    id: &str,
    label: &str,
    state: Option<&R::State>,
) -> tilo_actions::Resource {
    let vocabulary = vocabulary::<R>();
    let Some(state) = state else {
        let own = Own {
            availability: None,
            views: Vec::new(),
            local_operations: R::local_operations(),
            now: Vec::new(),
        };
        return publish_own(vocabulary, id, label, own, &|_| false);
    };
    let children = R::children(state);
    let own = Own {
        availability: Some(&|action| R::availability(state, action)),
        views: R::views(state),
        local_operations: R::local_operations(),
        now: weakening_now::<R>(state),
    };
    publish_own(vocabulary, id, label, own, &|decl| {
        crate::items::offered_bound(&children, decl)
    })
    .with_children(children)
}

/// What a resource's own publication reads of its state.
struct Own<'a> {
    /// Whether each declared action can be taken now; without a state,
    /// none is decided.
    availability: Option<&'a dyn Fn(&str) -> tilo_actions::Availability>,
    views: Vec<tilo_actions::ResourceViewSpec>,
    local_operations: Vec<ActionSpec>,
    /// The values now of the fields that weaken one way ([`weakening_now`]).
    now: Vec<(String, String)>,
}

/// The resource's own part of [`publish_parts`]: everything but its
/// children, with `offered_bound` saying which declarations a child offers
/// with arguments bound.
fn publish_own(
    vocabulary: &'static Vocabulary,
    id: &str,
    label: &str,
    own: Own<'_>,
    offered_bound: &dyn Fn(&Declaration) -> bool,
) -> tilo_actions::Resource {
    let mut resource = tilo_actions::Resource::new(id, vocabulary.kind, label);
    // Describe the declared domain ID before any host dispatch alias projection.
    resource.semantic_identity = tilo_actions::ResourceSemanticIdentity::new(vocabulary.kind, id);
    for decl in vocabulary.declarations().iter() {
        let _phase = crate::profiling::Phase::start("resource.declaration_spec");
        let mut spec = spec_of_declaration(decl);
        if let Some(availability) = own.availability {
            spec.contract.availability = availability(decl.id);
        }
        // Offered on the items it acts on, with some of its arguments bound
        // ([`items::bound`](crate::items::bound)), it stays published here
        // for scripts that name the item, out of the palette's way.
        resource = resource.with_action(if offered_bound(decl) {
            spec.for_audience(tilo_actions::Audience::Agents)
        } else {
            spec
        });
    }
    for view in own.views {
        resource = resource.with_view(view);
    }
    crate::share::offer(&mut resource);
    for spec in own.local_operations {
        if spec.kind == OperationKind::Document {
            continue;
        }
        resource = resource.with_action(spec);
    }
    resource = resource
        .with_action(apply_spec(vocabulary))
        .with_action(group_spec(vocabulary));
    if vocabulary.edits {
        resource = resource.with_action(field_edit_spec(vocabulary, own.now));
    }
    resource
}

/// Build the action a declaration produces for JSON arguments, validating them
/// against its parameters exactly as the host does.
///
/// # Errors
/// Returns why the action is unknown or the arguments do not bind.
pub fn build_action<R: Resource>(
    id: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
    now_ms: i64,
) -> Result<R::Action, String> {
    typed::<R>(build_action_of(vocabulary::<R>(), id, arguments, now_ms)?)
}

/// [`build_action`] of the resource `vocabulary`.
pub(crate) fn build_action_of(
    vocabulary: &Vocabulary,
    id: &str,
    arguments: &serde_json::Map<String, serde_json::Value>,
    now_ms: i64,
) -> Result<Built, String> {
    if id == crate::GROUP {
        return Err("a group is several actions; build it with `group::build_group`".into());
    }
    let read = if id == APPLY {
        Some((APPLY_PARAM, "missing action"))
    } else if id == FIELD_EDIT && vocabulary.edits {
        Some((EDIT_PARAM, "missing edit"))
    } else {
        None
    };
    if let Some((param, missing)) = read {
        let Some(value) = arguments.get(param).cloned() else {
            return Err(missing.to_owned());
        };
        let mut action = if id == APPLY {
            read_apply(vocabulary, value)?
        } else {
            read_edit(vocabulary, value)?
        };
        action.stamp(now_ms);
        return Ok(action);
    }
    let declarations = vocabulary.declarations();
    let Some(decl) = declarations.iter().find(|decl| decl.id == id) else {
        return Err(format!("{} has no action `{id}`", vocabulary.label));
    };
    let args = match ActionArgs::bind_json(&decl.params, arguments) {
        Ok(args) => args,
        Err(error) => return Err(error.to_string()),
    };
    (decl.build)(&args, now_ms)
}

/// `resource` with the bound edit `decl` published on it.
///
/// An id the resource does not declare is added. One it declares is kept,
/// unless `decl` says it refines it ([`ActionDecl::refining`]) and takes the
/// same parameters with the same permission and risk: then it builds the
/// same edit from the same arguments and only adds where it was invoked from
/// (an append that leaves its writer's caret after the text), and replaces
/// the declared spec while the binding lasts.
#[must_use]
pub fn bind_declaration(
    resource: tilo_actions::Resource,
    decl: &Declaration,
) -> tilo_actions::Resource {
    let spec = spec_of_declaration(decl);
    let mut resource = resource;
    match resource
        .actions
        .iter_mut()
        .find(|existing| existing.id == decl.id)
    {
        Some(declared) => {
            if decl.refines && same_shape(&spec, declared) {
                *declared = spec;
            }
            resource
        }
        None => resource.with_action(spec),
    }
}

fn same_shape(bound: &ActionSpec, declared: &ActionSpec) -> bool {
    let shape = |spec: &ActionSpec| {
        spec.params()
            .iter()
            .map(|param| (param.name.to_string(), param.ty, param.required))
            .collect::<Vec<_>>()
    };
    bound.kind == declared.kind
        && shape(bound) == shape(declared)
        && bound.contract.kind == declared.contract.kind
        && bound.contract.field == declared.contract.field
        && bound.contract.risk == declared.contract.risk
}

/// A resource of the person's own that one app keeps for every app of
/// theirs: their accounts, which Settings keeps and Mail signs in to. Each
/// app holds its copy in its own store; the copies are one record, the same
/// on every device, and converge over the person's own room. Only the
/// [`OWNER`](Self::OWNER) and the [`EDITORS`](Self::EDITORS) change it; every
/// other app reads it (`ViewCx::shared_person` in `tilo-app`).
pub trait SharedRecord: Resource {
    /// The app that keeps the record.
    const OWNER: &'static str;
    /// The other apps whose edits the record takes.
    const EDITORS: &'static [&'static str] = &[];

    /// Whether the app `app_id` changes the record.
    #[must_use]
    fn edited_by(app_id: &str) -> bool {
        app_id == Self::OWNER || Self::EDITORS.contains(&app_id)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchRow {
    /// The dimmer line under the title (an address, a path, a date).
    pub subtitle: Option<String>,
    /// A named icon from the icon set (`"mail"`, `"calendar"`); `None` draws
    /// the app's own.
    pub icon: Option<String>,
    /// What the hit is, for the row's shape.
    pub shape: SearchRowShape,
}

impl SearchRow {
    /// A generic row with `subtitle` under the title.
    pub fn subtitle(subtitle: impl Into<String>) -> Self {
        Self {
            subtitle: Some(subtitle.into()),
            ..Self::default()
        }
    }

    /// A row of `shape`.
    #[must_use]
    pub fn shaped(shape: SearchRowShape) -> Self {
        Self {
            shape,
            ..Self::default()
        }
    }

    /// The row's named icon.
    #[must_use]
    pub fn icon(mut self, name: impl Into<String>) -> Self {
        self.icon = Some(name.into());
        self
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum SearchRowShape {
    /// Icon, title and subtitle.
    #[default]
    Plain,
    /// A message: who sent it, a snippet, when, and whether it is unread.
    Message {
        sender: String,
        snippet: Option<String>,
        date: Option<String>,
        unread: bool,
    },
    /// A video, track or other media item: who made it and how long it
    /// runs. (A provider with artwork answers its own rows, with
    /// `AppImage::from_image_store` thumbnails.)
    Media {
        byline: Option<String>,
        duration: Option<String>,
    },
    /// A match inside a file: the path, the line and the matched line.
    FileHit {
        path: String,
        line: Option<u32>,
        snippet: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
pub struct ScopeDecl {
    pub anchor: String,
    pub label: String,
}

impl ScopeDecl {
    pub fn new(anchor: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            anchor: anchor.into(),
            label: label.into(),
        }
    }

    /// Whether the hit anchored at `anchor` lies inside this scope.
    #[must_use]
    pub fn contains(&self, anchor: &str) -> bool {
        scope_contains(&self.anchor, anchor)
    }
}

/// Whether `anchor` is `scope` or lies beneath it in the anchor path.
#[must_use]
pub fn scope_contains(scope: &str, anchor: &str) -> bool {
    anchor
        .strip_prefix(scope)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
}

#[cfg(test)]
mod schema_sharing_tests {
    use super::*;

    #[test]
    fn shared_schema_reloads_do_not_retain_or_repopulate_stale_generation() {
        let cache = Mutex::new(CompiledSchemas::default());
        let key = (TypeId::of::<()>(), TypeSchema::Apply, String::new());
        let first = cached_schema(&cache, key.clone(), &|| {
            Some(serde_json::json!({"minimum":1}))
        })
        .unwrap();
        let reused = cached_schema(&cache, key.clone(), &|| {
            panic!("cached type must not rebuild")
        })
        .unwrap();
        assert!(Arc::ptr_eq(&first, &reused));
        {
            let mut store = cache.lock().unwrap();
            store.epoch += 1;
            store.entries.clear();
        }
        let old_build = cached_schema(&cache, key.clone(), &|| {
            let mut store = cache.lock().unwrap();
            store.epoch += 1;
            store.entries.clear();
            drop(store);
            Some(serde_json::json!({"minimum":2}))
        })
        .unwrap();
        assert!(
            cache.lock().unwrap().entries.is_empty(),
            "old build cannot repopulate new generation"
        );
        let current =
            cached_schema(&cache, key, &|| Some(serde_json::json!({"minimum":3}))).unwrap();
        assert_eq!(first["minimum"], 1);
        assert_eq!(old_build["minimum"], 2);
        assert_eq!(current["minimum"], 3);
        assert!(!Arc::ptr_eq(&first, &current));
    }
}

#[cfg(test)]
mod selection_place_tests {
    use super::Place;
    use tilo_actions::SelectionPlace;

    #[test]
    fn a_selection_place_is_the_place_an_action_takes() {
        for place in [
            Place::Item("p1".into()),
            Place::Caret {
                item: "p1".into(),
                at: 3,
            },
            Place::Range {
                item: "p1".into(),
                start: 2,
                end: 7,
            },
        ] {
            let selection = SelectionPlace::from(place.clone());
            assert_eq!(
                serde_json::to_value(&selection).unwrap(),
                serde_json::to_value(&place).unwrap(),
                "both travel in one wire form"
            );
            assert_eq!(Place::try_from(selection).unwrap(), place);
        }
        assert!(Place::try_from(SelectionPlace::item("")).is_err());
    }
}
