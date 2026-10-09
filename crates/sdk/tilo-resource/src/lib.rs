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
mod low_mutation;
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
    Outcome, Params, Refusal, Stamped, Undoing, fresh_uuid, minted_ids,
};
/// A change a document reports, as an `answer` hook reads what its action did.
pub use automerge::Patch;
pub use child::{child_resource_id, publish_child};
pub use collection::{Collection, Id};
pub use delta::{Delta, ObjPath, Touch, Touched, Tracker};
pub use derived::{Bound, Derived};
pub use effect::{Answers, Effect, EffectDecl, NoEffect};
pub use evolution::Unreadable;
pub use group::{ActionGroup, GROUP, GROUP_PARAM};
pub use keyed::{Identified, Keyed};
pub use kind::{Kinds, ResourceKind};
pub use low_mutation::{APPLY_LOW, GROUP_LOW};
pub use model::{
    Atom, Doc, Fields, Json, Model, ModelError, Slot, edit_whole, read_key, refresh_key, write_key,
};
pub use ordered::Ordered;
pub use path::{Edit, EditOp, Operations, Path, Segment, Settable};
pub use reader::{Demand, Reader, heads_of};
pub use replica::Replica;
pub use resource::{
    APPLY, APPLY_PARAM, ActionDecl, BuildAction, EDIT_PARAM, FIELD_EDIT, NoAction, Place, Resource,
    ScopeDecl, SearchBody, SearchEntry, SearchPiece, SearchRow, SearchRowShape, SearchText,
    SearchWeight, SharedRecord, bind_declaration, build_action, declaration_spec, default_search,
    encode, origin_now_ms, publish, reset_type_schemas, scope_contains, spec_kind,
    with_origin_clock,
};
pub use schema::JsonSchema;
pub use shape::{FieldShape, Shape};
pub use storage::{Bytes, Counter, Grid, Outbox, OutboxEntry, Set, Settled};
pub use surface::Surface;
pub use text::{PlainText, TextEdit};
pub use tilo_actions::{Availability, PermissionRequirement};
pub use tilo_resource_macros::{Effect, JsonSchema, Model, resource};

/// Items the generated code refers to. Not part of the public API.
#[doc(hidden)]
pub mod __private {
    pub use automerge::{ObjId, ObjType, Patch, ROOT, ReadDoc, ScalarValue, Value};
    pub use serde;
    pub use serde_json;
    pub use tilo_actions::{
        ActionArgs, ActionParam, ActionSpec, ChangeKind, OperationKind, PROMPTED_PARAM, ParamType,
        PermissionRequirement, Resource, ResourceViewSpec, RiskLevel,
    };

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
                param.with_schema(property.clone())
            })
            .collect()
    }

    pub use crate::resource::default_search;

    /// The label an item is offered under: its `#[model(label)]`, else its
    /// first text, else its id.
    pub fn item_label<T: crate::Model>(id: &crate::Id, item: &T) -> String {
        if let Some(label) = item.label().filter(|label| !label.is_empty()) {
            return label;
        }
        let mut text = Vec::new();
        item.collect_text(&mut text);
        text.into_iter()
            .map(|line| line.lines().next().unwrap_or_default().trim().to_owned())
            .find(|line| !line.is_empty())
            .unwrap_or_else(|| id.to_string())
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
