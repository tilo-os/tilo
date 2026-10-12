//! Resource types by kind, for a host that links no resource type's code at
//! the call site.
//!
//! Every record names the kind of resource it holds (`RecordKind`, the
//! `record_kind` of an app's definition, [`Resource::KIND`]). A host that
//! works with whatever a room holds — an agent, a service, a script runner —
//! looks the kind up here and gets everything the typed [`Resource`] offers,
//! with the type erased: read a document's state, publish the tree the
//! palette and agents see, register the decoders replay needs, and build a
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
//! #[resource(kind = "example.note", label = "Note")]
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
use crate::resource::{self, Place, Resource, SearchEntry};

/// One resource type with its type erased: what [`Resource`] offers, reached
/// by the kind a record names. See the [module docs](self).
#[derive(Clone, Copy)]
pub struct ResourceKind {
    kind: &'static str,
    label: &'static str,
    action_type: &'static str,
    app: Option<(&'static str, &'static str)>,
    read: fn(&Doc) -> Result<Box<dyn Any>, ModelError>,
    name: fn(&dyn Any) -> Option<String>,
    publish: fn(&str, &str, &dyn Any) -> Option<tilo_actions::Resource>,
    search: fn(&dyn Any) -> Option<Vec<SearchEntry>>,
    child: fn(&dyn Any, &Doc, &str) -> Option<tilo_actions::Resource>,
    register: fn(&mut ActionRegistry),
    initialize: fn(&mut Doc) -> Result<(), ModelError>,
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

fn read<R: Resource>(doc: &Doc) -> Result<Box<dyn Any>, ModelError> {
    R::read(doc).map(|state| Box::new(state) as Box<dyn Any>)
}

fn resource_label<R: Resource>(state: &dyn Any) -> Option<String> {
    state
        .downcast_ref::<R::State>()
        .map(|_| R::LABEL.to_owned())
}

fn publish<R: Resource>(id: &str, label: &str, state: &dyn Any) -> Option<tilo_actions::Resource> {
    state
        .downcast_ref::<R::State>()
        .map(|state| resource::publish::<R>(id, label, state))
}

fn child<R: Resource>(state: &dyn Any, doc: &Doc, id: &str) -> Option<tilo_actions::Resource> {
    resource::child_by_id::<R>(state.downcast_ref::<R::State>()?, doc, id)
}

fn search<R: Resource>(state: &dyn Any) -> Option<Vec<SearchEntry>> {
    state.downcast_ref::<R::State>().map(R::search)
}

fn lands<R: Resource>(doc: &Doc, action: &EncodedAction) -> Option<Place> {
    let action: R::Action = if action.type_id == R::ACTION_TYPE {
        tilo_actions::decode_json(&action.payload).ok()?
    } else if action.type_id == crate::group::group_type::<R>() {
        tilo_actions::decode_json::<crate::group::ActionGroup<R::Action>>(&action.payload)
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
    /// `R`, by its kind.
    #[must_use]
    pub const fn of<R: Resource>() -> Self {
        Self {
            kind: R::KIND,
            label: R::LABEL,
            action_type: R::ACTION_TYPE,
            app: None,
            read: read::<R>,
            name: resource_label::<R>,
            publish: publish::<R>,
            search: search::<R>,
            child: child::<R>,
            register: crate::group::register::<R>,
            initialize: R::initialize,
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

    /// [`Self::read`] of a saved document, as a peer sends it.
    ///
    /// # Errors
    /// Returns [`ModelError`] when `bytes` are no document, or one that does
    /// not hold this resource.
    pub fn read_saved(&self, bytes: &[u8]) -> Result<Box<dyn Any>, ModelError> {
        let doc = Doc::load(bytes).map_err(|error| ModelError::invalid(error.to_string()))?;
        self.read(&doc)
    }

    /// [`Self::holds`] for a saved document.
    pub fn holds_saved(&self, bytes: &[u8]) -> bool {
        self.read_saved(bytes).is_ok()
    }

    /// The heads of a saved document; `None` when `bytes` are no document.
    pub fn saved_heads(bytes: &[u8]) -> Option<Vec<automerge::ChangeHash>> {
        Doc::load(bytes).ok().map(|mut doc| doc.get_heads())
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
    pub fn publish(&self, id: &str, doc: &Doc) -> Result<tilo_actions::Resource, ModelError> {
        let state = self.read(doc)?;
        self.publish_state(id, state.as_ref())
    }

    /// Read one existing lazy child from the publishing resource's own resolver.
    /// This returns metadata only and never creates or invokes the child.
    ///
    /// # Errors
    /// Returns why this document does not hold the publishing resource.
    pub fn child(&self, doc: &Doc, id: &str) -> Result<Option<tilo_actions::Resource>, ModelError> {
        let state = self.read(doc)?;
        Ok((self.child)(state.as_ref(), doc, id))
    }

    /// [`Self::publish`] from a state [`Self::read`] returned.
    ///
    /// # Errors
    /// Returns [`ModelError`] when `state` is not this resource's state.
    pub fn publish_state(
        &self,
        id: &str,
        state: &dyn Any,
    ) -> Result<tilo_actions::Resource, ModelError> {
        let name = self.name(state);
        (self.publish)(id, &name, state)
            .ok_or_else(|| ModelError::invalid(format!("{} state is another type", self.kind)))
    }

    /// Entries for the system search index.
    ///
    /// # Errors
    /// Returns [`ModelError`] when the document does not hold this resource.
    pub fn search(&self, doc: &Doc) -> Result<Vec<SearchEntry>, ModelError> {
        let state = self.read(doc)?;
        (self.search)(state.as_ref())
            .ok_or_else(|| ModelError::invalid(format!("{} state is another type", self.kind)))
    }

    /// Register what a replica of this kind replays: the resource's actions
    /// and groups of them.
    pub fn register(&self, registry: &mut ActionRegistry) {
        (self.register)(registry);
    }

    /// Write a brand-new record's genesis ([`Resource::initialize`]).
    ///
    /// # Errors
    /// Returns the document's write error.
    pub fn initialize(&self, doc: &mut Doc) -> Result<(), ModelError> {
        (self.initialize)(doc)
    }

    /// One typed action from its JSON form, encoded for replay — read as
    /// the generic `apply` action reads it, so a value naming a declared
    /// action is checked against that declaration.
    ///
    /// # Errors
    /// Returns why the value is not an action of this resource.
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
