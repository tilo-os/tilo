//! The resource contract: what any resource type provides so that it gets
//! history, undo, collaboration, permissions, palette/agent/script exposure,
//! search and replay without writing any of them.

use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use tilo_actions::{
    ActionArgs, ActionError, ActionParam, ActionRegistry, ActionSpec, EncodedAction,
    ExecutionLocus, OperationKind, ParamType, PermissionRequirement,
};

use crate::delta::Delta;
use crate::model::{Doc, ModelError};
use crate::reader::Demand;

/// A resource type. Implement it by hand for a type with its own storage
/// layout and merge semantics (a timeline, a rich-text document, a photo edit
/// stack); `#[resource]` implements it for a `#[derive(Model)]` record.
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

    /// Where `obj` sits in the document, when the state already knows: the
    /// path from the root to it. A change to an object the state has read
    /// (the paragraph being typed in) is then placed without asking the
    /// document, which for a large document is the costly part of reading a
    /// change. The default knows nothing, and the document is asked.
    fn locate(state: &Self::State, obj: &automerge::ObjId) -> Option<crate::delta::Path> {
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
    /// Only edits applied immediately (`Dispatcher::apply_now` in
    /// `tilo-app`) continue one another, and a surface ends the gesture
    /// (`Dispatcher::settle`) when the pointer is let go, so the next drag
    /// of the same slider is a step of its own.
    fn coalesces(previous: &Self::Action, next: &Self::Action) -> bool {
        let _ = (previous, next);
        false
    }

    /// Whether `action` only annotates what the action before it in the
    /// same event wrote (who wrote the text just typed): it joins that
    /// step, and the next action continues the one it annotates.
    fn annotates(action: &Self::Action) -> bool {
        let _ = action;
        false
    }

    /// Mutations offered by name. The generic [`APPLY`] action is always
    /// offered in addition, so any typed value can reach the host.
    fn declarations() -> Vec<ActionDecl<Self>> {
        Vec::new()
    }

    /// Mutations whose arguments are bound from `state` at publish time
    /// (clear-all that names the current rows): typed builders of
    /// [`Action`](Self::Action), published beside
    /// [`declarations`](Self::declarations) and checked, confirmed and undone
    /// the same way. Prefer [`declarations`](Self::declarations) when the
    /// action does not need the state. An action bound to where a person is
    /// rather than to the document (the selection, the current folder) is
    /// bound by the view or surface that knows it.
    fn bound_declarations(_state: &Self::State) -> Vec<ActionDecl<Self>> {
        Vec::new()
    }

    /// View and effect operations that depend on `state` (start or stop,
    /// carrying the current on/off), published on the same resource as
    /// [`declarations`](Self::declarations). Static ones stay on
    /// [`local_operations`](Self::local_operations); a mutation bound from
    /// the state is a [`bound_declarations`](Self::bound_declarations) entry,
    /// never a hand-built document spec here.
    fn bound_actions(_state: &Self::State) -> Vec<tilo_actions::ActionSpec> {
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
    fn search(_state: &Self::State) -> Vec<SearchEntry> {
        Vec::new()
    }

    /// The stored layout of this resource's content, for services that act
    /// on it generically: History restores a past version through it, field
    /// by field and item by item. `#[resource]` returns the record's
    /// [`Model::SHAPE`](crate::Model::SHAPE); the default opts out.
    fn shape() -> Option<crate::Shape> {
        None
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
/// It is programmatic: never offered in the palette or a menu, where a
/// person picks named actions. Its risk is the value's own
/// ([`tilo_actions::Action::risk`]), so a destructive value — deleting a clip —
/// needs the same confirmation a declared destructive action does, while an
/// ordinary edit applies directly.
pub const APPLY: &str = "apply";
/// The parameter of [`APPLY`] carrying the JSON-encoded action.
pub const APPLY_PARAM: &str = "action";

/// A search index entry. `anchor` addresses what a hit opens (an item id);
/// `action`, when set, is the published action a hit invokes, so choosing it
/// goes through the same door, permissions and history as the palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchEntry {
    pub anchor: Option<String>,
    pub title: String,
    pub text: String,
    pub action: Option<tilo_actions::ActionAddress>,
}

impl SearchEntry {
    /// An entry titled `title` whose body text is `text`.
    pub fn new(title: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            anchor: None,
            title: title.into(),
            text: text.into(),
            action: None,
        }
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
}

/// Builds a resource's action from arguments already bound against the
/// declaration's parameters. `now_ms` is the origin's clock, captured once so
/// replay is deterministic.
pub type BuildAction<R> =
    Arc<dyn Fn(&ActionArgs, i64) -> Result<<R as Resource>::Action, String> + Send + Sync>;

/// One mutation offered by name.
pub struct ActionDecl<R: Resource> {
    pub id: &'static str,
    pub label: String,
    pub description: Option<String>,
    pub params: Vec<ActionParam>,
    pub permission: PermissionRequirement,
    pub destructive: bool,
    pub build: BuildAction<R>,
}

impl<R: Resource> Clone for ActionDecl<R> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            label: self.label.clone(),
            description: self.description.clone(),
            params: self.params.clone(),
            permission: self.permission,
            destructive: self.destructive,
            build: Arc::clone(&self.build),
        }
    }
}

impl<R: Resource> ActionDecl<R> {
    pub fn new(
        id: &'static str,
        label: impl Into<String>,
        params: Vec<ActionParam>,
        build: impl Fn(&ActionArgs, i64) -> Result<R::Action, String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            id,
            label: label.into(),
            description: None,
            params,
            permission: PermissionRequirement::Write,
            destructive: false,
            build: Arc::new(build),
        }
    }

    #[must_use]
    pub fn describe(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    #[must_use]
    pub fn requiring(mut self, permission: PermissionRequirement) -> Self {
        self.permission = permission;
        self
    }

    #[must_use]
    pub fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }
}

/// Milliseconds since the Unix epoch on this machine. Only ever read on the
/// origin, when an action is built; never inside `apply`.
#[must_use]
pub fn origin_now_ms() -> i64 {
    i64::try_from(tilo_clock::unix_now_ms()).unwrap_or(i64::MAX)
}

/// Encode a resource action under its registry type id.
///
/// # Errors
/// Returns [`ActionError::Encode`] when the action does not serialize.
pub fn encode<R: Resource>(action: &R::Action) -> Result<EncodedAction, ActionError> {
    EncodedAction::encode(R::ACTION_TYPE, action)
}

/// What history calls one action value of `R`: the label of the declared
/// action it names (a `remove-marks` value is "Remove marks"), else its
/// variant name in words, or `None` when it names neither.
///
/// The generic [`APPLY`] and a [`GROUP`](crate::GROUP) of one kind of edit
/// are recorded under this name, so a person's history says what each edit
/// did rather than that an edit was applied.
pub fn action_label<R: Resource>(action: &R::Action) -> Option<String> {
    serde_json::to_value(action)
        .ok()
        .and_then(|value| value_label::<R>(&value))
}

fn value_label<R: Resource>(value: &serde_json::Value) -> Option<String> {
    let declarations = R::declarations();
    let ids: Vec<&str> = declarations.iter().map(|decl| decl.id).collect();
    if let Some((id, _)) = crate::schema::declared_invocation(value, &ids) {
        return declarations
            .into_iter()
            .find(|decl| decl.id == id)
            .map(|decl| decl.label);
    }
    let value = value.get("op").unwrap_or(value);
    let variant = match value {
        serde_json::Value::String(variant) => variant.as_str(),
        serde_json::Value::Object(fields) => match fields.get("action") {
            Some(serde_json::Value::String(variant)) => variant.as_str(),
            _ if fields.len() == 1 => fields.keys().next()?.as_str(),
            _ => return None,
        },
        _ => return None,
    };
    let words = in_words(variant);
    (!words.is_empty()).then_some(words)
}

/// `remove-marks`, `remove_marks` and `RemoveMarks` as "Remove marks".
fn in_words(identifier: &str) -> String {
    let mut words = String::with_capacity(identifier.len().saturating_add(4));
    let mut previous_lowercase_or_digit = false;
    for character in identifier.chars() {
        if matches!(character, '-' | '_' | ' ') {
            if !words.is_empty() && !words.ends_with(' ') {
                words.push(' ');
            }
            previous_lowercase_or_digit = false;
            continue;
        }
        if character.is_uppercase() && previous_lowercase_or_digit {
            words.push(' ');
        }
        previous_lowercase_or_digit = character.is_lowercase() || character.is_ascii_digit();
        if words.is_empty() {
            words.extend(character.to_uppercase());
        } else {
            words.extend(character.to_lowercase());
        }
    }
    words.trim_end().to_owned()
}

/// The history name of an encoded [`APPLY`]: its action's
/// [`action_label`].
fn apply_label<R: Resource>(encoded: &EncodedAction) -> Option<String> {
    if encoded.type_id != R::ACTION_TYPE {
        return None;
    }
    let value = serde_json::from_slice::<serde_json::Value>(&encoded.payload).ok()?;
    value_label::<R>(&value)
}

/// The history name of an encoded [`GROUP`](crate::GROUP) whose members are
/// all one kind of edit (a selection's moves are "Edit marks"); a mixed group
/// keeps the group's own label.
fn group_label<R: Resource>(encoded: &EncodedAction) -> Option<String> {
    if encoded.type_id != crate::group::group_type::<R>() {
        return None;
    }
    let members = serde_json::from_slice::<Vec<serde_json::Value>>(&encoded.payload).ok()?;
    let mut labels = members.iter().map(value_label::<R>);
    let first = labels.next()??;
    labels
        .all(|label| label.as_deref() == Some(first.as_str()))
        .then_some(first)
}

/// The encoded form of a builder's result: the action, or a refusal that
/// carries the builder's reason.
pub(crate) fn encode_built<R: Resource>(built: Result<R::Action, String>) -> EncodedAction {
    match built {
        Ok(action) => encode::<R>(&action).unwrap_or_else(|error| refused::<R>(&error.to_string())),
        Err(reason) => refused::<R>(&reason),
    }
}

/// An action whose payload cannot be decoded, so the host refuses it with
/// the builder's reason instead of applying anything.
fn refused<R: Resource>(reason: &str) -> EncodedAction {
    let _ = R::ACTION_TYPE;
    EncodedAction {
        type_id: tilo_actions::resource::INVALID_ARGUMENTS_TYPE.to_owned(),
        payload: reason.as_bytes().to_vec(),
    }
}

/// The published spec of one declared or bound mutation: what the palette,
/// agents and scripts invoke, building `R::Action` from their arguments.
pub fn declaration_spec<R: Resource>(decl: &ActionDecl<R>) -> ActionSpec {
    let build = Arc::clone(&decl.build);
    let mut spec = ActionSpec::typed(
        decl.id,
        decl.label.clone(),
        decl.label.clone(),
        ExecutionLocus::ClientLocal,
        decl.params.clone(),
        move |args| match build(args, origin_now_ms()) {
            Ok(action) => {
                encode::<R>(&action).unwrap_or_else(|error| refused::<R>(&error.to_string()))
            }
            Err(reason) => refused::<R>(&reason),
        },
    )
    .requiring(decl.permission)
    // Every document action is undone by the host's undo.
    .reversible();
    if let Some(description) = &decl.description {
        spec = spec.with_description(description.clone());
    }
    if decl.destructive {
        spec = spec.destructive();
    }
    spec
}

/// Each declared action's arguments object, as serde reads it.
fn declared_args<R: Resource>() -> std::collections::BTreeMap<&'static str, serde_json::Value> {
    R::declarations()
        .iter()
        .map(|decl| (decl.id, crate::schema::args_schema(&decl.params)))
        .collect()
}

/// The parameter of [`APPLY`]: one action value, described by
/// [`Resource::action_schema`] when the resource has one, with what only
/// restates a named action folded into a reference to it.
fn apply_param<R: Resource>() -> ActionParam {
    let param = ActionParam::required_owned(
        APPLY_PARAM,
        ParamType::Object,
        format!("A {} action, as JSON", R::LABEL),
    );
    match R::action_schema() {
        Some(schema) => {
            param.with_schema(crate::schema::fold_declared(&schema, &declared_args::<R>()))
        }
        None => param,
    }
}

/// A JSON action value as [`APPLY`] reads it. A value naming a declared
/// action is checked against that declaration first, so it is refused with
/// the reason the named action would give.
fn read_apply<R: Resource>(value: serde_json::Value) -> Result<R::Action, String> {
    let declarations = R::declarations();
    let ids: Vec<&str> = declarations.iter().map(|decl| decl.id).collect();
    if let Some((id, fields)) = crate::schema::declared_invocation(&value, &ids)
        && let Some(decl) = declarations.iter().find(|decl| decl.id == id)
    {
        ActionArgs::bind_json(&decl.params, &fields).map_err(|error| format!("{id}: {error}"))?;
    }
    serde_json::from_value::<R::Action>(value).map_err(|error| error.to_string())
}

/// The parameter of [`GROUP`](crate::GROUP): invocations of the resource's
/// declared actions and of [`APPLY`]. A declared member's arguments are
/// checked against its own action's schema when the group is built, so this
/// names those actions rather than repeating their schemas; an `apply`
/// member carries `apply`'s schema, without its descriptions.
fn group_param<R: Resource>() -> ActionParam {
    use crate::schema::{
        array, constant, describe, nullable, object, one_of, string_enum, undescribed,
    };

    let ids: Vec<&str> = R::declarations().iter().map(|decl| decl.id).collect();
    let mut members = Vec::new();
    if !ids.is_empty() {
        members.push(describe(
            object(vec![
                ("action", string_enum(&ids), true),
                (
                    "args",
                    nullable(serde_json::json!({"type": "object"})),
                    false,
                ),
            ]),
            "A named action, args as it takes them",
        ));
    }
    let action = apply_param::<R>().schema.map_or_else(
        || serde_json::json!({"type": "object"}),
        |schema| undescribed(&schema),
    );
    members.push(object(vec![
        ("action", constant(APPLY), true),
        ("args", object(vec![(APPLY_PARAM, action, true)]), true),
    ]));
    ActionParam::required_owned(
        crate::GROUP_PARAM,
        ParamType::Array,
        format!(
            "{} actions to apply in order as one step, each {{\"action\": id, \"args\": {{..}}}}",
            R::LABEL
        ),
    )
    .with_schema(array(one_of(members)))
}

fn apply_spec<R: Resource>() -> ActionSpec {
    ActionSpec::typed(
        APPLY,
        format!("Apply {} edit", R::LABEL),
        "Action as JSON",
        ExecutionLocus::ClientLocal,
        vec![apply_param::<R>()],
        |args| {
            let Some(tilo_actions::ArgValue::Object(value)) = args.get(APPLY_PARAM) else {
                return refused::<R>("missing action");
            };
            match read_apply::<R>(serde_json::Value::Object(value.clone())) {
                Ok(action) => {
                    encode::<R>(&action).unwrap_or_else(|error| refused::<R>(&error.to_string()))
                }
                Err(reason) => refused::<R>(&reason),
            }
        },
    )
    .with_description(
        "One edit as a typed JSON value: {action: id, ...its params}. The named action does the same and says more.",
    )
    // A typed edit is how a surface, an agent or a script says what should
    // happen without a named declaration; a person picks named actions.
    .programmatic()
    // History names each value after what it did, not after `apply`.
    .with_history_label(apply_label::<R>)
    // Each value is classified: deleting is destructive, trimming is not, so
    // a destructive value gets the confirmation a declared one would.
    .with_risk(|args| {
        let Some(tilo_actions::ArgValue::Object(value)) = args.get(APPLY_PARAM) else {
            return tilo_actions::RiskLevel::Low;
        };
        serde_json::from_value::<R::Action>(serde_json::Value::Object(value.clone()))
            .map_or(tilo_actions::RiskLevel::Low, |action| {
                tilo_actions::Action::risk(&action)
            })
    })
}

fn group_spec<R: Resource>() -> ActionSpec {
    ActionSpec::typed(
        crate::GROUP,
        format!("Apply several {} edits as one", R::LABEL),
        "Invocations as JSON",
        ExecutionLocus::ClientLocal,
        vec![group_param::<R>()],
        |args| match crate::group::build_group::<R>(&args.to_json(), origin_now_ms()) {
            Ok(group) => EncodedAction::encode(crate::group::group_type::<R>(), &group)
                .unwrap_or_else(|error| refused::<R>(&error.to_string())),
            Err(reason) => refused::<R>(&reason),
        },
    )
    .with_description(
        "Several actions as one all-or-nothing step: one undo, and if any member refuses nothing is applied. actions: [{action: id, args: {..}}].",
    )
    // One group is one undo step, like any other document action.
    .reversible()
    // A group of one kind of edit is named after it.
    .with_history_label(group_label::<R>)
    // A gesture's edits, from a surface, a script or an agent; a person picks
    // the named actions it is made of.
    .programmatic()
    // As consequential as its most consequential member.
    .with_risk(|args| {
        crate::group::build_group::<R>(&args.to_json(), 0)
            .map_or(tilo_actions::RiskLevel::Low, |group| {
                tilo_actions::Action::risk(&group)
            })
    })
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
    let children = R::children(state);
    let on_items: std::collections::HashSet<&str> = children
        .iter()
        .filter(|child| crate::items::item_of_resource::<R>(child.id.as_str()).is_some())
        .flat_map(|child| child.actions.iter().map(|spec| spec.id.as_str()))
        .collect();
    let mut resource = tilo_actions::Resource::new(id, R::KIND, label);
    for decl in R::declarations() {
        let spec = declaration_spec(&decl);
        resource = resource.with_action(if on_items.contains(decl.id) {
            spec.programmatic()
        } else {
            spec
        });
    }
    for decl in R::bound_declarations(state) {
        if resource
            .actions
            .iter()
            .any(|existing| existing.id == decl.id)
        {
            continue;
        }
        resource = resource.with_action(declaration_spec(&decl));
    }
    for spec in R::bound_actions(state) {
        resource = resource.with_action(spec);
    }
    for view in R::views(state) {
        resource = resource.with_view(view);
    }
    for spec in R::local_operations() {
        if spec.kind == OperationKind::Document {
            continue;
        }
        resource = resource.with_action(spec);
    }
    resource
        .with_action(apply_spec::<R>())
        .with_action(group_spec::<R>())
        .with_children(children)
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
    if id == crate::GROUP {
        return Err("a group is several actions; build it with `group::build_group`".into());
    }
    if id == APPLY {
        let value = arguments
            .get(APPLY_PARAM)
            .cloned()
            .ok_or_else(|| "missing action".to_owned())?;
        return read_apply::<R>(value);
    }
    let decl = R::declarations()
        .into_iter()
        .find(|decl| decl.id == id)
        .ok_or_else(|| format!("{} has no action `{id}`", R::LABEL))?;
    let args = ActionArgs::bind_json(&decl.params, arguments).map_err(|error| error.to_string())?;
    (decl.build)(&args, now_ms)
}
