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

    /// Write the initial content of a brand-new record.
    ///
    /// # Errors
    /// Returns the document's write error.
    fn initialize(_doc: &mut Doc) -> Result<(), ModelError> {
        Ok(())
    }

    /// Whether `next`, applied right after `previous` by the same person,
    /// continues it as one undo step — the characters of a word typed one
    /// keystroke at a time. The default keeps every action its own step.
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

    /// Document actions whose payload is bound from `state` at publish time
    /// (clear-all that names the current rows). Prefer [`declarations`] when
    /// the payload does not need the live state. Published on the same
    /// resource as [`declarations`].
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

/// A search index entry. `anchor` addresses what a hit opens (an item id).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchEntry {
    pub anchor: Option<String>,
    pub title: String,
    pub text: String,
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

fn spec_from<R: Resource>(decl: &ActionDecl<R>) -> ActionSpec {
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

/// The resource tree published for one open resource: every declared action,
/// the generic [`APPLY`] and [`GROUP`](crate::GROUP) actions, the named
/// [`Resource::views`], and the nested resources.
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
        let spec = spec_from(&decl);
        resource = resource.with_action(if on_items.contains(decl.id) {
            spec.programmatic()
        } else {
            spec
        });
    }
    for spec in R::bound_actions(state) {
        // Document actions and state-dependent views or effects. Static views
        // and effects stay on `local_operations`.
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
