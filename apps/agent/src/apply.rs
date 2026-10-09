//! The agent's replicas, and how it edits them.
//!
//! Each document the agent holds is held in its app's own view
//! ([`AttachedView`]): the app's real component mounted over the record,
//! laid out and never drawn, as a window would be on a host without a
//! screen. The agent edits only through that view's door — the app's own
//! actions, built from their typed references, checked against the agent's
//! permissions, applied under the replica's own actor, attributed to the
//! agent acting for the person who asked, and recorded in history with the
//! task it is carrying out. Nothing here writes a document any other way.

use anyhow::{Context, Result};
use automerge::AutoCommit;
use tilo_actions::Resource;
use ui::collaboration::DocumentMutation;
use ui::framework::AttachedView;
use ui::permissions::PermissionContext;
use ui::record::RecordCheckout;

use crate::surface::AppContract;

pub struct Editor {
    view: AttachedView,
    /// The record as the view last left it, read without borrowing the
    /// view's store.
    doc: AutoCommit,
    /// Which app wrote this document.
    contract: &'static AppContract,
    /// Exact authenticated route that supplied this replica. It can differ
    /// from the application definition `contract` comes from.
    route_app_id: String,
    /// The room the record lives in.
    room: String,
    /// Who acts through the view.
    invoker: tilo_actions::ActionInvoker,
    permissions: PermissionContext,
}

/// The agent `author` as it acts on its own: an agent, for nobody yet.
#[must_use]
pub fn agent(author: &str) -> tilo_actions::ActionInvoker {
    tilo_actions::ActionInvoker::actor(author).with_kind(tilo_actions::ActorKind::Agent)
}

impl Editor {
    /// Which app wrote the document this holds.
    pub fn contract(&self) -> &'static AppContract {
        self.contract
    }

    /// Exact app route that supplied this replica.
    pub fn route_app_id(&self) -> &str {
        &self.route_app_id
    }

    /// The replica, to read.
    pub fn doc(&self) -> &AutoCommit {
        &self.doc
    }

    /// Set the task subsequent actions are part of — the person the agent
    /// acts for and the run they group into. The session clears it after
    /// the run so a later, unrelated instruction cannot inherit it.
    pub fn set_history_context(&mut self, context: Option<tilo_actions::ActionContext>) {
        self.view.set_context(context);
    }

    /// The replica's current heads.
    pub fn heads(&mut self) -> Vec<automerge::ChangeHash> {
        self.doc.get_heads()
    }

    /// The changes the replica holds that `heads` do not.
    pub fn changes_since(&mut self, heads: &[automerge::ChangeHash]) -> Vec<automerge::Change> {
        self.doc.get_changes(heads)
    }

    /// Stable identity of this replica's writer.
    pub fn actor_hex(&self) -> String {
        self.doc.get_actor().to_hex_string()
    }

    /// The record's version history.
    pub fn versions(&mut self) -> Vec<ui::tilo_resource::history::Version> {
        ui::tilo_resource::history::versions(&mut self.doc)
    }

    /// Record one instruction that changed several documents as one run in
    /// this document's history. Returns the changes to broadcast.
    pub fn record_run(
        &mut self,
        instruction: &str,
        issued_by: Option<&str>,
        decision: Option<&str>,
        steps: &[ui::RunStep],
    ) -> Option<Vec<u8>> {
        if !self
            .view
            .record_run(instruction, issued_by, decision, steps)
        {
            return None;
        }
        self.take()
    }

    /// Newest non-genesis revision, suitable as one end of a run step.
    pub fn latest_entry_id(&mut self) -> Option<ui::HistoryEntryId> {
        self.versions()
            .into_iter()
            .rev()
            .find(|version| !tilo_doc::model::is_schema_genesis(version))
            .map(|version| ui::history_source::entry_id_from_hash(&version.hash.to_string()))
    }

    /// The tree the app publishes for this document now. Rebuilt on every
    /// call, since an address only means something if it names what is
    /// there now.
    pub fn resources(&self) -> Vec<Resource> {
        crate::surface::tree(self.contract, &self.doc)
    }

    /// The replica the agent was invited to, seeded from a full snapshot a
    /// peer sent on `contract`'s route in `room`, written under the agent's
    /// own name.
    ///
    /// # Errors
    ///
    /// When `bytes` are not a document of `contract`'s kind, or its app's
    /// view would not open over it.
    pub fn from_snapshot(
        contract: &'static AppContract,
        room: &str,
        bytes: &[u8],
        author: &str,
        permissions: PermissionContext,
    ) -> Result<Self> {
        Self::from_snapshot_as(
            contract,
            contract.app_id(),
            room,
            bytes,
            agent(author),
            permissions,
        )
    }

    /// A *peer* document — another app's, in the same room — that arrived
    /// on `route_app_id`. Each record's view writes under its own record
    /// writer's actor, so two documents the agent holds never number two
    /// unrelated changes alike; the invoker its edits are attributed to is
    /// the agent either way.
    ///
    /// # Errors
    ///
    /// When `bytes` are not a document of `contract`'s kind.
    pub fn peer_from_snapshot_on_route(
        contract: &'static AppContract,
        route_app_id: &str,
        room: &str,
        bytes: &[u8],
        author: &str,
        permissions: PermissionContext,
    ) -> Result<Self> {
        Self::from_snapshot_as(
            contract,
            route_app_id,
            room,
            bytes,
            agent(author),
            permissions,
        )
    }

    fn from_snapshot_as(
        contract: &'static AppContract,
        route_app_id: &str,
        room: &str,
        bytes: &[u8],
        invoker: tilo_actions::ActionInvoker,
        permissions: PermissionContext,
    ) -> Result<Self> {
        let doc = AutoCommit::load(bytes)
            .with_context(|| format!("load {}'s document", contract.namespace))?;
        contract
            .kind
            .read(&doc)
            .map_err(|error| anyhow::anyhow!("{}'s document: {error}", contract.namespace))?;
        let view = open(contract, room, bytes, &invoker, &permissions)?;
        let mut editor = Self {
            view,
            doc,
            contract,
            route_app_id: route_app_id.to_string(),
            room: room.to_owned(),
            invoker,
            permissions,
        };
        editor.refresh();
        Ok(editor)
    }

    /// Read the record again from the view.
    fn refresh(&mut self) {
        if let Some(doc) = self.view.read(AutoCommit::clone) {
            self.doc = doc;
        }
    }

    /// What this replica did since the last call, as one incremental
    /// change to broadcast; `None` when it did nothing.
    fn take(&mut self) -> Option<Vec<u8>> {
        self.refresh();
        match self.view.take_changes()? {
            DocumentMutation::Snapshot(bytes) | DocumentMutation::Incremental(bytes) => {
                (!bytes.is_empty()).then_some(bytes)
            }
        }
    }

    /// Merge a full snapshot from a peer. Never replaces: both sides keep
    /// whatever the other had not seen.
    ///
    /// Returns whether the snapshot was taken. A room carries more than one
    /// document, and a subscriber that announced no record — or no app at
    /// all, which is cluster-scoped — is handed all of them, so a snapshot
    /// that is not this document is ordinary rather than an error.
    pub fn merge_full(&mut self, bytes: &[u8]) -> bool {
        let Ok(mut incoming) = AutoCommit::load(bytes) else {
            return false;
        };
        if !self.contract.kind.holds(&incoming) {
            return false;
        }
        let merged = self
            .view
            .merge(&DocumentMutation::Snapshot(bytes.to_vec()))
            .unwrap_or(false);
        self.refresh();
        merged || self.doc.get_heads() == incoming.get_heads()
    }

    /// Whether a peer's snapshot is a *different* document that is asking
    /// for something from us.
    ///
    /// Only consulted once a snapshot has failed to merge. Without a
    /// `--record` the agent cannot tell from the address which of a room's
    /// documents is the one it was invited to, and adopting the first to
    /// arrive picks whichever app answered first. An instruction addressed
    /// to us by name is the one signal in the content that settles it.
    pub fn is_addressed_to(bytes: &[u8], name: &str) -> bool {
        let Some(snapshot) = tilo_doc::model::snapshot_of(bytes) else {
            return false;
        };
        let blocks = crate::plan::blocks(&snapshot);
        crate::plan::find_addressed(&blocks, name).is_some()
    }

    /// Take `bytes` as the document, discarding what we hold. Only for the
    /// case above, and only before the agent has made an edit.
    ///
    /// # Errors
    ///
    /// When `bytes` are not a document of this replica's kind.
    pub fn replace_with(&mut self, bytes: &[u8]) -> Result<()> {
        let doc = AutoCommit::load(bytes).context("load a peer snapshot")?;
        self.contract
            .kind
            .read(&doc)
            .map_err(|error| anyhow::anyhow!("a peer snapshot: {error}"))?;
        self.view = open(
            self.contract,
            &self.room,
            bytes,
            &self.invoker,
            &self.permissions,
        )?;
        self.doc = doc;
        self.refresh();
        Ok(())
    }

    /// Apply a peer's incremental change set. Returns whether the replica
    /// moved — a delta belonging to one of the room's other documents
    /// refers to objects this one has never seen, and that is not this
    /// agent's business.
    pub fn merge_incremental(&mut self, bytes: &[u8]) -> bool {
        let before = self.doc.get_heads();
        let merged = self
            .view
            .merge(&DocumentMutation::Incremental(bytes.to_vec()))
            .unwrap_or(false);
        self.refresh();
        merged && self.doc.get_heads() != before
    }

    /// Perform `invoke` — an action of the app's resource `R`, built from
    /// its typed reference (`Chat::RECORD_WORK.with(..)`,
    /// `Invoke::edit(..)`) — through the app's view: permission-checked,
    /// attributed to the agent acting for the person who asked and recorded
    /// within the task. Returns the changes to broadcast, or `None` when
    /// nothing changed.
    ///
    /// # Errors
    ///
    /// The door's refusal — the agent may not do this, or it names
    /// something no longer in the document.
    pub fn invoke<R: ui::tilo_resource::Resource>(
        &mut self,
        invoke: &ui::tilo_resource::Invoke<R>,
    ) -> Result<Option<Vec<u8>>> {
        self.view
            .invoke_typed(invoke, false)
            .map_err(|error| anyhow::anyhow!("{} refused the edit: {error}", R::LABEL))?;
        Ok(self.take())
    }

    /// How many ancestor changes this replica is waiting for.
    ///
    /// Automerge queues a change whose dependencies it has not got, and
    /// nothing about reading the document afterwards says so: it
    /// materialises as the page *before* the gap. So a replica missing one
    /// change in the middle reads exactly like a document with nothing
    /// more in it — this is the one honest way to tell those apart.
    pub fn missing_ancestors(&mut self) -> usize {
        self.doc.get_missing_deps(&[]).len()
    }

    /// The whole document, for a peer that just arrived.
    pub fn full(&mut self) -> Vec<u8> {
        self.doc.save()
    }

    /// The document as blocks in order.
    ///
    /// # Errors
    ///
    /// When the document held is not a rich-text document.
    pub fn snapshot(&self) -> Result<tilo_doc::DocSnapshot> {
        let c = word_processor::model::open(&self.doc)
            .map_err(|e| anyhow::anyhow!("the document has no rich-text body: {e}"))?;
        Ok(tilo_doc::model::materialize(&self.doc, &c))
    }

    /// Whether `bytes` are a document with blocks in it: what a page the
    /// agent is invited to looks like before any route says whose it is.
    pub fn looks_like_a_document(bytes: &[u8]) -> bool {
        tilo_doc::model::snapshot_of(bytes).is_some()
    }
}

/// `contract`'s view over the record `bytes` hold in `room`, for `invoker`.
fn open(
    contract: &'static AppContract,
    room: &str,
    bytes: &[u8],
    invoker: &tilo_actions::ActionInvoker,
    permissions: &PermissionContext,
) -> Result<AttachedView> {
    let definition = (contract.definition)();
    let doc = AutoCommit::load(bytes).context("load the record")?;
    // The record the document says it is; one that names none — the room's
    // own document, or a page not saved as a record yet — is held as the
    // room's document.
    let checkout = ui::record::identity(&doc)
        .filter(|_| !definition.keeps_room_record())
        .unwrap_or_else(|| RecordCheckout::package_primary(definition.record_kind.clone()));
    AttachedView::open_for(
        definition,
        room,
        checkout,
        bytes.to_vec(),
        invoker,
        permissions.clone(),
        // What opening it wrote (the record's binding) goes out with the
        // first change, so peers can take what builds on it.
        true,
    )
    .map_err(|error| anyhow::anyhow!("{}'s view would not open: {error}", contract.namespace))
}
