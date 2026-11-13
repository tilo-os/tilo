//! Resource types by kind, for a host that links no resource type's code at
//! the call site.
//!
//! Every record names the kind of resource it holds (`RecordKind`, the
//! `record_kind` of an app's definition, [`Resource::KIND`]). A host that
//! works with whatever a workspace holds — an agent, a service, a script
//! runner — looks the kind up here and gets everything the typed [`Resource`]
//! offers, with the type erased: read a document's state, publish the tree
//! the palette and agents see, register the decoders replay needs, and build a
//! typed action from its JSON form. Each type registers once, as
//! [`ResourceKind::of`]; an app framework adds who hosts it
//! ([`ResourceKind::hosted_by`]) and what an open resource is called
//! ([`ResourceKind::named`]).
//!
//! ```
//! use tilo_resource::{Kinds, Model, ResourceKind, resource};
//!
//! #[derive(Model, Default)]
//! pub struct Note { pub body: String }
//!
//! #[resource(kind = "example.note")]
//! impl Note {
//!     /// Edit the note.
//!     #[action]
//!     pub fn edit(&mut self, text: String) { self.body = text; }
//! }
//!
//! fn main() {
//!     let kinds = Kinds::new().with(ResourceKind::of::<Note>());
//!     let note = kinds.get("example.note").unwrap();
//!     let doc = tilo_resource::Doc::new();
//!     let tree = note.publish("example.note", &doc).unwrap();
//!     assert!(tree.actions.iter().any(|action| action.id == "edit"));
//! }
//! ```

use std::any::Any;

use tilo_actions::{ActionRegistry, EncodedAction};

use crate::model::{Doc, ModelError};
use crate::reader::Demand;
use crate::resource::{self, Place, Resource, SearchEntry};

/// One resource type with its type erased: what [`Resource`] offers, reached
/// by the kind a record names. See the [module docs](self).
#[derive(Clone, Copy)]
pub struct ResourceKind {
    kind: &'static str,
    label: &'static str,
    action_type: &'static str,
    app: Option<(&'static str, &'static str)>,
    version: u32,
    read: fn(&Doc) -> Result<Box<dyn Any>, ModelError>,
    /// [`Self::read`] with what the resource's search entries quote read too
    /// ([`Resource::search_demand`]).
    read_for_search: fn(&Doc) -> Result<Box<dyn Any>, ModelError>,
    name: fn(&dyn Any) -> Option<String>,
    publish: fn(&str, &str, &dyn Any) -> Option<tilo_actions::ResourceSpec>,
    search: fn(&dyn Any) -> Option<Vec<SearchEntry>>,
    to_json: fn(&dyn Any) -> Option<serde_json::Value>,
    child: fn(&dyn Any, &Doc, &str) -> Option<tilo_actions::ResourceSpec>,
    content: fn(&dyn Any, &Doc, Option<&str>) -> Result<serde_json::Value, String>,
    reads_item: fn(&str) -> bool,
    register: fn(&mut ActionRegistry),
    initialize: fn(&mut Doc) -> Result<(), ModelError>,
    migrate: fn(&mut Doc) -> Result<bool, ModelError>,
    needs_migrate: fn(&Doc) -> bool,
    encode: fn(serde_json::Value) -> Result<EncodedAction, String>,
    lands: fn(&Doc, &EncodedAction) -> Option<Place>,
}

impl std::fmt::Debug for ResourceKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceKind")
            .field("kind", &self.kind)
            .field("app", &self.app)
            .finish_non_exhaustive()
    }
}

/// The app a resource kind names when the type declares no app of its own: a
/// test's, a tool's, a host's.
pub const DEFAULT_APP: &str = "so.tilo.app";

// What a kind declared from a type's name alone answers: it publishes no
// state of its own, since the record it names is published where it lives.

fn unavailable(_doc: &Doc) -> Result<Box<dyn Any>, ModelError> {
    Err(ModelError::Refused(
        "a kind declared by name alone holds no record".to_owned(),
    ))
}

fn no_name(_state: &dyn Any) -> Option<String> {
    None
}

fn no_publish(_id: &str, _label: &str, _state: &dyn Any) -> Option<tilo_actions::ResourceSpec> {
    None
}

fn no_search(_state: &dyn Any) -> Option<Vec<SearchEntry>> {
    None
}

fn no_to_json(_state: &dyn Any) -> Option<serde_json::Value> {
    None
}

fn no_child(_state: &dyn Any, _doc: &Doc, _id: &str) -> Option<tilo_actions::ResourceSpec> {
    None
}

fn no_content(
    _state: &dyn Any,
    _doc: &Doc,
    _item: Option<&str>,
) -> Result<serde_json::Value, String> {
    Err("a kind declared by name alone holds no record".to_owned())
}

fn no_item(_id: &str) -> bool {
    false
}

fn no_register(_registry: &mut ActionRegistry) {}

fn no_initialize(_doc: &mut Doc) -> Result<(), ModelError> {
    Err(ModelError::Refused(
        "a kind declared by name alone initialises no record".to_owned(),
    ))
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "a kind's migration answers as a type's migration does, whether or not it changes anything"
)]
fn no_migrate(_doc: &mut Doc) -> Result<bool, ModelError> {
    Ok(false)
}

fn no_needs_migrate(_doc: &Doc) -> bool {
    false
}

fn no_encode(_action: serde_json::Value) -> Result<EncodedAction, String> {
    Err("a kind declared by name alone has no action type of its own".to_owned())
}

fn no_lands(_doc: &Doc, _action: &EncodedAction) -> Option<Place> {
    None
}

/// `a == b` as a `const fn`, which `str` comparison is not.
#[allow(
    clippy::indexing_slicing,
    reason = "both sides are walked to the shorter one's length"
)]
const fn app_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i = i.saturating_add(1);
    }
    true
}

/// [`Resource::label`], for a kind declared from a type's name alone.
#[allow(
    clippy::unnecessary_wraps,
    reason = "a kind names its records through a `Resource` method, whatever its state"
)]
fn naming<R: Resource>(_state: &dyn Any) -> Option<String> {
    Some(R::label())
}

fn read<R: Resource>(doc: &Doc) -> Result<Box<dyn Any>, ModelError> {
    R::read(doc).map(|state| Box::new(state) as Box<dyn Any>)
}

/// [`read`] as a reader of the resource reads it when it reads nothing of it yet: its state, and
/// the parts [`Resource::load`] reads for a demand that asks for none. What search derives says
/// what a record holds, so it reads the record the way a window's reader does.
fn read_for_search<R: Resource>(doc: &Doc) -> Result<Box<dyn Any>, ModelError> {
    let mut state = R::read(doc)?;
    R::load(&mut state, doc, &Demand::none())?;
    Ok(Box::new(state) as Box<dyn Any>)
}

fn resource_label<R: Resource>(state: &dyn Any) -> Option<String> {
    state.downcast_ref::<R::State>().map(|_| R::label())
}

fn publish<R: Resource>(
    id: &str,
    label: &str,
    state: &dyn Any,
) -> Option<tilo_actions::ResourceSpec> {
    state
        .downcast_ref::<R::State>()
        .map(|state| resource::publish::<R>(id, label, state))
}

fn child<R: Resource>(state: &dyn Any, doc: &Doc, id: &str) -> Option<tilo_actions::ResourceSpec> {
    resource::child_by_id::<R>(state.downcast_ref::<R::State>()?, doc, id)
}

fn search<R: Resource>(state: &dyn Any) -> Option<Vec<SearchEntry>> {
    state.downcast_ref::<R::State>().map(R::search)
}

/// A record's state as JSON, through its own model table
/// ([`Model::to_json`](crate::Model::to_json)): what a view of it is offered.
fn to_json<R: Resource>(state: &dyn Any) -> Option<serde_json::Value> {
    let table = R::RECORD_TABLE?;
    state
        .downcast_ref::<R::State>()
        .map(|state| crate::table::Run(table).to_json(state))
}

/// [`crate::read::content`] of a state this type reads.
fn content<R: Resource>(
    state: &dyn Any,
    doc: &Doc,
    item: Option<&str>,
) -> Result<serde_json::Value, String> {
    let state = state
        .downcast_ref::<R::State>()
        .ok_or_else(|| format!("{} state is another type", R::KIND))?;
    crate::read::content::<R>(state, doc, item)
}

fn reads_item<R: Resource>(item: &str) -> bool {
    R::RECORD_TABLE.is_some_and(|table| crate::table::reads_item(table, R::KIND, item))
}

fn lands<R: Resource>(doc: &Doc, action: &EncodedAction) -> Option<Place> {
    let action: R::Action = if action.type_id == R::ACTION_TYPE {
        tilo_actions::decode_json(&action.payload).ok()?
    } else if action.type_id == crate::group::group_type::<R>() {
        tilo_actions::decode_payload::<crate::group::ActionGroup<R::Action>>(
            &action.type_id,
            &action.payload,
        )
        .ok()?
        .actions
        .pop()?
    } else {
        return None;
    };
    R::lands(&R::read(doc).ok()?, &action)
}

fn encode<R: Resource>(action: serde_json::Value) -> Result<EncodedAction, String> {
    encode_in(crate::vocabulary::<R>(), action)
}

/// [`ResourceKind::encode`] of the resource `vocabulary`.
fn encode_in(
    vocabulary: &crate::Vocabulary,
    action: serde_json::Value,
) -> Result<EncodedAction, String> {
    let mut arguments = serde_json::Map::new();
    arguments.insert(resource::APPLY_PARAM.to_owned(), action);
    let action = resource::build_action_of(
        vocabulary,
        resource::APPLY,
        &arguments,
        resource::origin_now_ms(),
    )?;
    action.encode().map_err(|error| error.to_string())
}

impl ResourceKind {
    /// `R`, by its kind, published by the app it declares
    /// ([`Resource::APP_ID`]) unless the caller hosts it for another
    /// ([`Self::hosted_by`]).
    #[must_use]
    pub const fn of<R: Resource>() -> Self {
        Self {
            kind: R::KIND,
            label: R::LABEL,
            action_type: R::ACTION_TYPE,
            app: Some((R::APP_ID, R::LABEL)),
            version: R::VERSION,
            read: read::<R>,
            read_for_search: read_for_search::<R>,
            name: resource_label::<R>,
            publish: publish::<R>,
            search: search::<R>,
            to_json: to_json::<R>,
            child: child::<R>,
            content: content::<R>,
            reads_item: reads_item::<R>,
            register: crate::group::register::<R>,
            initialize: R::initialize,
            migrate: crate::evolution::migrate::<R>,
            needs_migrate: crate::evolution::needs_migrate::<R>,
            encode: encode::<R>,
            lands: lands::<R>,
        }
    }

    /// The app that hosts records of this kind: its id, which permissions
    /// name, and its title.
    #[must_use]
    pub const fn hosted_by(mut self, app_id: &'static str, title: &'static str) -> Self {
        self.app = Some((app_id, title));
        self
    }

    /// A kind for an effect vocabulary, which publishes no record of its own
    /// and reaches state through the record an effect addresses: `kind` is the
    /// operation its effects travel under and `app` the app that offers them.
    /// What an effect's [`ActionRef`](crate::ActionRef) carries.
    #[must_use]
    pub const fn of_effect<V: crate::Effect>(
        kind: &'static str,
        label: &'static str,
        app: &'static str,
    ) -> Self {
        Self {
            kind,
            label,
            action_type: kind,
            app: Some((app, label)),
            version: 0,
            read: unavailable,
            read_for_search: unavailable,
            name: no_name,
            publish: no_publish,
            search: no_search,
            to_json: no_to_json,
            child: no_child,
            content: no_content,
            reads_item: no_item,
            register: no_register,
            initialize: no_initialize,
            migrate: no_migrate,
            needs_migrate: no_needs_migrate,
            encode: no_encode,
            lands: no_lands,
        }
    }

    /// `R`'s kind as the app `app` declares it: hosted by it, naming its
    /// records as the resource itself does, and built from the type's name
    /// alone — so a typed [`ActionRef`](crate::ActionRef) carries it as a
    /// constant and [`ActionRef::key`](crate::ActionRef::key) names the app,
    /// kind and action a control is bound against. Nothing is registered,
    /// so it costs an app no initialisation.
    #[must_use]
    pub const fn declaring<R>(app: &'static str, title: &'static str) -> Self
    where
        R: Resource,
    {
        Self::of::<R>().hosted_by(app, title).named(naming::<R>)
    }

    /// [`Self::declaring`] for a kind built where `R` may still be defined:
    /// the `kind`, `label` and `app` the declaration wrote. A resource
    /// publishes its own kind from [`Self::of`], so a control bound by key
    /// reads the same kind either way.
    #[must_use]
    pub const fn declared<R>(kind: &'static str, label: &'static str, app: &'static str) -> Self
    where
        R: Resource,
    {
        let mut declared = Self::of::<R>().hosted_by(app, label);
        declared.kind = kind;
        declared
    }

    /// The app this kind names its actions by: the one hosting it, else
    /// [`DEFAULT_APP`] for a resource type that declares no app of its own.
    #[must_use]
    pub fn declaring_app(&self) -> &'static str {
        self.app.map_or(DEFAULT_APP, |(id, _)| id)
    }

    /// Whether `app` is the app `declared` names, or the type declared none
    /// and `app` is the [default](DEFAULT_APP). What a generated
    /// [`ActionRef`](crate::ActionRef) asks at compile time before it carries
    /// `app`, so a constant naming another app than its type's fails where it
    /// is written rather than at the control that binds it.
    #[must_use]
    pub const fn declares(app: &str, declared: &str) -> bool {
        if app_eq(declared, DEFAULT_APP) {
            app_eq(app, DEFAULT_APP)
        } else {
            app_eq(app, declared)
        }
    }

    /// What an open resource of this kind is called, read from its state
    /// (a document's first heading). `name` answers `None` for a state of
    /// another type. Defaults to the resource's [`Resource::LABEL`].
    #[must_use]
    pub const fn named(mut self, name: fn(&dyn Any) -> Option<String>) -> Self {
        self.name = name;
        self
    }

    /// The kind records of this type name, e.g. `"notes.notebook"`.
    pub fn kind(&self) -> &'static str {
        self.kind
    }

    /// The human name of one resource of this kind.
    pub fn label(&self) -> &'static str {
        self.label
    }

    /// The registry type id its actions replay under.
    pub fn action_type(&self) -> &'static str {
        self.action_type
    }

    /// Where `action` of this kind, just applied to `doc`, leaves the
    /// participant who took it ([`Resource::lands`]); `None` for an action
    /// of another type or one that lands nowhere in particular. A group
    /// lands where its last member does.
    pub fn lands(&self, doc: &Doc, action: &EncodedAction) -> Option<Place> {
        (self.lands)(doc, action)
    }

    /// The id of the app hosting it, when one registered it.
    pub fn app_id(&self) -> Option<&'static str> {
        self.app.map(|(id, _)| id)
    }

    /// The title of the app hosting it, when one registered it.
    pub fn app_title(&self) -> Option<&'static str> {
        self.app.map(|(_, title)| title)
    }

    /// The storage version the resource reads; an older record is brought
    /// up to it when it opens ([`crate::evolution`]).
    #[must_use]
    pub fn version(&self) -> u32 {
        self.version
    }

    /// The state views render, read from `doc`; downcast it to the
    /// resource's `State` where the type is known.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold this resource.
    pub fn read(&self, doc: &Doc) -> Result<Box<dyn Any>, ModelError> {
        (self.read)(doc)
    }

    /// [`Self::read`] as the state type the caller expects.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold this resource
    /// or its state is not an `S`.
    pub fn read_as<S: 'static>(&self, doc: &Doc) -> Result<S, ModelError> {
        self.read(doc)?
            .downcast::<S>()
            .map(|state| *state)
            .map_err(|_| ModelError::invalid(format!("{} state is another type", self.kind)))
    }

    /// Whether `doc` holds a resource of this kind: its own `read` accepts
    /// it.
    pub fn holds(&self, doc: &Doc) -> bool {
        self.read(doc).is_ok()
    }

    /// Whether `doc` holds a resource of this kind, or was kept before the
    /// app changed its storage and is brought up to it when it opens: a
    /// workspace still keeps the older document after a replica migrated its
    /// own.
    pub fn holds_or_older(&self, doc: &Doc) -> bool {
        crate::evolution::stored_version(doc) < self.version || self.holds(doc)
    }

    /// What the open resource is called, from a state [`Self::read`]
    /// returned.
    pub fn name(&self, state: &dyn Any) -> String {
        (self.name)(state).unwrap_or_else(|| self.label.to_owned())
    }

    /// The tree published for the resource `doc` holds, under `id`: every
    /// declared and state-bound action, the generic `apply` and `group`
    /// actions, its views and its nested resources — what
    /// [`publish`](crate::publish) builds for the typed resource.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold this resource.
    pub fn publish(&self, id: &str, doc: &Doc) -> Result<tilo_actions::ResourceSpec, ModelError> {
        let state = self.read(doc)?;
        self.publish_state(id, state.as_ref())
    }

    /// Read one existing lazy child from the publishing resource's own resolver.
    /// This returns metadata only and never creates or invokes the child.
    pub fn child(
        &self,
        doc: &Doc,
        id: &str,
    ) -> Result<Option<tilo_actions::ResourceSpec>, ModelError> {
        let state = self.read(doc)?;
        Ok((self.child)(state.as_ref(), doc, id))
    }

    /// What `doc` holds, as JSON a caller can act on: the resource itself, or
    /// with `item`, the part of it that id names — the reading a document
    /// host offers a view (`docs/architecture/apps.md` §2). Read through the
    /// type's own [`Resource::read_content`] where it answers reads, else
    /// from the record's model tables, so a host that links no model type
    /// still reads exactly what the app's process did.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold this resource,
    /// or a refusal naming why the part cannot be read.
    pub fn content(&self, doc: &Doc, item: Option<&str>) -> Result<serde_json::Value, ModelError> {
        let state = self.read(doc)?;
        (self.content)(state.as_ref(), doc, item).map_err(ModelError::Refused)
    }

    /// Whether [`Self::content`] answers with `item`: the record publishes a
    /// read of that part.
    #[must_use]
    pub fn reads_item(&self, item: &str) -> bool {
        (self.reads_item)(item)
    }

    /// [`Self::publish`] from a state [`Self::read`] returned.
    ///
    /// # Errors
    /// Returns [`ModelError`] when `state` is not this resource's state.
    pub fn publish_state(
        &self,
        id: &str,
        state: &dyn Any,
    ) -> Result<tilo_actions::ResourceSpec, ModelError> {
        let name = self.name(state);
        (self.publish)(id, &name, state)
            .ok_or_else(|| ModelError::invalid(format!("{} state is another type", self.kind)))
    }

    /// What the state a [`Self::read`] returned is, as JSON: the reading a
    /// view of the resource is offered, where no better adapter answers.
    ///
    /// # Errors
    /// Returns [`ModelError`] when `state` is not this resource's state.
    pub fn to_json(&self, state: &dyn Any) -> Result<serde_json::Value, ModelError> {
        (self.to_json)(state)
            .ok_or_else(|| ModelError::invalid(format!("{} state is another type", self.kind)))
    }

    /// Entries for the system search index.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold this resource.
    pub fn search(&self, doc: &Doc) -> Result<Vec<SearchEntry>, ModelError> {
        let state = (self.read_for_search)(doc)?;
        (self.search)(state.as_ref())
            .ok_or_else(|| ModelError::invalid(format!("{} state is another type", self.kind)))
    }

    /// Register what a replica of this kind replays: the resource's actions
    /// and groups of them.
    pub fn register(&self, registry: &mut ActionRegistry) {
        (self.register)(registry);
    }

    /// Write a brand-new record's genesis ([`Resource::initialize`]).
    pub fn initialize(&self, doc: &mut Doc) -> Result<(), ModelError> {
        (self.initialize)(doc)
    }

    /// Bring `doc` up to this kind's version with its own migration
    /// ([`crate::evolution::migrate`]), and to this build's version key.
    /// Returns whether it changed anything.
    ///
    /// # Errors
    /// Returns the migration's error; `doc` is left as it was.
    pub fn migrate(&self, doc: &mut Doc) -> Result<bool, ModelError> {
        (self.migrate)(doc)
    }

    /// Whether [`Self::migrate`] would change `doc`: it is older than this
    /// kind, or still keeps its version under a key this one replaced.
    #[must_use]
    pub fn needs_migrate(&self, doc: &Doc) -> bool {
        (self.needs_migrate)(doc)
    }

    /// One typed action from its JSON form, encoded for replay — read as
    /// the generic `apply` action reads it, so a value naming a declared
    /// action is checked against that declaration.
    pub fn encode(&self, action: serde_json::Value) -> Result<EncodedAction, String> {
        (self.encode)(action)
    }
}

/// The resource types a host knows, by kind. See the [module docs](self).
#[derive(Clone, Debug, Default)]
pub struct Kinds {
    kinds: Vec<ResourceKind>,
}

impl Kinds {
    /// No types.
    #[must_use]
    pub const fn new() -> Self {
        Self { kinds: Vec::new() }
    }

    /// These and `kind`. A kind registered again replaces the earlier entry.
    #[must_use]
    pub fn with(mut self, kind: ResourceKind) -> Self {
        self.insert(kind);
        self
    }

    /// Add `kind`, replacing an earlier entry of the same kind.
    pub fn insert(&mut self, kind: ResourceKind) {
        match self.kinds.iter_mut().find(|known| known.kind == kind.kind) {
            Some(known) => *known = kind,
            None => self.kinds.push(kind),
        }
    }

    /// The type records of `kind` hold.
    pub fn get(&self, kind: &str) -> Option<&ResourceKind> {
        self.kinds.iter().find(|known| known.kind == kind)
    }

    /// The type the app `app_id` hosts.
    pub fn hosted_by(&self, app_id: &str) -> Option<&ResourceKind> {
        self.kinds
            .iter()
            .find(|known| known.app_id() == Some(app_id))
    }

    /// Every type, in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &ResourceKind> {
        self.kinds.iter()
    }

    /// The decoders of every type, in one registry: what a host replaying
    /// any of their records needs.
    pub fn registry(&self) -> ActionRegistry {
        let mut registry = ActionRegistry::new();
        for kind in &self.kinds {
            kind.register(&mut registry);
        }
        registry
    }
}
