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

#![cfg_attr(
    test,
    allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::indexing_slicing,
        clippy::panic,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

// The derives name this crate by its path, here as in an app.
extern crate self as tilo_resource;

pub mod action;
pub mod batch;
pub mod child;
pub mod collection;
pub mod content;
pub mod delta;
pub mod derived;
pub mod diff;
pub mod effect;
pub mod group;
pub mod history;
pub mod items;
pub mod kind;
mod low_mutation;
pub mod model;
pub mod order;
pub mod path;
pub mod reader;
pub mod replica;
pub mod resource;
pub mod schema;
pub mod shape;
pub mod storage;
pub mod text;

pub use action::{
    ActionCx, ActionKey, ActionKind, ActionRef, Binds, Invoke, Mapped, NoParams, Operation,
    Outcome, Params, Refusal, Stamped,
};
pub use child::{child_resource_id, publish_child};
pub use collection::{Collection, Id};
pub use delta::{Delta, ObjPath, Touch, Touched, Tracker};
pub use derived::{Bound, Derived};
pub use effect::{Answers, Effect, EffectDecl, NoEffect};
pub use group::{ActionGroup, GROUP, GROUP_PARAM};
pub use kind::{Kinds, ResourceKind};
pub use low_mutation::{APPLY_LOW, GROUP_LOW};
pub use model::{
    Atom, Doc, Fields, Json, Model, ModelError, Slot, edit_whole, read_key, refresh_key, write_key,
};
pub use path::{Edit, EditOp, Operations, Path, Segment, Settable};
pub use reader::{Demand, Reader, heads_of};
pub use replica::Replica;
pub use resource::{
    APPLY, APPLY_PARAM, ActionDecl, BuildAction, EDIT_PARAM, FIELD_EDIT, NoAction, Place, Resource,
    ScopeDecl, SearchBody, SearchEntry, SearchPiece, SearchRow, SearchRowShape, SearchText,
    SearchWeight, SharedRecord, action_label, bind_declaration, build_action, declaration_spec,
    encode, origin_now_ms, publish, reset_type_schemas, scope_contains, spec_kind,
};
pub use schema::JsonSchema;
pub use shape::{FieldShape, Shape};
pub use storage::{Bytes, Counter, Grid, Outbox, OutboxEntry, Set, Settled};
pub use text::{PlainText, TextEdit};
pub use tilo_actions::{Availability, PermissionRequirement};
pub use tilo_resource_macros::{Effect, JsonSchema, Model, resource};

/// Items the generated code refers to. Not part of the public API.
#[doc(hidden)]
pub mod __private {
    pub use automerge::{ObjId, ObjType, ROOT, ReadDoc, ScalarValue, Value};
    pub use serde;
    pub use serde_json;
    pub use tilo_actions::{
        ActionArgs, ActionParam, ChangeKind, OperationKind, ParamType, PermissionRequirement,
        Resource, ResourceViewSpec, RiskLevel,
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

    /// The default search entries of a record: one per labelled item,
    /// anchored at its item resource, or else all of its text in one entry.
    pub fn default_search<R: crate::Resource + crate::Fields>(
        label: &str,
        record: &R,
    ) -> Vec<crate::SearchEntry> {
        let items = crate::Fields::item_search(record);
        if !items.is_empty() {
            return items
                .into_iter()
                .map(|item| {
                    crate::SearchEntry::new(item.label, item.text)
                        .anchored(crate::items::item_resource_id::<R>(item.field, &item.id))
                })
                .collect();
        }
        let mut text = Vec::new();
        record.collect_text(&mut text);
        if text.is_empty() {
            return Vec::new();
        }
        vec![crate::SearchEntry {
            anchor: None,
            title: text.first().cloned().unwrap_or_else(|| label.to_owned()),
            text: text.join("\n"),
            ..Default::default()
        }]
    }

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
