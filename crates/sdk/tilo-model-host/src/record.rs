//! One record of one resource kind, kept by the document host: its document,
//! the tree it offers, the readings it answers, and the actions it admits.
//!
//! See the [crate docs](crate) for what this is and why an app's process does
//! not hold it.

use serde_json::{Map, Value as Json};
use tilo_actions::{
    ActionAddress, ActionContext, ActionInvoker, ActionOutput, ActionRegistry, DoorError,
    ResourceSpec,
};
use tilo_resource::{Doc, Kinds, ModelError, ResourceKind};

/// Who an action is recorded as: the session decides it
/// (`docs/architecture/apps.md` §4), never the app.
///
/// A view submits an action over its connection; the session says whether the
/// person was acting in the view (a gesture token), or the app acting on its
/// own. The invoker that comes with the action carries that decision, and this
/// record keeps it: what the change is attributed to, whose grant is checked,
/// and the actor bytes the change is committed under.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Invoker {
    /// The invoker as the door recorded it: the person, or an agent acting for
    /// them, or the app.
    pub invoker: ActionInvoker,
    /// The actor the change is committed under. `None` commits it under the
    /// document's own actor, for a record the person has not signed.
    pub writer: Option<Vec<u8>>,
}

impl Invoker {
    /// `invoker`'s change, committed under the actor `writer` names.
    #[must_use]
    pub fn new(invoker: ActionInvoker, writer: Option<Vec<u8>>) -> Self {
        Self { invoker, writer }
    }

    /// `invoker`'s change (the session's own decision of who acted: the person
    /// in a gesture, an agent, or the app), committed under the actor `writer`.
    #[must_use]
    pub fn of(invoker: ActionInvoker, writer: Option<Vec<u8>>) -> Self {
        Self { invoker, writer }
    }

    /// The person seated at this device acting for themselves, committed as
    /// `person`.
    #[must_use]
    pub fn person(person: impl Into<String>) -> Self {
        let person = person.into();
        Self {
            writer: Some(person.clone().into_bytes()),
            invoker: ActionInvoker::actor(person),
        }
    }
}

/// Why a record could not be opened, read or changed.
#[derive(Debug, thiserror::Error)]
pub enum RecordError {
    /// The kind names no resource type this host links.
    #[error("no model host runs a {0} record")]
    NoModel(String),
    /// The document does not read as the model: why.
    #[error("{0}")]
    Model(#[from] ModelError),
    /// The door refused the action.
    #[error(transparent)]
    Door(#[from] DoorError),
    /// The record is kept on disk and the store refused it.
    #[error("{0}")]
    Store(String),
    /// The document refused a change.
    #[error("{0}")]
    Document(String),
}

impl From<automerge::AutomergeError> for RecordError {
    fn from(error: automerge::AutomergeError) -> Self {
        Self::Document(error.to_string())
    }
}

impl From<tilo_actions::ActionError> for RecordError {
    fn from(error: tilo_actions::ActionError) -> Self {
        Self::Door(DoorError::Failed(error.to_string()))
    }
}

/// What [`Record::open`] answers: the record, and whether opening it wrote its
/// genesis (a brand-new record) or read one that was already there — so a
/// caller knows whether anything of the person's was touched.
#[derive(Debug)]
pub struct Opened {
    pub record: Record,
    pub created: bool,
}

/// One record of one resource kind, kept here.
///
/// A record is opened once per app and workspace. What it holds is read
/// through the model's own code ([`ResourceKind`]), so the same record reads
/// as the app's views read it, and every change is the model's own verb over
/// the door, so a change this host commits is one the app's process could have
/// made — and, exactly as that process did, it is signed by whoever keeps the
/// record, which is now this host.
pub struct Record {
    /// The kind the record holds, from the model the host links.
    kind: ResourceKind,
    /// The resource address this record is published at: readings and actions
    /// name it.
    address: String,
    /// The document, the one copy of what the record holds.
    doc: Doc,
    /// What a change this record commits replays through.
    registry: ActionRegistry,
    /// The app that keeps it, for the tree it offers.
    app: Option<(&'static str, &'static str)>,
    /// The tree, and the heads it was published at: what a view is offered is
    /// recomputed only when the document moved. Refreshed through a shared
    /// reference, so a service publishes what its records offer by `&self`.
    published: std::cell::RefCell<Option<(Vec<automerge::ChangeHash>, ResourceSpec)>>,
    /// Where it is kept, when it is on disk ([`Record::open_kept`]).
    #[cfg(feature = "store")]
    kept: Option<crate::store::Kept>,
    /// Whether anything changed since a reader last asked.
    moved: bool,
}

impl std::fmt::Debug for Record {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Record")
            .field("kind", &self.kind.kind())
            .field("address", &self.address)
            .finish_non_exhaustive()
    }
}

impl Record {
    /// Open the record of `kind` that the model the host links keeps at
    /// `address`, over `doc`, in memory.
    ///
    /// A document that does not read as the kind is brought up with the
    /// model's own migration ([`ResourceKind::migrate`]) before it is opened,
    /// never rewritten by this host: what it held keeps its ids and nothing is
    /// deleted (the data invariants). A document of no record yet is given its
    /// genesis by the model (`created`).
    ///
    /// # Errors
    /// The kind names no model this host links, or the document does not read
    /// as the model even after its migration.
    pub fn open(
        kinds: &Kinds,
        kind: &str,
        address: &str,
        mut doc: Doc,
    ) -> Result<Opened, RecordError> {
        let model = *kinds
            .get(kind)
            .ok_or_else(|| RecordError::NoModel(kind.to_owned()))?;
        let created = Self::settle(model, &mut doc)?;
        Ok(Opened {
            record: Self::plain(model, address, doc),
            created,
        })
    }

    /// Open a new record of `kind` at `address`: its genesis written by the
    /// model, in memory.
    ///
    /// # Errors
    /// The kind names no model this host links, or the model's genesis fails.
    pub fn genesis(kinds: &Kinds, kind: &str, address: &str) -> Result<Record, RecordError> {
        let model = *kinds
            .get(kind)
            .ok_or_else(|| RecordError::NoModel(kind.to_owned()))?;
        let mut doc = Doc::new();
        model.initialize(&mut doc)?;
        doc.commit();
        Ok(Self::plain(model, address, doc))
    }

    /// Open the record of `kind` at `address` from where it is kept, in
    /// `workspace` for `app`: the same place and key a record the app's own
    /// process kept was written to, so a record a person already has opens
    /// unchanged. A record the store holds none of is kept there as the
    /// model's genesis.
    ///
    /// # Errors
    /// As [`Self::open`], and the store's own refusal.
    #[cfg(feature = "store")]
    pub fn open_kept(
        kinds: &Kinds,
        kind: &str,
        address: &str,
        app: &str,
        workspace: &str,
        record: &str,
    ) -> Result<Opened, RecordError> {
        let mut kept =
            crate::store::Kept::open(app, workspace, record).map_err(RecordError::Store)?;
        let doc = kept.read().map_err(RecordError::Store)?.unwrap_or_default();
        let mut opened = Self::open(kinds, kind, address, doc)?;
        opened.record.kept = Some(kept);
        opened.record.persist()?;
        Ok(opened)
    }

    /// `doc` brought to `model`'s version by the model's own genesis or
    /// migration, and whether that made its genesis.
    fn settle(model: ResourceKind, doc: &mut Doc) -> Result<bool, RecordError> {
        let created = !model.holds_or_older(doc);
        if created {
            model.initialize(doc)?;
            doc.commit();
            return Ok(true);
        }
        if model.needs_migrate(doc) {
            model.migrate(doc)?;
            doc.commit();
        }
        // A record this host cannot read even after its migration is refused
        // here, rather than shown as current state.
        model.read(doc)?;
        Ok(false)
    }

    fn plain(model: ResourceKind, address: &str, doc: Doc) -> Self {
        Self {
            kind: model,
            address: address.to_owned(),
            registry: registry_of(model),
            app: model.app_id().zip(model.app_title()),
            doc,
            published: std::cell::RefCell::new(None),
            #[cfg(feature = "store")]
            kept: None,
            moved: true,
        }
    }

    /// The kind the record holds.
    #[must_use]
    pub fn kind(&self) -> &str {
        self.kind.kind()
    }

    /// The resource address readings and actions name it at.
    #[must_use]
    pub fn address(&self) -> &str {
        &self.address
    }

    /// The document, for a host that keeps or syncs it. What a view is offered
    /// never reaches it: a view sees [`Self::offers`] and [`Self::read`].
    #[must_use]
    pub fn document(&self) -> &Doc {
        &self.doc
    }

    /// Take in what another replica of the same record holds. The change is
    /// whoever made it: it is merged, not re-signed.
    ///
    /// # Errors
    /// The changes do not apply to the document.
    pub fn merge(&mut self, other: &mut Doc) -> Result<bool, RecordError> {
        use tilo_automerge::ApplyIncoming as _;
        let before = self.doc.get_heads();
        self.doc.merge_incoming(other)?;
        let changed = self.doc.get_heads() != before;
        if changed {
            self.doc.commit();
            self.changed();
        }
        Ok(changed)
    }

    /// What the record offers now: the resource's tree — its actions, the
    /// items it collects and what each offers — published by the model under
    /// the address it keeps. Recomputed only when the document moved.
    ///
    /// # Errors
    /// The document does not read as the model.
    pub fn offers(&self) -> Result<ResourceSpec, RecordError> {
        let heads = self.doc.committed_document().get_heads();
        if let Some((at, spec)) = &*self.published.borrow()
            && *at == heads
        {
            return Ok(spec.clone());
        }
        let mut spec = self.kind.publish(&self.address, &self.doc)?;
        if spec.app_id.is_none() {
            spec.app_id = self.app.map(|(id, _)| id.to_owned());
        }
        *self.published.borrow_mut() = Some((heads, spec.clone()));
        Ok(spec)
    }

    /// The record's own reading: what it holds, as typed data, and the ids of
    /// the parts it collects, each of which a view reads as its own item.
    ///
    /// # Errors
    /// The document does not read as the model.
    pub fn reading(&self) -> Result<Json, RecordError> {
        let spec = self.offers()?;
        let state = self.kind.read(&self.doc)?;
        let mut reading = self.kind.to_json(state.as_ref())?;
        let parts = parts_of(&spec);
        if let Some(object) = reading.as_object_mut()
            && !parts.is_empty()
        {
            object.insert("parts".to_owned(), Json::Array(parts));
        }
        Ok(reading)
    }

    /// The reading of `resource`, as a view is offered it: the record itself
    /// for [`Self::address`], or the item of its collection that `resource`
    /// names. `None` when `resource` is no part of this record — a view is
    /// offered readings, never a document, and a part it was not offered
    /// answers nothing.
    ///
    /// # Errors
    /// The record does not read as the model, or what it names is not a part
    /// of it.
    pub fn read(&self, resource: &str) -> Option<Result<Json, RecordError>> {
        if resource == self.address {
            return Some(self.reading());
        }
        self.kind.reads_item(resource).then(|| {
            self.kind
                .content(&self.doc, Some(resource))
                .map_err(RecordError::Model)
        })
    }

    /// Admit `action` of `resource` with `arguments` as `invoker`'s step: the
    /// model's verb over the door, checked and recorded with its history entry,
    /// committed by this host as the invoker's change.
    ///
    /// The app's process neither runs the verb nor signs the change: it submits
    /// the action, and this host runs the model and commits it.
    ///
    /// # Errors
    /// The door's refusal (no such action, bad arguments, unavailable, the
    /// verb's own refusal), or the document's write error.
    pub fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &Map<String, Json>,
        invoker: &Invoker,
    ) -> Result<ActionOutput, RecordError> {
        let tree = self.offers()?;
        let address = ActionAddress::new(resource, action);
        let known = if resource == self.address {
            tree
        } else {
            self.kind
                .child(&self.doc, resource)?
                .ok_or_else(|| RecordError::Door(DoorError::Unknown(address.clone())))?
        };
        let encoded =
            tilo_actions::door::encode_json(std::slice::from_ref(&known), &address, arguments)?;
        let context = ActionContext::for_resource(resource, action, action)
            .with_invoker(invoker.invoker.clone());
        self.commit(encoded, context, invoker.writer.as_deref())?;
        // What it made is published before it is answered, so a view's next
        // reading names it.
        *self.published.borrow_mut() = None;
        Ok(ActionOutput {
            value: None,
            resources: Vec::new(),
        })
    }

    /// Commit `encoded` as the change of `context`'s invoker, under the actor
    /// `writer`.
    fn commit(
        &mut self,
        encoded: tilo_actions::EncodedAction,
        context: ActionContext,
        writer: Option<&[u8]>,
    ) -> Result<tilo_actions::ActionEnvelope, RecordError> {
        let action = self.registry.decode(&encoded)?;
        let applied = tilo_actions::apply_local_with_context(
            &mut self.doc,
            action.as_ref(),
            encoded,
            writer,
            context,
        )?;
        self.doc.commit();
        self.changed();
        self.persist()?;
        Ok(applied)
    }

    /// Whether anything changed since a reader last asked, and clear it.
    pub fn take_moved(&mut self) -> bool {
        std::mem::take(&mut self.moved)
    }

    /// Note that the record moved: a view following it reads again, and the
    /// tree it offers is published again.
    fn changed(&mut self) {
        self.moved = true;
        *self.published.borrow_mut() = None;
    }

    /// Keep the document where it is kept, when it is on disk.
    #[cfg(feature = "store")]
    fn persist(&mut self) -> Result<(), RecordError> {
        let Some(kept) = &mut self.kept else {
            return Ok(());
        };
        kept.write(&mut self.doc).map_err(RecordError::Store)
    }

    /// Nothing to keep: a record in memory.
    #[cfg(not(feature = "store"))]
    #[allow(
        clippy::unnecessary_wraps,
        clippy::unused_self,
        clippy::needless_pass_by_ref_mut,
        reason = "a record on disk answers as one in memory does, whether or not there is anything to keep"
    )]
    fn persist(&mut self) -> Result<(), RecordError> {
        Ok(())
    }
}

/// The registry one kind's changes replay through.
fn registry_of(model: ResourceKind) -> ActionRegistry {
    let mut registry = ActionRegistry::new();
    model.register(&mut registry);
    registry
}

/// The addresses of the items `spec` publishes: each its own reading a view
/// asks for by name.
fn parts_of(spec: &ResourceSpec) -> Vec<Json> {
    fn walk(spec: &ResourceSpec, out: &mut Vec<Json>) {
        for child in &spec.children {
            out.push(Json::String(child.id.as_str().to_owned()));
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(spec, &mut out);
    out
}
