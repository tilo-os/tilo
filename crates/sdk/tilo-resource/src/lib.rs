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
//! A type with its own layout and merge semantics — a rich-text document, a
//! sheet — implements [`Resource`] by hand: [`read`](Resource::read) its
//! state from a document, name its replayable [`Action`](Resource::Action)
//! type, and optionally [`declarations`](Resource::declarations) for named,
//! typed palette entries. The SDK ships those types (`RichText`, plain text,
//! `Grid`); an app derives its record with `#[derive(Model)]` and
//! `#[resource]` and holds them as fields, never implementing `Resource`
//! itself (docs/architecture/resources.md §8). Such a type gets exactly the
//! same services. Edits without a named declaration travel as the generic
//! [`APPLY`] action, so nothing a custom surface does has to bypass
//! permissions, history or undo. `APPLY` is for surfaces, agents and
//! scripts, never a palette row; each value's own
//! [`risk`](tilo_actions::Action::risk) decides whether it needs
//! confirmation, so a destructive typed edit is treated exactly as a
//! declared destructive action is: what Undo takes back runs at once and
//! offers Undo, and only what it cannot is confirmed.

// The derives name this crate by its path, here as in an app.
extern crate self as tilo_resource;

pub mod action;
mod apply_cache;
pub mod batch;
pub mod child;
pub mod children;
mod chunk_map;
pub mod collection;
pub mod content;
mod cow;
#[cfg(feature = "chrono")]
mod dates;
mod search_preview;
pub use search_preview::SearchPreview;
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
mod map;
pub mod model;
pub mod order;
pub mod ordered;
pub mod path;
pub mod reactions;
pub mod reader;
pub mod replica;
pub mod resource;
pub mod schema;
pub mod shape;
pub mod share;
pub mod sort;
pub mod storage;
pub mod surface;
pub mod table;
pub mod text;
pub mod unreadable;
mod upsert;
mod value;
pub mod vocabulary;

pub use action::{
    ActionCx, ActionKey, ActionKind, ActionRef, Binds, Invoke, Mapped, NoParams, Operation,
    Outcome, Params, Refusal, Reverting, Stamped, Undoing, fresh_uuid, minted_ids, reverting_type,
    take_minted,
};
/// A change a document reports, as an `answer` hook reads what its action did.
pub use automerge::Patch;
pub use child::{child_resource_id, publish_child};
pub use children::{ChildList, ChildRef, Children, Trashed};
pub use collection::{Collection, Id, Membership, Quarantined};
pub use delta::{Delta, ObjPath, Touch, Touched, Tracker};
pub use derived::{Bound, Derived};
pub use effect::{Answers, Effect, EffectDecl, EffectOffload, NoEffect};
pub use evolution::Unreadable;
pub use group::{ActionGroup, GROUP, GROUP_PARAM};
pub use keyed::{ById, Identified, ItemStorage, Items, Keyed};
pub use kind::{Kinds, ResourceKind};
pub use map::Map;
pub use model::{
    Atom, Doc, Fields, LocatedAt, MapSlot, Model, ModelError, Slot, VARIANT_KEY, edit_whole,
    load_slot, read_key, read_slots, refresh_key, take_slot, write_key, write_key_changed,
};
pub use ordered::{Arranged, Ordered};
pub use path::{At, Edit, EditOp, Operations, Path, Segment, Settable, outbox_settle_arguments};
pub use reactions::{
    Reaction, ReactionGroup, Reactions, Reactor, custom_emoji_name, reaction_emoji,
    valid_custom_emoji_name,
};
pub use reader::{Demand, Reader, heads_of};
pub use replica::Replica;
pub use resource::{
    APPLY, APPLY_PARAM, ActionDecl, BuildAction, EDIT_PARAM, FIELD_EDIT, HeadInputs, NoAction,
    Place, Resource, ScopeDecl, SearchBody, SearchEntry, SearchPiece, SearchRow, SearchRowShape,
    SearchText, SearchWeight, SharedRecord, bind_declaration, build_action, child_by_id,
    declaration_spec, default_search, encode, head_inputs, need_of, origin_now_ms, publish,
    publish_head, publish_head_of, reset_type_schemas, scope_contains, spec_kind,
    with_origin_clock,
};
pub use schema::JsonSchema;
pub use shape::{FieldShape, Shape, StoredText, Trust, absent_scalar, stored_text};
pub use storage::{Bytes, Counter, Grid, Outbox, OutboxEntry, Set, Settled};
pub use surface::Surface;
pub use text::{PlainText, TextEdit, TextSpliceError, char_len, splice_chars, text_of};
pub use tilo_action_policy::edits::Weaker;
pub use tilo_actions::Availability;
pub use tilo_resource_macros::{Effect, JsonSchema, Model, committed_surface, resource};
pub use unreadable::withhold_document_actions;
pub use vocabulary::{Built, Declaration, Vocabulary, vocabulary};

/// Items the generated code refers to. Not part of the public API.
#[doc(hidden)]
pub mod __private {
    pub use automerge::{ObjId, ObjType, Patch, ROOT, ReadDoc, ScalarValue, Value};
    pub use serde;
    pub use serde_json;
    pub use tilo_actions::{
        ActionArgs, ActionKind, ActionParam, ActionRegistry, ActionSpec, Audience, ChangeKind,
        HOST_RENDER_TAG, OperationKind, PROMPTED_PARAM, ParamType, Resource, ResourceViewSpec,
        RiskLevel,
    };

    pub use crate::apply_cache::forget_all;

    /// Whether `record` holds an item where the nested item verb `action`
    /// acts: what its children would offer it bound on.
    #[must_use]
    pub fn holds_nested_items(
        record: &crate::table::RecordDesc,
        state: &dyn std::any::Any,
        action: &crate::items::ItemAction,
    ) -> bool {
        let Some(field) = action.collection else {
            return false;
        };
        let mut path: Vec<&str> = action.within.to_vec();
        path.push(field);
        crate::table::holds_nested_items(record, state, &path)
    }

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

    /// How a verb's change is undone.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum VerbUndo {
        /// By reverting the document: the change is all there is.
        Revert,
        /// By its compensating verb (`#[action(undo = ...)]`): it also
        /// queued an outside consequence, which only that takes back.
        Compensation,
    }

    /// What a verb answers its caller.
    #[derive(Clone, Copy, PartialEq, Eq)]
    pub enum VerbAnswer {
        /// Only the items it created.
        Created,
        /// What it made (`#[action(answers)]`, [`crate::Resource::answer`]).
        Made,
        /// What the outside work it queued came back with, once a host
        /// performed it.
        Queued,
    }

    /// What `#[resource]` declares of one verb, besides how it is built.
    #[derive(Clone, Copy)]
    #[allow(
        clippy::struct_excessive_bools,
        reason = "one independent `#[action(...)]` flag each, as the macro reads them"
    )]
    pub struct VerbDecl {
        pub id: &'static str,
        pub label: &'static str,
        pub description: &'static str,
        pub kind: ActionKind,
        /// Whether one invocation's kind is decided by its arguments
        /// (`#[action(local = path)]`); `kind` is then the least it is.
        pub by_arguments: bool,
        pub field: Option<&'static str>,
        pub destructive: bool,
        /// It also loses what Undo cannot bring back
        /// (`#[action(irreversible)]`).
        pub irreversible: bool,
        /// A change about the record rather than to it
        /// (`#[action(housekeeping)]`).
        pub housekeeping: bool,
        pub answer: VerbAnswer,
        /// It records a message someone received (`#[action(incoming_message)]`).
        pub incoming_message: bool,
        /// How its change is undone.
        pub undo: VerbUndo,
        /// Who listings offer it to (`#[action(audience = "agents")]`).
        pub audience: tilo_actions::Audience,
        pub collects: Option<&'static str>,
        /// The parameter the content it collects arrives as.
        pub content: Option<&'static str>,
        pub item_field: Option<&'static str>,
        pub writes: &'static [&'static str],
        pub shape: crate::Shape,
        pub params: &'static [ParamDecl],
    }

    /// What `#[resource]` declares of one parameter of a verb: static data,
    /// made into the published parameter by one function for every verb.
    #[derive(Clone, Copy)]
    pub struct ParamDecl {
        pub name: &'static str,
        pub ty: ParamTypeOf,
        pub description: &'static str,
        /// The schema of the parameter's type.
        pub schema: fn() -> serde_json::Value,
        pub optional: bool,
        pub place: PlaceIn,
        pub example: Option<fn() -> serde_json::Value>,
        /// Its type's fields are the verb's parameters, each its own.
        pub flat: bool,
        /// It takes content another action produced, by its hash, beside
        /// the verb's other parameters.
        pub content: bool,
    }

    /// A declared parameter's type.
    #[derive(Clone, Copy)]
    pub enum ParamTypeOf {
        /// Known from the Rust type.
        Known(ParamType),
        /// Read off the schema of the (present) value's type.
        Schema(fn() -> serde_json::Value),
    }

    /// The published parameters of `params`.
    #[must_use]
    pub fn params_of(params: &[ParamDecl]) -> Vec<ActionParam> {
        let mut out = Vec::with_capacity(params.len());
        for param in params {
            if param.flat {
                out.extend(flat_params(&(param.schema)()));
                continue;
            }
            let ty = match param.ty {
                ParamTypeOf::Known(ty) => ty,
                ParamTypeOf::Schema(schema) => param_type_of(&schema()),
            };
            let declared = declared_param(
                param.name,
                ty,
                param.description,
                param.schema,
                param.optional,
                param.place,
                param.example,
            );
            out.push(if param.content {
                declared.as_content()
            } else {
                declared
            });
        }
        out
    }

    /// The declaration of the verb `verb`, built by `build`.
    pub fn declare<R: crate::Resource>(
        verb: VerbDecl,
        build: fn(&ActionArgs, i64) -> Result<R::Action, String>,
    ) -> crate::ActionDecl<R> {
        let builds: crate::vocabulary::Builds =
            std::sync::Arc::new(move |args, now_ms| match build(args, now_ms) {
                Ok(action) => Ok(crate::Built::of::<R>(action)),
                Err(reason) => Err(reason),
            });
        crate::ActionDecl::declared(verb_declaration(crate::vocabulary::<R>(), verb, builds))
    }

    /// The declaration of the verb `verb` of the record `R`, whose action
    /// is its operation `O` decoded from the arguments: one build for
    /// every verb of the record.
    pub fn declare_verb<R, O>(verb: VerbDecl) -> crate::ActionDecl<R>
    where
        R: crate::Resource<Action = crate::Stamped<O>>,
        O: serde::de::DeserializeOwned,
    {
        let (id, content) = (verb.id, verb.content);
        let builds: crate::vocabulary::Builds = std::sync::Arc::new(move |args, now_ms| {
            let arguments = verb_arguments(args, content);
            match build_stamped::<O>(id, &arguments, now_ms) {
                Ok(action) => Ok(crate::Built::of::<R>(action)),
                Err(reason) => Err(reason),
            }
        });
        crate::ActionDecl::declared(verb_declaration(crate::vocabulary::<R>(), verb, builds))
    }

    /// The arguments of a verb invoked with `args`, the content it collects
    /// (if any) under its parameter `content`.
    fn verb_arguments(
        args: &ActionArgs,
        content: Option<&'static str>,
    ) -> serde_json::Map<String, serde_json::Value> {
        let mut arguments = args.to_json();
        if let Some(content) = content
            && let Some(hash) = arguments.remove(PROMPTED_PARAM)
        {
            arguments.insert(String::from(content), hash);
        }
        arguments
    }

    /// The declaration of `verb` of the resource `vocabulary`, built by
    /// `build`.
    fn verb_declaration(
        vocabulary: &crate::Vocabulary,
        verb: VerbDecl,
        build: crate::vocabulary::Builds,
    ) -> crate::Declaration {
        let mut decl = crate::Declaration::new(
            vocabulary,
            verb.id,
            verb.label.to_owned(),
            params_of(verb.params),
            build,
        );
        decl.kind = verb.kind;
        decl.by_arguments = verb.by_arguments;
        decl.writes = crate::footprint::declared(verb.shape, verb.item_field, verb.writes);
        // What the description says beyond its first line, the label.
        let description = verb
            .description
            .strip_prefix(verb.label)
            .map_or(verb.description, |rest| {
                rest.trim_start_matches('.').trim_start()
            });
        if !description.is_empty() {
            decl.description = Some(description.to_owned());
        }
        decl.field = verb.field;
        decl.destructive = verb.destructive;
        if verb.irreversible {
            decl.tags.push(tilo_actions::IRREVERSIBLE_TAG.to_owned());
        }
        if verb.housekeeping {
            decl.tags.push(tilo_actions::HOUSEKEEPING_TAG.to_owned());
        }
        if verb.incoming_message {
            decl.tags
                .push(tilo_actions::INCOMING_MESSAGE_TAG.to_owned());
        }
        if let Some(tag) = verb.audience.tag() {
            decl.tags.push(tag.to_owned());
        }
        if verb.undo == VerbUndo::Compensation {
            decl.tags
                .push(tilo_actions::COMPENSATED_UNDO_TAG.to_owned());
        }
        match verb.answer {
            VerbAnswer::Created => {}
            VerbAnswer::Made => {
                decl.result = Some(tilo_actions::ResultSpec::Json {
                    schema: serde_json::json!({"type": "object"}),
                });
            }
            // One result, or a list for several entries; nothing while the
            // work is still queued.
            VerbAnswer::Queued => {
                decl.result = Some(tilo_actions::ResultSpec::Json {
                    schema: serde_json::json!({"type": ["object", "array", "null"]}),
                });
            }
        }
        if let Some(prompt) = verb.collects {
            decl.content = Some(prompt.to_owned());
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
        let mut edits: Vec<crate::Edit<()>> = Vec::new();
        before.diff(&after, path, &mut edits);
        for edit in edits {
            R::edit_fields(doc, &ROOT, edit.path(), edit.op(), cx).map_err(refused)?;
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

    /// The JSON value in `bytes`, parsed by code compiled once: a model
    /// type's reader that parses its stored text calls this and then reads
    /// itself out of the value, rather than compiling `serde_json`'s whole
    /// text parser again for every app it is instantiated in.
    #[inline(never)]
    pub(crate) fn value_of_bytes(bytes: &[u8]) -> Result<serde_json::Value, serde_json::Error> {
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

    /// The byte argument `name` taken from `args`, decoded by `decode`
    /// ([`crate::content::bytes`]): inline, or by a content reference.
    ///
    /// # Errors
    /// Why the argument is missing or not bytes.
    pub fn bytes_arg<T>(
        args: &mut serde_json::Map<String, serde_json::Value>,
        name: &'static str,
        decode: fn(serde_json::Value) -> Result<T, serde_json::Error>,
    ) -> Result<T, serde_json::Error> {
        use serde::de::Error as _;
        args.remove(name)
            .map_or_else(|| Err(serde_json::Error::missing_field(name)), decode)
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
    pub(crate) fn first_line_of(text: Vec<String>) -> Option<String> {
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

pub mod profiling;
