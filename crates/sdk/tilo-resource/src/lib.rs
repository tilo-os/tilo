//! # The Tilo resource layer
//!
//! A Tilo app's state is a set of **resources**: typed, versioned, shared
//! values the system owns. This crate is the layer that turns a Rust type into
//! one. It has no rendering, windowing or widget dependency. The UI toolkit
//! uses only its document-generic parts (history, stamped commits, replay);
//! `tilo-app` binds resources to views.
//!
//! ## The golden path
//!
//! ```
//! use tilo_resource::{ActionCx, Collection, Id, Model, resource};
//!
//! #[derive(Model, Default)]
//! pub struct Todo {
//!     pub items: Collection<Item>,
//! }
//!
//! #[derive(Model, Default)]
//! pub struct Item {
//!     pub title: String,
//!     pub done: bool,
//! }
//!
//! #[resource(kind = "example.todo", label = "Todo list")]
//! impl Todo {
//!     /// Add an item.
//!     #[action]
//!     pub fn add(&mut self, cx: &mut ActionCx, title: String) {
//!         self.items.push(cx.id(), Item { title, done: false });
//!     }
//!
//!     /// Mark an item done or not done.
//!     #[action]
//!     pub fn set_done(&mut self, item: Id, done: bool) -> Result<(), &'static str> {
//!         self.items.get_mut(&item).ok_or("no such item")?.done = done;
//!         Ok(())
//!     }
//! }
//!
//! // Tests are replay: apply actions the way a control, the palette, an agent
//! // or a script would, and assert on state.
//! fn main() {
//!     let mut replica = tilo_resource::Replica::<Todo>::new();
//!     replica.apply(Todo::ADD.with("Buy milk")).unwrap();
//!     let id = replica.state().unwrap().items.ids().next().unwrap().clone();
//!     replica.apply(Todo::SET_DONE.with((id, true))).unwrap();
//!     assert!(replica.state().unwrap().items.values().all(|item| item.done));
//! }
//! ```
//!
//! That is the whole app-side state layer. From it the app gets, without
//! writing any of it:
//!
//! * **storage and sync** — the record is an Automerge document; every field
//!   write is a minimal diff, text is spliced, collections are keyed and
//!   fractionally ordered, so collaborators' concurrent edits merge;
//! * **replayable actions** — each `#[action]` method is a serializable
//!   operation with typed parameters, applied deterministically (ids and time
//!   come from [`ActionCx`]) and replayed byte-identically on every replica;
//! * **exposure** — the same declarations are what the palette, agents and
//!   scripts list and invoke ([`publish`]), with permissions per action, and
//!   the generic [`APPLY`] operation publishes a JSON Schema of the action
//!   vocabulary ([`Resource::action_schema`], [`JsonSchema`]);
//! * **history** — every action is a change attributed to its author with its
//!   label and time ([`history`]); any version can be read back
//!   ([`history::read_at`]) and restored, field by field and item by item,
//!   from the record's stored [`Shape`];
//! * **search** — text fields are indexed by default ([`Resource::search`]);
//! * **lookup by kind** — a host that works with whatever a room holds
//!   reads, publishes and replays any registered type from the kind its
//!   record names ([`Kinds`], [`ResourceKind`]);
//! * **document machinery** — fractional [`order`] keys, stamped commits and
//!   positional replay ([`history`]), writing large content as a few changes
//!   ([`batch`]) and id-aligned structural [`diff`]s, shared by every
//!   resource type including the hand-written ones;
//! * **incremental reads** — after a change only what it touched is read
//!   again ([`Resource::refresh`], [`Delta`]); a record resource gets this
//!   field by field and item by item, and a resource too large to read whole
//!   reads what views look at ([`Resource::load`], [`Demand`]).
//!
//! ## Custom resource types
//!
//! A type with its own layout and merge semantics — a timeline whose clips
//! patch field by field, a rich-text document, a sheet — implements
//! [`Resource`] by hand: [`read`](Resource::read) its state from a document,
//! name its replayable [`Action`](Resource::Action) type, and optionally
//! [`declarations`](Resource::declarations) for named, typed palette entries.
//! It then gets exactly the same services. Edits without a named declaration
//! travel as the generic [`APPLY`] action, so nothing a custom surface does
//! has to bypass permissions, history or undo. `APPLY` is for surfaces,
//! agents and scripts, never a palette row; each value's own
//! [`risk`](tilo_actions::Action::risk) decides whether it needs
//! confirmation, so a destructive typed edit is confirmed exactly as a
//! declared destructive action is.

// The derives name this crate by its path, here as in an app.
extern crate self as tilo_resource;

pub mod action;
mod apply_cache;
pub mod batch;
pub mod child;
pub mod collection;
pub mod content;
#[cfg(feature = "chrono")]
mod dates;
pub mod delta;
pub mod derived;
mod describe;
pub mod diff;
pub mod effect;
pub mod evolution;
pub mod footprint;
pub mod group;
pub mod history;
pub mod items;
pub mod keyed;
pub mod kind;
pub mod model;
pub mod order;
pub mod ordered;
pub mod path;
pub mod reader;
pub mod replica;
pub mod resource;
pub mod schema;
pub mod shape;
pub mod storage;
pub mod surface;
pub mod text;

pub use action::{
    ActionCx, ActionKey, ActionKind, ActionRef, Binds, Invoke, Mapped, NoParams, Operation,
    Outcome, Params, Refusal, Reverting, Stamped, Undoing, fresh_uuid, minted_ids, reverting_type,
    take_minted,
};
/// A change a document reports, as an `answer` hook reads what its action did.
pub use automerge::Patch;
pub use child::{child_resource_id, publish_child};
pub use collection::{Collection, Id, Membership};
pub use delta::{Delta, ObjPath, Touch, Touched, Tracker};
pub use derived::{Bound, Derived};
pub use effect::{Answers, Effect, EffectDecl, NoEffect};
pub use evolution::Unreadable;
pub use group::{ActionGroup, GROUP, GROUP_PARAM};
pub use keyed::{ById, Identified, ItemStorage, Items, Keyed};
pub use kind::{Kinds, ResourceKind};
pub use model::{
    Atom, Doc, Fields, Json, LocatedAt, MapSlot, Model, ModelError, Slot, edit_whole, load_slot,
    read_key, read_slots, refresh_key, take_slot, write_key, write_key_changed,
};
pub use ordered::{Arranged, Ordered};
pub use path::{Edit, EditOp, Operations, Path, Segment, Settable};
pub use reader::{Demand, Reader, heads_of};
pub use replica::Replica;
pub use resource::{
    APPLY, APPLY_PARAM, ActionDecl, BuildAction, EDIT_PARAM, FIELD_EDIT, NoAction, Place, Resource,
    ScopeDecl, SearchBody, SearchEntry, SearchPiece, SearchRow, SearchRowShape, SearchText,
    SearchWeight, SharedRecord, bind_declaration, build_action, child_by_id, declaration_spec,
    default_search, encode, need_of, origin_now_ms, publish, publish_head, reset_type_schemas,
    scope_contains, spec_kind, with_origin_clock,
};
pub use schema::JsonSchema;
pub use shape::{FieldShape, Shape, StoredText, stored_text};
pub use storage::{Bytes, Counter, Grid, Outbox, OutboxEntry, Set, Settled};
pub use surface::Surface;
pub use text::{PlainText, TextEdit};
pub use tilo_actions::Availability;
pub use tilo_resource_macros::{Effect, JsonSchema, Model, committed_surface, resource};

/// Items the generated code refers to. Not part of the public API.
#[doc(hidden)]
pub mod __private {
    pub use automerge::{ObjId, ObjType, Patch, ROOT, ReadDoc, ScalarValue, Value};
    pub use serde;
    pub use serde_json;
    pub use tilo_actions::{
        ActionArgs, ActionKind, ActionParam, ActionRegistry, ActionSpec, ChangeKind, OperationKind,
        PROMPTED_PARAM, ParamType, Resource, ResourceViewSpec, RiskLevel,
    };

    pub use crate::apply_cache::forget_all;

    /// A record to copy, if its type can be copied: `#[resource]` asks for
    /// `(&&Snapshot(record)).snapshot()`, which method resolution answers
    /// with [`SnapshotByClone`] for a `Clone` record and with
    /// [`SnapshotUnavailable`] otherwise.
    pub struct Snapshot<'a, T>(pub &'a T);

    /// A copy of a `Clone` record.
    pub trait SnapshotByClone<T> {
        fn snapshot(&self) -> Option<T>;
    }

    impl<T: Clone> SnapshotByClone<T> for &Snapshot<'_, T> {
        fn snapshot(&self) -> Option<T> {
            Some(self.0.clone())
        }
    }

    /// No copy of a record that cannot be cloned.
    pub trait SnapshotUnavailable<T> {
        fn snapshot(&self) -> Option<T>;
    }

    impl<T> SnapshotUnavailable<T> for Snapshot<'_, T> {
        fn snapshot(&self) -> Option<T> {
            None
        }
    }

    /// `T`'s value of a field `key` the document does not hold, located at
    /// it: one function per field type, not a closure per field.
    ///
    /// # Errors
    /// Why `T` has no such value.
    use crate::model::LocatedAt as _;

    pub fn missing_at<T: crate::Model>(key: &str) -> Result<T, crate::ModelError> {
        T::missing().located(key)
    }

    /// The field `key` of a record's JSON form, read as a `T` (or its
    /// missing value), located at it.
    ///
    /// # Errors
    /// Why the value is not a `T`.
    pub fn from_json_at<T: crate::Model>(
        fields: &serde_json::Map<String, serde_json::Value>,
        key: &str,
    ) -> Result<T, crate::ModelError> {
        match fields.get(key) {
            Some(value) => T::from_json(value),
            None => T::missing(),
        }
        .located(key)
    }

    /// The field edit `op` at `rest` inside the field `key` of `obj`,
    /// located at it.
    ///
    /// # Errors
    /// Why the edit does not fit, located at the field.
    pub fn edit_at<T: crate::Model>(
        doc: &mut crate::Doc,
        obj: &ObjId,
        key: &str,
        rest: &[crate::Segment],
        op: &crate::EditOp,
        cx: &mut crate::ActionCx,
    ) -> Result<(), crate::ModelError> {
        T::edit(doc, crate::Slot::Key(obj, key), rest, op, cx).located(key)
    }

    /// [`Model::read_whole`](crate::Model::read_whole) of the field `key`,
    /// located at it.
    ///
    /// # Errors
    /// Why the value does not read whole.
    pub fn read_whole_at<T: crate::Model>(
        value: &mut T,
        doc: &crate::Doc,
        key: &str,
    ) -> Result<(), crate::ModelError> {
        value.read_whole(doc).located(key)
    }

    /// What `#[resource]` declares of one verb, besides how it is built.
    #[derive(Clone, Copy)]
    pub struct VerbDecl {
        pub id: &'static str,
        pub label: &'static str,
        pub description: &'static str,
        pub kind: ActionKind,
        pub field: Option<&'static str>,
        pub destructive: bool,
        pub answering: bool,
        pub collects: Option<&'static str>,
        pub item_field: Option<&'static str>,
        pub writes: &'static [&'static str],
        pub shape: crate::Shape,
        pub params: fn() -> Vec<ActionParam>,
    }

    /// The declaration of the verb `verb`, built by `build`: one function
    /// per record type, not a builder chain per verb.
    pub fn declare<R: crate::Resource>(
        verb: VerbDecl,
        build: fn(&ActionArgs, i64) -> Result<R::Action, String>,
    ) -> crate::ActionDecl<R> {
        let mut decl = crate::ActionDecl::new(verb.id, verb.label, (verb.params)(), build)
            .of_kind(verb.kind)
            .writing(crate::footprint::declared(
                verb.shape,
                verb.item_field,
                verb.writes,
            ));
        // What the description says beyond its first line, the label.
        let description = verb
            .description
            .strip_prefix(verb.label)
            .map_or(verb.description, |rest| {
                rest.trim_start_matches('.').trim_start()
            });
        if !description.is_empty() {
            decl = decl.describe(description);
        }
        if let Some(field) = verb.field {
            decl = decl.below_field(field);
        }
        if verb.destructive {
            decl = decl.destructive();
        }
        if verb.answering {
            decl = decl.answering();
        }
        if let Some(prompt) = verb.collects {
            decl = decl.collecting_content(prompt);
        }
        decl
    }

    /// Where a declared parameter names a place in the record.
    #[derive(Clone, Copy)]
    pub enum PlaceIn {
        /// It is no place.
        Nowhere,
        /// Anywhere in the record.
        Record,
        /// Among the items of this field.
        Field(&'static str),
    }

    /// A verb's declared parameter, built by one function for every verb:
    /// its schema is its type's, described, with the example when given.
    #[must_use]
    pub fn declared_param(
        name: &'static str,
        ty: ParamType,
        description: &'static str,
        schema: fn() -> serde_json::Value,
        optional: bool,
        place: PlaceIn,
        example: Option<fn() -> serde_json::Value>,
    ) -> ActionParam {
        let param = if optional {
            ActionParam::optional_owned(name, ty, description)
        } else {
            ActionParam::required_owned(name, ty, description)
        }
        // The parameter carries its description; its schema does not say
        // it again.
        .with_schema(param_schema(schema(), "", example.map(|example| example())));
        match place {
            PlaceIn::Nowhere => param,
            PlaceIn::Record => param.as_place(),
            PlaceIn::Field(field) => param.as_place_in(field),
        }
    }

    /// A parameter's schema: its type's, described, with an example.
    #[must_use]
    pub fn param_schema(
        schema: serde_json::Value,
        description: &str,
        example: Option<serde_json::Value>,
    ) -> serde_json::Value {
        let mut schema = crate::schema::describe(schema, description);
        if let (Some(example), serde_json::Value::Object(fields)) = (example, &mut schema) {
            fields.insert("examples".into(), serde_json::Value::Array(vec![example]));
        }
        schema
    }

    /// `Some(T::default())` when `T` is `Default`, else `None`, decided
    /// where `T` is concrete: `(&DefaultOf::<T>::new()).default_value()`
    /// with [`HasDefault`] and [`NoDefault`] in scope. A skipped field reads
    /// as its containing type's default this way.
    pub struct DefaultOf<T>(::core::marker::PhantomData<T>);

    impl<T> DefaultOf<T> {
        #[must_use]
        pub const fn new() -> Self {
            Self(::core::marker::PhantomData)
        }
    }

    impl<T> Default for DefaultOf<T> {
        fn default() -> Self {
            Self::new()
        }
    }

    /// See [`DefaultOf`].
    pub trait HasDefault<T> {
        fn default_value(&self) -> Option<T>;
    }

    impl<T: Default> HasDefault<T> for DefaultOf<T> {
        fn default_value(&self) -> Option<T> {
            Some(T::default())
        }
    }

    /// See [`DefaultOf`].
    pub trait NoDefault<T> {
        fn default_value(&self) -> Option<T>;
    }

    impl<T> NoDefault<T> for &DefaultOf<T> {
        fn default_value(&self) -> Option<T> {
            None
        }
    }

    /// Run `change` on the item of type `T` at `path` of the record `R`,
    /// reading only that item, then write what changed as field edits at
    /// its path, each checked: an item method applied.
    ///
    /// # Errors
    /// There is no such item, it does not read as `T`, the method refused,
    /// or an edit did not apply.
    pub fn apply_item<R, T>(
        doc: &mut crate::Doc,
        cx: &mut crate::ActionCx,
        path: &[crate::Segment],
        change: impl FnOnce(&mut T, &mut crate::ActionCx) -> Result<(), crate::Refusal>,
    ) -> Result<(), crate::Refusal>
    where
        R: crate::Fields,
        T: crate::Model,
    {
        let refused = |error: crate::ModelError| crate::Refusal::new(error.to_string());
        let obj = crate::Path::<R, T>::from_segments(path.to_vec())
            .object(doc)
            .ok_or_else(|| crate::Refusal::new("no such item"))?;
        let map = automerge::Value::Object(automerge::ObjType::Map);
        let before = T::load(doc, &map, &obj).map_err(refused)?;
        let mut after = T::load(doc, &map, &obj).map_err(refused)?;
        change(&mut after, cx)?;
        crate::Model::check(&after)?;
        let mut edits: Vec<crate::Edit<R>> = Vec::new();
        before.diff(&after, path, &mut edits);
        for edit in edits {
            edit.apply(doc, cx).map_err(refused)?;
        }
        Ok(())
    }

    /// The first non-empty line of the text `value` carries: one part of a
    /// label.
    pub fn first_line<T: crate::Model>(value: &T) -> Option<String> {
        let mut text = Vec::new();
        value.collect_text(&mut text);
        first_line_of(text)
    }

    /// The parameter type of a `T` that may or may not describe its JSON
    /// form: `(&ParamOf::<T>::new()).param_type()` reads `T`'s schema when it
    /// implements [`JsonSchema`](crate::JsonSchema) (whatever it is called:
    /// an alias of `Uuid` is text), and is an object otherwise.
    pub struct ParamOf<T: ?Sized>(std::marker::PhantomData<fn() -> T>);

    impl<T: ?Sized> ParamOf<T> {
        #[must_use]
        pub const fn new() -> Self {
            Self(std::marker::PhantomData)
        }
    }

    impl<T: ?Sized> Default for ParamOf<T> {
        fn default() -> Self {
            Self::new()
        }
    }

    pub trait DescribedParam {
        fn param_type(&self) -> ParamType;
    }

    impl<T: crate::JsonSchema + ?Sized> DescribedParam for ParamOf<T> {
        fn param_type(&self) -> ParamType {
            param_type_of(&T::json_schema())
        }
    }

    pub trait UndescribedParam {
        fn param_type(&self) -> ParamType;
    }

    impl<T: ?Sized> UndescribedParam for &ParamOf<T> {
        fn param_type(&self) -> ParamType {
            ParamType::Object
        }
    }

    /// The type of a parameter whose Rust type the macros cannot read
    /// (an app's own enum or struct): what its schema says it is written as.
    /// A unit enum is text, a struct an object; a value written in several
    /// shapes is an object.
    #[must_use]
    pub fn param_type_of(schema: &serde_json::Value) -> ParamType {
        fn of(schema: &serde_json::Value) -> Option<ParamType> {
            // A nullable type: what it is when present.
            let kind = match schema.get("type") {
                Some(serde_json::Value::Array(kinds)) => kinds
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .find(|kind| *kind != "null"),
                Some(kind) => kind.as_str(),
                None => None,
            };
            if let Some(kind) = kind {
                return Some(match kind {
                    "string" => ParamType::Text,
                    "integer" => ParamType::Integer,
                    "number" => ParamType::Number,
                    "boolean" => ParamType::Boolean,
                    "array" => ParamType::Array,
                    _ => ParamType::Object,
                });
            }
            if let Some(values) = schema.get("enum").and_then(serde_json::Value::as_array) {
                return values
                    .iter()
                    .all(serde_json::Value::is_string)
                    .then_some(ParamType::Text);
            }
            let alternatives = schema
                .get("oneOf")
                .or_else(|| schema.get("anyOf"))
                .and_then(serde_json::Value::as_array)?;
            let mut kinds = alternatives
                .iter()
                .filter(|alternative| alternative.get("type") != Some(&serde_json::json!("null")))
                .map(of);
            let first = kinds.next()??;
            kinds.all(|kind| kind == Some(first)).then_some(first)
        }
        of(schema).unwrap_or(ParamType::Object)
    }

    /// The parameters a `#[param(flatten)]` struct's schema lists: each
    /// property by name, typed and described by its own schema, required as
    /// the schema says.
    #[must_use]
    pub fn flat_params(schema: &serde_json::Value) -> Vec<ActionParam> {
        let required: Vec<&str> = schema
            .get("required")
            .and_then(serde_json::Value::as_array)
            .map(|names| names.iter().filter_map(serde_json::Value::as_str).collect())
            .unwrap_or_default();
        let Some(properties) = schema
            .get("properties")
            .and_then(serde_json::Value::as_object)
        else {
            return Vec::new();
        };
        properties
            .iter()
            .map(|(name, property)| {
                let description = property
                    .get("description")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or(name)
                    .to_owned();
                let ty = param_type_of(property);
                let param = if required.contains(&name.as_str()) {
                    ActionParam::required_owned(name.clone(), ty, description)
                } else {
                    ActionParam::optional_owned(name.clone(), ty, description)
                };
                // The parameter carries the description; its schema does not
                // say it again.
                let mut schema = property.clone();
                if let Some(schema) = schema.as_object_mut() {
                    schema.remove("description");
                }
                param.with_schema(schema)
            })
            .collect()
    }

    /// `value` in its JSON form, written as bytes and read back: the
    /// byte serializer every recorded action already has serves, rather
    /// than one more per type for `serde_json::to_value`.
    ///
    /// # Errors
    /// Why `value` does not serialize.
    #[inline(never)]
    pub fn json_via_bytes<T: serde::Serialize + ?Sized>(
        value: &T,
    ) -> Result<serde_json::Value, serde_json::Error> {
        let bytes = serde_json::to_vec(value)?;
        value_of_bytes(&bytes)
    }

    #[inline(never)]
    fn value_of_bytes(bytes: &[u8]) -> Result<serde_json::Value, serde_json::Error> {
        serde_json::from_slice(bytes)
    }

    /// The action `id` with `arguments`, stamped `now_ms`, decoded as a
    /// recorded action is (`{"op": {"action", "args"}, "at"}` from bytes):
    /// one decoder per resource serves building and replaying.
    ///
    /// # Errors
    /// Why the arguments do not decode as the action's parameters.
    pub fn build_stamped<O: serde::de::DeserializeOwned>(
        id: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
        now_ms: i64,
    ) -> Result<crate::Stamped<O>, String> {
        let operation = serde_json::json!({ "action": id, "args": arguments });
        O::deserialize(operation)
            .map(|operation| crate::Stamped { operation, now_ms })
            .map_err(|error| error.to_string())
    }

    pub use crate::resource::default_search;

    /// The arguments of a verb's JSON form: an object (`null` for none).
    ///
    /// # Errors
    /// Why `value` is no arguments object.
    #[inline(never)]
    pub fn args_object(
        value: serde_json::Value,
    ) -> Result<serde_json::Map<String, serde_json::Value>, serde_json::Error> {
        use serde::de::Error as _;
        match value {
            serde_json::Value::Object(args) => Ok(args),
            serde_json::Value::Null => Ok(serde_json::Map::new()),
            other => Err(serde_json::Error::invalid_type(
                serde::de::Unexpected::Other(&other.to_string()),
                &"the arguments as an object",
            )),
        }
    }

    /// The argument `name` taken from `args`, decoded as a `T`; an absent
    /// one is `None` when `optional`, else missing.
    ///
    /// # Errors
    /// Why the argument is missing or not a `T`.
    pub fn arg<T: serde::de::DeserializeOwned>(
        args: &mut serde_json::Map<String, serde_json::Value>,
        name: &'static str,
        optional: bool,
    ) -> Result<T, serde_json::Error> {
        use serde::de::Error as _;
        match args.remove(name) {
            Some(value) => T::deserialize(value),
            None if optional => T::deserialize(serde_json::Value::Null),
            None => Err(serde_json::Error::missing_field(name)),
        }
    }

    /// The action name and arguments of an action's JSON form
    /// (`{"action": .., "args": ..}`, the arguments `null` when absent).
    ///
    /// # Errors
    /// Why `value` is not an action.
    #[inline(never)]
    pub fn tagged(
        value: serde_json::Value,
    ) -> Result<(String, serde_json::Value), serde_json::Error> {
        use serde::de::Error as _;
        let serde_json::Value::Object(mut fields) = value else {
            return Err(serde_json::Error::custom("an action is an object"));
        };
        let Some(serde_json::Value::String(action)) = fields.remove("action") else {
            return Err(serde_json::Error::missing_field("action"));
        };
        Ok((
            action,
            fields.remove("args").unwrap_or(serde_json::Value::Null),
        ))
    }

    /// The JSON form's schema of a record of `shape`: each field's under its
    /// key, read from the shape so it is built by one function for every
    /// record type.
    #[must_use]
    pub fn record_value_schema(shape: crate::Shape) -> serde_json::Value {
        let properties: serde_json::Map<String, serde_json::Value> = shape
            .fields()
            .iter()
            .map(|field| (field.key.to_owned(), (field.schema)()))
            .collect();
        serde_json::json!({"type": "object", "properties": properties})
    }

    /// The label an item is offered under: its `#[model(label)]`, else its
    /// first text, else its id.
    pub fn item_label<T: crate::Model>(id: &crate::Id, item: &T) -> String {
        if let Some(label) = item.label()
            && !label.is_empty()
        {
            return label;
        }
        let mut text = Vec::new();
        item.collect_text(&mut text);
        text_or_id(text, id)
    }

    /// The first line of `text`, else the id.
    #[inline(never)]
    fn text_or_id(text: Vec<String>, id: &crate::Id) -> String {
        first_line_of(text).unwrap_or_else(|| id.to_string())
    }

    /// The first non-empty line among `text`.
    #[inline(never)]
    fn first_line_of(text: Vec<String>) -> Option<String> {
        text.into_iter()
            .map(|line| line.lines().next().unwrap_or_default().trim().to_owned())
            .find(|line| !line.is_empty())
    }

    /// A `#[model(check)]` function's result as a refusal.
    ///
    /// # Errors
    /// Returns the check's refusal.
    pub fn check_outcome<O: crate::Outcome>(outcome: O) -> Result<(), crate::Refusal> {
        outcome.into_result()
    }
}

/// Explicit bounded observations of real resource publication on this thread.
#[cfg(all(feature = "test-support", not(target_arch = "wasm32")))]
pub mod profiling;
