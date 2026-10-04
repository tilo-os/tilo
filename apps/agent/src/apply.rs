//! The agent's replicas, and how it edits them.
//!
//! Each document the agent holds is a framework
//! [`Participant`](ui::framework::Participant) of the app's resource kind:
//! a replica it merges what the room sends into, and edits only through the
//! door a person's own window uses — the app's own actions, checked against
//! the agent's permissions, applied under its own actor, attributed to it and
//! recorded in history with the task it is carrying out. Nothing here writes
//! a document any other way.

use anyhow::{Context, Result};
use automerge::AutoCommit;
use tilo_actions::Resource;
use ui::framework::Participant;
use ui::permissions::PermissionContext;

use crate::surface::AppContract;

/// `action`, and the authorship of the text it adds: the durable `author`
/// mark over every character the agent wrote, so the document itself
/// records who wrote them. Applied together as one edit, as the editor
/// signs what a person types.
pub fn authored(
    action: &word_processor::actions::RichTextAction,
    author: &str,
) -> Vec<word_processor::actions::RichTextAction> {
    let mut actions = vec![action.clone()];
    if let word_processor::actions::RichTextAction::SpliceText {
        region,
        block,
        at,
        ins,
        ..
    } = action
        && !ins.is_empty()
        && !author.is_empty()
    {
        actions.push(word_processor::actions::RichTextAction::SetMark {
            region: *region,
            spans: vec![word_processor::actions::Span {
                block: block.clone(),
                start: *at,
                end: at.saturating_add(ins.chars().count()),
            }],
            name: tilo_doc::schema::MARK_AUTHOR.to_string(),
            value: Some(word_processor::actions::MarkVal::Str(author.to_string())),
            also_clear: Vec::new(),
        });
    }
    actions
}

pub struct Editor {
    participant: Participant,
    /// Which app wrote this document.
    contract: &'static AppContract,
    /// Exact authenticated route that supplied this replica. It can differ
    /// from the application definition `contract` comes from.
    route_app_id: String,
}

/// Whether a snapshot is a document with blocks in it.
fn has_document_schema(doc: &AutoCommit) -> bool {
    tilo_doc::model::open(doc, &automerge::ROOT).is_ok()
}

impl Editor {
    /// A fresh, unbound document of `contract`'s kind, as `author`. Used by
    /// tests; a live agent always starts from a peer's snapshot.
    pub fn empty_for(
        contract: &'static AppContract,
        author: &str,
        permissions: PermissionContext,
    ) -> Self {
        Self {
            participant: Participant::new(contract.kind, author, permissions),
            contract,
            route_app_id: contract.app_id().to_string(),
        }
    }

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
        self.participant.doc()
    }

    /// Set task context for subsequent edits. The session clears it after
    /// the run so a later, unrelated instruction cannot inherit it.
    pub fn set_history_context(&mut self, context: Option<tilo_actions::ActionContext>) {
        self.participant.set_context(context);
    }

    /// The replica's current heads.
    pub fn heads(&mut self) -> Vec<automerge::ChangeHash> {
        self.participant.heads()
    }

    /// The changes the replica holds that `heads` do not.
    pub fn changes_since(&mut self, heads: &[automerge::ChangeHash]) -> Vec<automerge::Change> {
        self.participant.changes_since(heads)
    }

    /// Stable identity of this replica's writer.
    pub fn actor_hex(&self) -> String {
        automerge::ActorId::from(self.participant.actor()).to_hex_string()
    }

    /// The record's version history.
    pub fn versions(&mut self) -> Vec<ui::tilo_resource::history::Version> {
        self.participant.versions()
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
        self.participant
            .record_run(instruction, issued_by, decision, steps)
    }

    /// Newest non-genesis revision, suitable as one end of a run step.
    pub fn latest_entry_id(&mut self) -> Option<ui::HistoryEntryId> {
        self.participant
            .versions()
            .into_iter()
            .rev()
            .find(|version| !tilo_doc::model::is_schema_genesis(version))
            .map(|version| ui::history_source::entry_id_from_hash(&version.hash.to_string()))
    }

    /// The tree the app publishes for this document now. Rebuilt on every
    /// call, since an address only means something if it names what is
    /// there now.
    pub fn resources(&self) -> Vec<Resource> {
        crate::surface::tree(self.contract, self.participant.doc())
    }

    /// The replica the agent was invited to, seeded from a full snapshot a
    /// peer sent on `contract`'s route, written under the agent's own name.
    ///
    /// # Errors
    ///
    /// When `bytes` are not a document of `contract`'s kind.
    pub fn from_snapshot(
        contract: &'static AppContract,
        bytes: &[u8],
        author: &str,
        permissions: PermissionContext,
    ) -> Result<Self> {
        Self::from_snapshot_as(
            contract,
            contract.app_id(),
            bytes,
            author,
            author,
            permissions,
        )
    }

    /// A *peer* document — another app's, in the same room — held under a
    /// replica actor of its own.
    ///
    /// The actor is not cosmetic. An actor id names a **replica**, and the
    /// agent holds one replica per document in the room — sharing one id
    /// across documents means two unrelated ones both carry a change
    /// numbered `(agent, 1)`, and a cluster-scoped feed handing one
    /// document a change belonging to another then makes the real next
    /// change for it a `duplicate seq` that is dropped without a word. The
    /// author its edits are attributed to is the agent's name either way.
    ///
    /// # Errors
    ///
    /// When `bytes` are not a document of `contract`'s kind.
    pub fn peer_from_snapshot_on_route(
        contract: &'static AppContract,
        route_app_id: &str,
        bytes: &[u8],
        author: &str,
        permissions: PermissionContext,
    ) -> Result<Self> {
        Self::from_snapshot_as(
            contract,
            route_app_id,
            bytes,
            author,
            &format!("{author}#{}", contract.namespace),
            permissions,
        )
    }

    fn from_snapshot_as(
        contract: &'static AppContract,
        route_app_id: &str,
        bytes: &[u8],
        author: &str,
        actor: &str,
        permissions: PermissionContext,
    ) -> Result<Self> {
        let participant = Participant::from_snapshot(contract.kind, bytes, author, permissions)
            .with_context(|| format!("load {}'s document", contract.namespace))?
            .writing_as(actor.as_bytes());
        Ok(Self {
            participant,
            contract,
            route_app_id: route_app_id.to_string(),
        })
    }

    /// Merge a full snapshot from a peer. Never replaces: both sides keep
    /// whatever the other had not seen.
    ///
    /// Returns whether the snapshot was taken. A room carries more than one
    /// document, and a subscriber that announced no record — or no app at
    /// all, which is cluster-scoped — is handed all of them, so a snapshot
    /// that is not this document is ordinary rather than an error.
    pub fn merge_full(&mut self, bytes: &[u8]) -> bool {
        self.participant.merge_snapshot(bytes)
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
        let Ok(doc) = AutoCommit::load(bytes) else {
            return false;
        };
        let Ok(c) = tilo_doc::model::open(&doc, &automerge::ROOT) else {
            return false;
        };
        let snapshot = tilo_doc::model::materialize(&doc, &c);
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
        let participant = Participant::from_snapshot(
            self.contract.kind,
            bytes,
            self.participant.me(),
            self.participant.permissions().clone(),
        )
        .context("load a peer snapshot")?
        .writing_as(self.participant.actor());
        self.participant = participant;
        Ok(())
    }

    /// Apply a peer's incremental change set. Returns whether the replica
    /// moved — a delta belonging to one of the room's other documents
    /// refers to objects this one has never seen, and that is not this
    /// agent's business.
    pub fn merge_incremental(&mut self, bytes: &[u8]) -> bool {
        self.participant.merge_changes(bytes)
    }

    /// Apply `actions` of the app's resource `R` as one edit, through the
    /// app's own door: permission-checked, attributed to the agent and
    /// recorded within the task context. Returns the changes to broadcast,
    /// or `None` when nothing changed.
    ///
    /// # Errors
    ///
    /// The door's refusal — the agent may not make this edit, or an action
    /// names something no longer in the document.
    pub fn apply<R: ui::tilo_resource::Resource>(
        &mut self,
        actions: &[R::Action],
    ) -> Result<Option<Vec<u8>>> {
        self.participant
            .apply_all_typed::<R>(actions)
            .map_err(|error| anyhow::anyhow!("{} refused the edit: {error}", R::LABEL))
    }

    /// How many ancestor changes this replica is waiting for.
    ///
    /// Automerge queues a change whose dependencies it has not got, and
    /// nothing about reading the document afterwards says so: it
    /// materialises as the page *before* the gap. So a replica missing one
    /// change in the middle reads exactly like a document with nothing
    /// more in it — this is the one honest way to tell those apart.
    pub fn missing_ancestors(&mut self) -> usize {
        self.participant.missing_changes()
    }

    /// The whole document, for a peer that just arrived.
    pub fn full(&mut self) -> Vec<u8> {
        self.participant.save()
    }

    /// The document as blocks in order.
    ///
    /// # Errors
    ///
    /// When the document held is not a rich-text document.
    pub fn snapshot(&self) -> Result<tilo_doc::DocSnapshot> {
        let doc = self.participant.doc();
        let c = tilo_doc::model::open(doc, &automerge::ROOT)
            .map_err(|e| anyhow::anyhow!("the document has no rich-text schema: {e:?}"))?;
        Ok(tilo_doc::model::materialize(doc, &c))
    }

    /// Whether `bytes` are a document with blocks in it: what a page the
    /// agent is invited to looks like before any route says whose it is.
    pub fn looks_like_a_document(bytes: &[u8]) -> bool {
        AutoCommit::load(bytes).is_ok_and(|doc| has_document_schema(&doc))
    }
}
