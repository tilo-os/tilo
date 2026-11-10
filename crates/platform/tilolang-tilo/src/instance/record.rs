//! The instance's path to its workspace, without the toolkit: what the runtime planned is committed
//! to the workspace record and handed to the workspace through the collab client (`collaboration`), and
//! what the workspace's other replicas committed comes back into the record and the instance.
//!
//! An instance queues each planned change as an invocation of the host's `commit` action
//! ([`Instance::take_invocation`]). [`InstanceRecord::commit`] applies each to the workspace record as
//! the recorded action it is ([`DynAction`]), attributed to the person at the window, and sends
//! the change on the record's route. [`InstanceRecord::receive`] takes in the changes the route
//! brings, checked against the route they came on, and brings the instance up to date with the
//! record.

use std::collections::{BTreeMap, BTreeSet};
use std::rc::Rc;

use app_state::{DocStore, DocWriter};
use automerge::ChangeHash;
use collaboration::document_routes::{decode_routed_mutation, validated_route_payload_with};
use collaboration::history_source::authors::{
    HistoryAuthor, ResolveAuthor, UNKNOWN_AUTHOR_NAME, actor_handle, short_id, stable_hue,
};
use collaboration::history_source::steps::{self, Scope};
use collaboration::history_source::verbs::{self, EDIT_TYPE, HistoryEdit};
use collaboration::history_source::words::HistoryWords;
use collaboration::record::save::{open_stored, record_document};
use collaboration::record::{RecordId, RecordSaveStatus};
use collaboration::{
    CollabEvent, CollabRequest, CollaborationManager, DocumentMutation, RouteSpec,
};
use durable_store::DurableStore;

use serde_json::Value as Json;
use tilo_actions::{ActionContext, ActionInvoker, ActionRegistry, EncodedAction, InvocationId};
use tilo_automerge::ApplyIncoming as _;
use tilo_resource::Doc;

use super::Instance;
use crate::action::{ACTION_TYPE, DynAction, DynEdit};
use crate::chunk::{self, ChunkNo, Chunks, FIRST, Stamps};
use crate::publish::{COMMIT, HOST_RESOURCE};

/// Why a change did not reach the workspace.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RecordError {
    /// The instance has no host over a record yet.
    #[error("the instance holds no workspace record yet")]
    NoHost,
    /// No committed action is queued under the ticket.
    #[error("no committed action {0}")]
    NoAction(u64),
    /// The action could not be applied to the workspace record.
    #[error("the action was refused: {0}")]
    Refused(String),
    /// The record could not be opened: no route, an unreadable copy, or a committed read that
    /// failed.
    #[error("the workspace record could not be opened: {0}")]
    Open(String),
}

/// Where the broker's committed read of a record being opened comes from.
enum Read {
    /// Read on this thread, as the record opens.
    Here,
    /// Read off this thread already; `None` when the route needs none.
    Taken(
        Option<
            Box<(
                collaboration::native_initial::PrimaryRead,
                collaboration::CommittedDocument,
            )>,
        >,
    ),
}

/// The workspace record an instance commits to, the route it travels on, and its history and save
/// state.
///
/// Its history is the one every app's record has (`collaboration::history_source`): each change
/// is a step of whoever made it, and Undo, Redo, History and the other history verbs are
/// [`verbs`], applied to the record as steps of their own, as a native app applies them.
pub struct InstanceRecord {
    /// The record (chunk [`FIRST`]) and the chunks above it this replica holds, each a document of
    /// its own. A change is written in the chunk the manifest places its resource in, and one
    /// action's changes across chunks travel under one workspace commit.
    chunks: Chunks,
    manager: CollaborationManager,
    route: RouteSpec,
    person: String,
    /// The heads of what the workspace was handed, per chunk: a commit sends what follows them.
    /// They move only once the collab client took the change; what it could not take yet (its
    /// queue full, the route still connecting) stays in the record past them and goes with the
    /// next send.
    sent: Stamps,
    /// The chunks above the record whose routes this record joined: a chunk's changes travel on
    /// `derived:chunk.<n>@<base>`, and only a replica that opened it is sent them.
    joined: BTreeSet<ChunkNo>,
    /// Why the record's changes past [`Self::sent`] stay on this device: the collab client
    /// refused them for good (a change above its size limit). They are kept in the record and
    /// its on-disk copy, never dropped.
    unshared: Option<String>,
    /// The words its history verbs speak in.
    words: HostWords,
    /// The on-disk copies of the chunks this replica holds, each a document of its own written
    /// as the record's own is; `None` keeps them in memory only.
    writers: Option<Writers>,
    /// Whether this device governs the workspace: the record's origin (convergence.md §1), whose
    /// copy needs no committed read from another host.
    governs: bool,
}

/// The on-disk copies of the record's chunks: the store the record was opened from, and the
/// writer of each chunk it holds. Chunk [`FIRST`] is the record's own copy, which the record
/// opens; every chunk above it is a document of its own, kept beside the record's.
struct Writers {
    /// Where the copies live: what a chunk this replica comes to hold later is kept in.
    documents: Rc<dyn DurableStore>,
    /// The writer of each chunk this replica holds, the record first.
    chunks: BTreeMap<ChunkNo, DocWriter>,
}

impl Writers {
    /// The copies kept in `documents`, with the writer the record's own chunk
    /// ([`FIRST`]) was opened with.
    fn of(documents: &Rc<dyn DurableStore>, record: DocWriter) -> Self {
        Self {
            documents: Rc::clone(documents),
            chunks: BTreeMap::from([(FIRST, record)]),
        }
    }

    /// The copy of chunk `chunk` of `route` this device kept, as it was left, with the writer
    /// that keeps it from here on: `None` when this device holds none, or it cannot be read.
    fn held(&self, route: &RouteSpec, chunk: ChunkNo) -> Option<(Doc, DocWriter)> {
        let store = copy_document(&self.documents, route, chunk)?;
        let stored = store.has_stored().ok()?;
        stored.then(|| open_stored(store))
    }

    /// The writer of chunk `chunk`, started when this replica comes to hold a chunk it kept no
    /// copy of: what it writes there is on disk from then on.
    fn start(&mut self, route: &RouteSpec, chunk: ChunkNo) -> Option<&mut DocWriter> {
        if !self.chunks.contains_key(&chunk) {
            let writer = copy_document(&self.documents, route, chunk).map_or_else(
                || DocWriter::disabled("the chunk's copy could not be opened"),
                DocStore::writer,
            );
            self.chunks.insert(chunk, writer);
        }
        self.chunks.get_mut(&chunk)
    }
}

/// The document the copy of chunk `chunk` of `route`'s record is kept in, in `documents`: the
/// record of the chunk above the record ([`RecordId::chunk`]), a record of its own beside the
/// record's copy. `None` for the record's own chunk, which the record itself opens, and for a
/// route whose record is no record id.
fn copy_document(
    documents: &Rc<dyn DurableStore>,
    route: &RouteSpec,
    chunk: ChunkNo,
) -> Option<DocStore> {
    if chunk == FIRST {
        return None;
    }
    let base = RecordId::from_route(route.record.as_deref().unwrap_or(""))?;
    let key = base.chunk(chunk).storage_key();
    record_document(
        documents,
        &route.app_id,
        &route.cluster_id.to_string(),
        &key,
    )
    .ok()
}

impl InstanceRecord {
    /// The record `doc`, shared on `route` through `manager`, changed by `person`.
    #[must_use]
    pub fn new(
        doc: Doc,
        manager: CollaborationManager,
        route: RouteSpec,
        person: impl Into<String>,
    ) -> Self {
        Self::of(Chunks::of(doc), manager, route, person)
    }

    /// The record `chunks` (the record's own document, chunk [`FIRST`], and whatever else this
    /// replica already holds), shared on `route` through `manager`, changed by `person`.
    #[must_use]
    pub fn of(
        chunks: Chunks,
        manager: CollaborationManager,
        route: RouteSpec,
        person: impl Into<String>,
    ) -> Self {
        let mut record = Self::holding(chunks, None, manager, route, person.into());
        record.join_chunks();
        record
    }

    fn holding(
        chunks: Chunks,
        writers: Option<Writers>,
        manager: CollaborationManager,
        route: RouteSpec,
        person: String,
    ) -> Self {
        let mut chunks = chunks;
        // What the record's manifest places is where this replica routes its reads and writes.
        chunks.reread_placement();
        // The workspace holds everything each chunk this replica opens holds: only what it makes
        // past then is sent.
        let sent = chunks
            .docs()
            .map(|(chunk, doc)| (chunk, doc.committed_heads()))
            .collect();
        Self {
            chunks,
            manager,
            route,
            person,
            sent,
            joined: BTreeSet::new(),
            unshared: None,
            words: HostWords::new("en-US"),
            writers,
            governs: true,
        }
    }

    /// Whether this device governs the workspace, as the record's origin: a record another host
    /// governs is read from it first ([`Self::open`]). What the origin alone does (a package's
    /// migrations) runs where this holds.
    #[must_use]
    pub const fn governs(&self) -> bool {
        self.governs
    }

    /// The record, its history verbs speaking `locale` (`en-US`, `de-DE`).
    #[must_use]
    pub fn speaking(mut self, locale: impl Into<String>) -> Self {
        self.words = HostWords::new(locale);
        self
    }

    /// The words the record's history verbs speak in.
    #[must_use]
    pub fn words(&self) -> &HostWords {
        &self.words
    }

    /// Open the workspace record of the route `manager` joined, changed by `person`, kept in
    /// `documents`: as a native launch opens its workspace's document. In a workspace this device governs,
    /// the record is its on-disk copy. In a workspace another host governs, the broker's committed
    /// read of the workspace comes first and is merged into that copy, which keeps it from then on.
    pub fn open(
        manager: CollaborationManager,
        documents: &Rc<dyn DurableStore>,
        person: impl Into<String>,
    ) -> Result<Self, RecordError> {
        Self::open_with(manager, Read::Here, documents, person)
    }

    /// [`Self::open`], with the broker's committed read taken off this thread: `read` is what
    /// [`PrimaryRead::of`](collaboration::native_initial::PrimaryRead::of) said the manager's
    /// route needs, and what it read. An owner that runs other instances meanwhile waits for
    /// the read on another thread and opens the record with it here.
    ///
    /// # Errors
    /// As [`Self::open`]; and a read that is not of the manager's route.
    pub fn open_read(
        manager: CollaborationManager,
        read: Option<(
            collaboration::native_initial::PrimaryRead,
            collaboration::CommittedDocument,
        )>,
        documents: &Rc<dyn DurableStore>,
        person: impl Into<String>,
    ) -> Result<Self, RecordError> {
        Self::open_with(manager, Read::Taken(read.map(Box::new)), documents, person)
    }

    fn open_with(
        manager: CollaborationManager,
        read: Read,
        documents: &Rc<dyn DurableStore>,
        person: impl Into<String>,
    ) -> Result<Self, RecordError> {
        use collaboration::native_initial::{self, NativeConstruction};
        use collaboration::record::save::{
            open_stored_adopting, open_with_committed_read, workspace_document,
        };
        let route = manager
            .primary_route()
            .map(|joined| joined.route().clone())
            .ok_or_else(|| RecordError::Open("the connection joined no route".to_owned()))?;
        let workspace = route.cluster_id.to_string();
        // The workspace's record of the app, or the one the route names: a member's own.
        let store = match &route.record {
            None => workspace_document(documents, &route.app_id, &workspace),
            Some(record) => {
                let id = collaboration::record::RecordId::parse(record)
                    .ok_or_else(|| RecordError::Open(format!("`{record}` is no record id")))?;
                record_document(documents, &route.app_id, &workspace, &id.storage_key())
            }
        }
        .map_err(|error| RecordError::Open(error.to_string()))?;
        // Construction custody reads the committed history before the record is built, as a
        // native launch's does: only a route whose reads are committed has one.
        let construction = match read {
            Read::Taken(read) => NativeConstruction::enter_read(manager, read.map(|read| *read)),
            Read::Here => NativeConstruction::enter(manager),
        }
        .map_err(RecordError::Open)?;
        let primary = native_initial::primary(&route.app_id, Some(&workspace));
        let governs = primary.is_none();
        // A record read from the workspace is the workspace's up to what was read. One opened from this
        // replica's own copy was handed to the workspace by nothing yet: its first changes (the
        // copy's creation among them) are made here, and a sealed workspace's channel holds only
        // what was written on it, so the first send hands it all of them.
        let handed_nothing = primary.is_none();
        let opened = match primary {
            Some(read) => read.and_then(|(_, read)| open_with_committed_read(store, &read)),
            // A member's own record of an app that kept the workspace's record before starts
            // from what that record held on this device (`open_stored_adopting`).
            None => Ok(match route.record.as_deref() {
                Some(record) if tilo_action_policy::member_record(record).is_some() => {
                    open_stored_adopting(
                        store,
                        workspace_document(documents, &route.app_id, &workspace).ok(),
                    )
                }
                _ => open_stored(store),
            }),
        };
        let (doc, writer) = match opened {
            Ok(opened) => opened,
            Err(error) => {
                native_initial::fail(&error);
                return Err(RecordError::Open(error));
            }
        };
        let manager = native_initial::transfer_to_app(native_initial::take_manager)
            .map_err(RecordError::Open)?
            .ok_or_else(|| RecordError::Open("the connection was not handed back".to_owned()))?;
        drop(construction);
        let person = person.into();
        let mut writers = Writers::of(documents, writer);
        let mut chunks = Chunks::of(doc);
        // Every chunk the record's manifest names that this device kept a copy of is read with
        // it: what was written into it here and not handed to the workspace yet is still there,
        // and what comes next reaches it as any change does.
        let named: Vec<ChunkNo> = chunks.placement().chunks().into_iter().collect();
        for chunk in named {
            if let Some((copy, writer)) = writers.held(&route, chunk) {
                chunks.insert(chunk, copy);
                writers.chunks.insert(chunk, writer);
            }
        }
        let mut record = Self::holding(chunks, Some(writers), manager, route, person);
        record.governs = governs;
        if handed_nothing {
            record.sent.clear();
        }
        // The chunks the record's manifest names are the ones this replica reads: it opens them
        // all, so a change of one reaches it ([`Self::join_chunks`]).
        record.join_chunks();
        Ok(record)
    }

    /// The record as this replica holds it: chunk [`FIRST`], which every record has and which
    /// holds the manifest of where everything else is.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "an instance's record holds chunk 0 from the moment it opens"
    )]
    pub fn doc(&self) -> &Doc {
        self.chunks.first().expect("the record is held")
    }

    /// The record and the chunks above it this replica holds.
    #[must_use]
    pub fn chunks(&self) -> &Chunks {
        &self.chunks
    }

    /// The chunk `chunk` holds, to read.
    #[must_use]
    pub fn chunk(&self, chunk: ChunkNo) -> Option<&Doc> {
        self.chunks.mirror(chunk)
    }

    /// The collab client the record travels through.
    #[must_use]
    pub fn manager(&self) -> &CollaborationManager {
        &self.manager
    }

    /// The collab client the record travels through, to send what else the instance says on
    /// its connection (actions, presence).
    pub fn manager_mut(&mut self) -> &mut CollaborationManager {
        &mut self.manager
    }

    /// The route the record travels on.
    #[must_use]
    pub fn route(&self) -> &RouteSpec {
        &self.route
    }

    /// Whether the record's changes are stored: in memory only without an on-disk copy. Every
    /// chunk is stored to its own copy, so the record is stored when all of them are.
    #[must_use]
    pub fn save_status(&self) -> RecordSaveStatus {
        let Some(writers) = &self.writers else {
            return RecordSaveStatus::MemoryOnly;
        };
        let mut status = RecordSaveStatus::Saved;
        for (chunk, writer) in &writers.chunks {
            let Some(doc) = self.chunks.mirror(*chunk) else {
                continue;
            };
            match RecordSaveStatus::of_writer(doc, writer) {
                failure @ RecordSaveStatus::Failed(_) => return failure,
                RecordSaveStatus::Saving => status = RecordSaveStatus::Saving,
                RecordSaveStatus::Saved | RecordSaveStatus::MemoryOnly => {}
            }
        }
        status
    }

    /// Whether the record holds changes the workspace was not handed yet: they go with the next
    /// send ([`Self::share`]).
    #[must_use]
    pub fn unsent(&mut self) -> bool {
        !self.unsent_chunks().is_empty()
    }

    /// The chunks holding changes the workspace was not handed yet, the record first.
    fn unsent_chunks(&self) -> Vec<ChunkNo> {
        self.chunks
            .docs()
            .filter(|(chunk, doc)| {
                !heads_equal(
                    &doc.committed_heads(),
                    self.sent.get(chunk).map_or(&[], Vec::as_slice),
                )
            })
            .map(|(chunk, _)| chunk)
            .collect()
    }

    /// Why the record's changes stay on this device, when the collab client refused them for
    /// good: they are kept, in the record and on disk, but no longer shared.
    #[must_use]
    pub fn unshared(&self) -> Option<&str> {
        self.unshared.as_deref()
    }

    /// Write what the record holds to its on-disk copies now, as a window does before it closes:
    /// every chunk it holds, each to its own.
    pub fn flush(&mut self) {
        let Self {
            writers, chunks, ..
        } = self;
        let Some(writers) = writers.as_mut() else {
            return;
        };
        for (chunk, writer) in &mut writers.chunks {
            if let Some(doc) = chunks.mirror_mut(*chunk) {
                writer.request(doc);
            }
            writer.flush();
        }
    }

    /// Have the on-disk copy of chunk `chunk` keep what it holds: the chunk's change goes in the
    /// same durable step as the commit that made it, as the record's own does.
    fn keep(&mut self, chunk: ChunkNo) {
        let Self {
            writers,
            chunks,
            route,
            ..
        } = self;
        let Some(writers) = writers.as_mut() else {
            return;
        };
        let Some(doc) = chunks.mirror_mut(chunk) else {
            return;
        };
        if let Some(writer) = writers.start(route, chunk) {
            writer.request(doc);
        }
    }

    /// The chunk `chunk`, to write into: the one this replica holds, or the one it starts here
    /// (a write into a run that begins there, before that chunk arrives), with the writer that
    /// keeps it on disk like the record's own.
    fn open_chunk(&mut self, chunk: ChunkNo) -> &mut Doc {
        let Self {
            writers,
            chunks,
            route,
            ..
        } = self;
        if let Some(writers) = writers.as_mut()
            && !writers.chunks.contains_key(&chunk)
        {
            // A chunk this replica writes to keeps its own copy: it is started before the doc
            // is written, so the change goes to disk with the record's own.
            let _ = writers.start(route, chunk);
        }
        chunks.open(chunk)
    }

    /// Whether the person at the window has a step to undo, or an undo to redo.
    #[must_use]
    pub fn can_undo(&self) -> (bool, bool) {
        self.can_undo_as(&self.person)
    }

    /// Whether `person` has a step to undo, or an undo to redo, in the record: what the
    /// history verbs `undo` and `redo` would take for them.
    #[must_use]
    pub fn can_undo_as(&self, person: &str) -> (bool, bool) {
        let scope = Scope::Invoker(person.to_owned());
        let owns = |version: &tilo_resource::history::Version| scope.owns(version);
        let mut undo = false;
        let mut redo = false;
        for (_, doc) in self.chunks.docs() {
            undo |= !steps::undo_preview(doc, &scope, 1).is_empty();
            redo |= !steps::undos_in_effect(doc, &owns, 1).is_empty();
        }
        (undo, redo)
    }

    /// The invocation the person's last step in the record ran in: the id every change of the
    /// action that made it names, in this unit and in every other one the action wrote.
    #[must_use]
    pub fn last_step_invocation(&self) -> Option<InvocationId> {
        let scope = Scope::Invoker(self.person.clone());
        let step = self.last_step(&scope)?;
        self.invocation_of(&step)
    }

    /// The newest step `scope` has in effect among the chunks: the change whose time is the
    /// latest, the record's own before a chunk's when two share one (a change's time is what
    /// orders the chunks, which share no history between them).
    fn last_step(&self, scope: &Scope) -> Option<[u8; 32]> {
        self.chunks
            .docs()
            .filter_map(|(_, doc)| {
                let step = steps::undo_preview(doc, scope, 1).into_iter().next()?;
                Some((step.time_ms, step.hash.0))
            })
            .max()
            .map(|(_, hash)| hash)
    }

    /// The changes of this unit the invocation `invocation` made, oldest first: the unit's part
    /// of that action, read from the documents themselves. An undo across the action needs
    /// nothing else, so it works after a restart that lost every log the transport kept.
    #[must_use]
    pub fn changes_of(&self, invocation: InvocationId) -> Vec<[u8; 32]> {
        let mut changes: Vec<(i64, [u8; 32])> = Vec::new();
        for (_, doc) in self.chunks.docs() {
            for change in doc.committed_document().get_changes(&[]) {
                if change
                    .message()
                    .and_then(ActionContext::from_change_message)
                    .is_some_and(|context| context.source_invocation == Some(invocation))
                {
                    changes.push((change.timestamp(), change.hash().0));
                }
            }
        }
        changes.sort_unstable();
        changes.into_iter().map(|(_, hash)| hash).collect()
    }

    /// The chunk of this instance that holds the change `change`.
    #[must_use]
    pub fn chunk_of_change(&self, change: &[u8; 32]) -> Option<ChunkNo> {
        self.chunks
            .docs()
            .find(|(_, doc)| {
                doc.committed_document()
                    .get_changes(&[])
                    .iter()
                    .any(|known| known.hash().0 == *change)
            })
            .map(|(chunk, _)| chunk)
    }

    /// The invocation the change `change` of this instance ran in.
    fn invocation_of(&self, change: &[u8; 32]) -> Option<InvocationId> {
        let chunk = self.chunk_of_change(change)?;
        self.chunks
            .mirror(chunk)?
            .committed_document()
            .get_changes(&[])
            .into_iter()
            .find(|known| known.hash().0 == *change)
            .and_then(|known| known.message().and_then(ActionContext::from_change_message))
            .and_then(|context| context.source_invocation)
    }

    /// Whether the step the change `change` is part of is still in effect for the person: what an
    /// undo of it takes back, and not a step already undone or not theirs.
    #[must_use]
    pub fn can_revert(&self, change: &[u8; 32]) -> bool {
        let Some(doc) = self
            .chunk_of_change(change)
            .and_then(|chunk| self.chunk(chunk))
        else {
            return false;
        };
        let scope = Scope::Invoker(self.person.clone());
        steps::steps_in_effect(doc, &|version| scope.owns(version), usize::MAX)
            .into_iter()
            .flatten()
            .any(|version| version.hash.0 == *change)
    }

    /// Take back the step the change `change` is part of through the history verb `undo`, as a
    /// step of `invoker` in `ambient`, and leave it unsent: the caller sends every unit of the
    /// action under one commit ([`Self::unsent_change`], [`Self::share_under`]). The instance is
    /// brought up to date.
    ///
    /// # Errors
    /// The step is undone already, is not one this undo may take back, or its inverse could not
    /// be applied.
    pub fn revert_step(
        &mut self,
        instance: &mut Instance,
        resource: &str,
        kind: &str,
        invoker: ActionInvoker,
        ambient: &ActionContext,
        change: &[u8; 32],
    ) -> Result<(), RecordError> {
        let edit = HistoryEdit::Undo {
            steps: None,
            run: None,
            by: None,
            to: None,
            step: Some(ChangeHash(*change).to_string()),
        };
        let chunk = self.chunk_of_change(change).unwrap_or(FIRST);
        self.history_apply(chunk, &edit, resource, kind, invoker, Some(ambient))?;
        instance.sync_chunks(&self.chunks);
        Ok(())
    }

    /// Take back the person's last step through the history verb `undo` on `resource` (a
    /// resource `kind`), as a step of their own that the workspace is handed like any other; the
    /// instance is brought up to date. Whether there was one.
    ///
    /// What somebody else changed since stays theirs: the step is taken back around it.
    ///
    /// # Errors
    /// The undo failed: the step's inverse could not be applied.
    pub fn undo(
        &mut self,
        instance: &mut Instance,
        resource: &str,
        kind: &str,
    ) -> Result<bool, RecordError> {
        let scope = Scope::Invoker(self.person.clone());
        let Some(step) = self.last_step(&scope) else {
            return Ok(false);
        };
        // One step may have written several chunks, each with a change of its own: the undo
        // takes back the change of every one of them, so the step is back everywhere it was made.
        // Its parts are one action of its own, under one invocation, and go out as one commit.
        let ambient = ActionContext {
            source_invocation: Some(InvocationId::new()),
            ..ActionContext::default()
        };
        let mut taken = false;
        for change in self.changes_of_step(&step) {
            if !self.can_revert(&change) {
                continue;
            }
            let invoker = ActionInvoker::actor(self.person.clone());
            let chunk = self.chunk_of_change(&change).unwrap_or(FIRST);
            let edit = HistoryEdit::Undo {
                steps: None,
                run: None,
                by: None,
                to: None,
                step: Some(ChangeHash(change).to_string()),
            };
            self.history_apply(chunk, &edit, resource, kind, invoker, Some(&ambient))?;
            taken = true;
        }
        Ok(self.finish_history(instance, taken))
    }

    /// The changes the step the change `step` is part of made across the chunks: its own, and
    /// those of the other chunks that ran under the same invocation.
    fn changes_of_step(&self, step: &[u8; 32]) -> Vec<[u8; 32]> {
        match self.invocation_of(step) {
            Some(invocation) => self.changes_of(invocation),
            None => vec![*step],
        }
    }

    /// Carry out what the person asked of their history at the window (`asked`) through the
    /// history verb `undo` or `redo` on `resource` (a resource `kind`), as a step of their own;
    /// the instance is brought up to date. Whether anything was taken back or brought back.
    pub fn asked(
        &mut self,
        instance: &mut Instance,
        asked: &crate::instance::HistoryAsk,
        resource: &str,
        kind: &str,
    ) -> Result<bool, RecordError> {
        use crate::instance::HistoryAsk;
        match asked {
            HistoryAsk::Undo => return self.undo(instance, resource, kind),
            HistoryAsk::Redo => return self.redo(instance, resource, kind),
            HistoryAsk::UndoStep(_) | HistoryAsk::UndoRun(_) => {}
        }
        let edit = asked.edit();
        let chunk = match asked {
            HistoryAsk::UndoStep(change) => {
                let change = change.parse::<ChangeHash>().map_err(|error| {
                    RecordError::Refused(format!("{change} names no change of the record: {error}"))
                })?;
                self.chunk_of_change(&change.0).unwrap_or(FIRST)
            }
            HistoryAsk::Undo | HistoryAsk::Redo | HistoryAsk::UndoRun(_) => FIRST,
        };
        let me = ActionInvoker::actor(self.person.clone());
        self.history_chunk(instance, chunk, &edit, resource, kind, me, None)
            .map(|_| true)
    }

    /// Bring back the person's last undo through the history verb `redo`. Whether there was one.
    ///
    /// # Errors
    /// As [`Self::undo`].
    pub fn redo(
        &mut self,
        instance: &mut Instance,
        resource: &str,
        kind: &str,
    ) -> Result<bool, RecordError> {
        if !self.can_undo().1 {
            return Ok(false);
        }
        let scope = Scope::Invoker(self.person.clone());
        let owns = |version: &tilo_resource::history::Version| scope.owns(version);
        let ambient = ActionContext {
            source_invocation: Some(InvocationId::new()),
            ..ActionContext::default()
        };
        // The undo being redone may have taken back changes in several chunks: each is brought
        // back in the chunk that holds it, as one action, sent once all of them are.
        let undone: Vec<ChunkNo> = self
            .chunks
            .docs()
            .filter(|(_, doc)| !steps::undos_in_effect(doc, &owns, 1).is_empty())
            .map(|(chunk, _)| chunk)
            .collect();
        let mut brought = false;
        for chunk in undone {
            let edit = HistoryEdit::Redo { steps: None };
            let invoker = ActionInvoker::actor(self.person.clone());
            self.history_apply(chunk, &edit, resource, kind, invoker, Some(&ambient))?;
            brought = true;
        }
        Ok(self.finish_history(instance, brought))
    }

    /// Send what the history verb `taken` wrote, once, as one action across the chunks it
    /// touched, and bring the instance up to date. Whether anything was taken back or brought
    /// back.
    fn finish_history(&mut self, instance: &mut Instance, taken: bool) -> bool {
        if !taken {
            return false;
        }
        if let Some(why) = self.share() {
            instance.unshared(why);
        }
        instance.sync_chunks(&self.chunks);
        true
    }

    /// Apply the history verb `edit`, invoked on `resource` (a resource `kind`) by `invoker` in
    /// `ambient`, to the record as one step of its own, as a native app applies it; send it on
    /// the record's route and bring the instance up to date. What the verb did, as its caller
    /// is answered ([`verbs::result`]).
    pub fn history(
        &mut self,
        instance: &mut Instance,
        edit: &HistoryEdit,
        resource: &str,
        kind: &str,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<Option<Json>, RecordError> {
        self.history_chunk(instance, FIRST, edit, resource, kind, invoker, ambient)
    }

    /// [`Self::history`] on the chunk `chunk`: the undo or redo of a step written there is a step
    /// of that chunk's document.
    fn history_chunk(
        &mut self,
        instance: &mut Instance,
        chunk: ChunkNo,
        edit: &HistoryEdit,
        resource: &str,
        kind: &str,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<Option<Json>, RecordError> {
        let (result, unshared) =
            self.history_step(chunk, edit, resource, kind, invoker, ambient)?;
        if let Some(why) = unshared {
            instance.unshared(why);
        }
        instance.sync_chunks(&self.chunks);
        Ok(result)
    }

    /// [`Self::history`] with no instance: the record's origin applying an agent task's history
    /// verb (its `undo`).
    pub fn history_at_origin(
        &mut self,
        edit: &HistoryEdit,
        resource: &str,
        kind: &str,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<Option<Json>, RecordError> {
        match self.history_step(FIRST, edit, resource, kind, invoker, ambient)? {
            (_, Some(why)) => Err(RecordError::Refused(why)),
            (result, None) => Ok(result),
        }
    }

    /// Apply the history verb `edit` as one step and send it: what it did, and why the workspace
    /// refused its change for good, when it did.
    fn history_step(
        &mut self,
        chunk: ChunkNo,
        edit: &HistoryEdit,
        resource: &str,
        kind: &str,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<(Option<Json>, Option<String>), RecordError> {
        let result = self.history_apply(chunk, edit, resource, kind, invoker, ambient)?;
        Ok((result, self.share()))
    }

    /// Apply the history verb `edit` as one step, leaving it unsent: its caller sends it, alone or
    /// as its unit's part of one action.
    fn history_apply(
        &mut self,
        chunk: ChunkNo,
        edit: &HistoryEdit,
        resource: &str,
        kind: &str,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<Option<Json>, RecordError> {
        let action = HostHistoryEdit {
            edit: edit.clone(),
            words: self.words.clone(),
        };
        let encoded = EncodedAction {
            type_id: EDIT_TYPE.to_owned(),
            payload: serde_json::to_vec(edit)
                .map_err(|error| RecordError::Refused(error.to_string()))?,
        };
        let before = self.open_chunk(chunk).get_heads();
        // The verb's change is a step of its own, or a part of the one action an undo or redo
        // across chunks is (`ambient`'s invocation).
        let invocation = ambient
            .and_then(|ambient| ambient.source_invocation)
            .unwrap_or_default();
        let context = ActionContext::for_resource(resource, kind, kind)
            .with_source_invocation(Some(invocation));
        self.apply_on(chunk, &action, encoded, context, invoker, ambient)?;
        let Some(doc) = self.chunks.mirror_mut(chunk) else {
            return Err(RecordError::Refused(format!(
                "chunk {chunk} is not held, though its change was taken"
            )));
        };
        let after = doc.get_heads();
        let patches = doc.diff(&before, &after);
        Ok(verbs::result(edit.verb(), doc, &patches, &self.words))
    }

    /// Answer the history view `payload` asks ([`verbs::VIEW_TYPE`]) of the record.
    ///
    /// The view is the record's own chunk: a step written in another chunk is listed there. The
    /// action that made it is in the workspace's log, which names every unit the action wrote.
    pub fn view_history(&self, payload: &[u8]) -> Result<tilo_actions::ActionOutput, String> {
        verbs::answer(self.doc(), payload, &self.words)
    }

    /// Hand the workspace what the record holds past what it was handed, as one change. What the
    /// collab client cannot take yet (its queue is full, or the route is still connecting) stays
    /// in the record and goes with the next send: a frame sends again while any is left
    /// ([`Self::unsent`]), and the client wakes its owner once it has workspace. A change it
    /// refuses for good (above its size limit) stays on this device, kept in the record and on
    /// disk: the reason is returned the first time, for the window to say so.
    pub fn share(&mut self) -> Option<String> {
        self.send(None)
    }

    /// What this unit holds past what the workspace was handed, as its parts: one per chunk,
    /// each the part of an action its caller sends with every other unit of the action
    /// ([`Self::share_under`]). Empty when the workspace holds all of it.
    pub fn unsent_parts(&self) -> Vec<(ChunkNo, DocumentMutation)> {
        self.chunks
            .docs()
            .filter_map(|(chunk, doc)| {
                let sent = self.sent.get(&chunk).map_or(&[][..], Vec::as_slice);
                let change = doc.committed_document().save_after(sent);
                (!change.is_empty()).then_some((chunk, DocumentMutation::Incremental(change)))
            })
            .collect()
    }

    /// What the record holds past what the workspace was handed, as one change: the record's
    /// own. Empty when the workspace holds all of it.
    pub fn unsent_change(&mut self) -> DocumentMutation {
        let sent = self.sent.get(&FIRST).map_or(&[][..], Vec::as_slice);
        DocumentMutation::Incremental(self.doc().committed_document().save_after(sent))
    }

    /// [`Self::share`] as this unit's part of `commit`, the one action across every unit it
    /// wrote: the parts travel together, each carrying the same commit, so a peer applies all of
    /// the action or none of it.
    pub fn share_under(&mut self, commit: &collab_types::WorkspaceCommit) -> Option<String> {
        self.send(Some(commit))
    }

    fn send(&mut self, commit: Option<&collab_types::WorkspaceCommit>) -> Option<String> {
        let unsent = self.unsent_chunks();
        if unsent.is_empty() {
            return None;
        }
        for chunk in unsent {
            self.keep(chunk);
        }
        if self.unshared.is_some() {
            return None;
        }
        let parts = self.unsent_parts();
        // The parts of one action that wrote several chunks go out as one workspace commit,
        // naming every one of them: a peer applies all of the action or none of it. A part whose
        // action wrote one chunk travels as any change does.
        let commits = match commit {
            Some(commit) => vec![Some(commit.clone()); parts.len()],
            None => self.commits_of(&parts),
        };
        for ((chunk, mutation), commit) in parts.into_iter().zip(commits) {
            let DocumentMutation::Incremental(change) = mutation else {
                continue;
            };
            if change.is_empty() {
                // The workspace holds all of it already, by heads named apart.
                self.note_sent(chunk);
                continue;
            }
            let request = match commit {
                Some(commit) => {
                    CollabRequest::document_action(DocumentMutation::Incremental(change), commit)
                }
                None => CollabRequest::document_incremental(change),
            };
            let sent = match self.send_on(chunk, request) {
                Ok(()) => true,
                Err(error @ collaboration::CollabSendError::TooLarge { .. }) => {
                    tracing::warn!(chunk, %error, "workspace: the chunk's changes stay on this device");
                    self.unshared = Some(error.to_string());
                    return self.unshared.clone();
                }
                Err(error) => {
                    tracing::debug!(chunk, %error, "workspace: the change goes with the next send");
                    false
                }
            };
            if sent {
                self.note_sent(chunk);
            }
        }
        None
    }

    /// The commit each unsent part travels under: the parts of one action that wrote several
    /// chunks share one commit naming every one of them, so they reach a peer together or not at
    /// all. A part whose action wrote one chunk only, or whose changes name no action, travels
    /// under none.
    fn commits_of(
        &self,
        parts: &[(ChunkNo, DocumentMutation)],
    ) -> Vec<Option<collab_types::WorkspaceCommit>> {
        let records: Vec<String> = parts
            .iter()
            .map(|(chunk, _)| self.chunk_record(*chunk))
            .collect();
        let units: Vec<collab_types::UnitMutation<'_>> = parts
            .iter()
            .zip(&records)
            .map(|((_, mutation), record)| collab_types::UnitMutation {
                app: &self.route.app_id,
                record,
                mutation,
            })
            .collect();
        let workspace = self.route.cluster_id.to_string();
        let author = collab_types::CommitAuthor {
            principal: None,
            device: [0; 32],
        };
        collab_types::WorkspaceCommit::of_units(&workspace, &author, &units).map_or_else(
            |_| vec![None; parts.len()],
            |commits| {
                commits
                    .into_iter()
                    .map(|commit| commit.filter(|commit| commit.parts.len() > 1))
                    .collect()
            },
        )
    }

    /// The record id chunk `chunk` travels on: the record's own for [`FIRST`], the chunk's
    /// record's above it ([`chunk::record_id`]).
    fn chunk_record(&self, chunk: ChunkNo) -> String {
        let base = self.route.record.as_deref().unwrap_or("");
        if chunk == FIRST {
            base.to_owned()
        } else {
            chunk::record_id(base, chunk)
        }
    }

    /// The routes this replica takes changes in on: the record's own, and each chunk's it holds
    /// or the manifest names ([`Self::chunk_route`]).
    fn taken_routes(&self) -> Vec<(RouteSpec, ChunkNo)> {
        let mut chunks: BTreeSet<ChunkNo> = self.chunks.placement().chunks();
        chunks.extend(self.chunks.numbers().filter(|chunk| *chunk != FIRST));
        let mut routes = vec![(self.route.clone(), FIRST)];
        routes.extend(
            chunks
                .into_iter()
                .map(|chunk| (self.chunk_route(chunk), chunk)),
        );
        routes
    }

    /// Note that what arrived of chunk `chunk` is the workspace's already, so nothing of it is
    /// sent back; what this replica made and the workspace was not handed yet still goes with the
    /// next send.
    fn note_arrived(&mut self, chunk: ChunkNo, before: &[ChangeHash]) {
        let sent = self.sent.get(&chunk).cloned().unwrap_or_default();
        let heads = self
            .chunks
            .mirror(chunk)
            .map(Doc::committed_heads)
            .unwrap_or_default();
        let mut held = sent.clone();
        if !heads_equal(before, &sent) {
            for head in heads {
                if !before.contains(&head) && !held.contains(&head) {
                    held.push(head);
                }
            }
        }
        self.sent.insert(chunk, held);
        self.keep(chunk);
    }

    /// Note that the workspace holds chunk `chunk` as it is now.
    fn note_sent(&mut self, chunk: ChunkNo) {
        let heads = self
            .chunks
            .mirror(chunk)
            .map(Doc::committed_heads)
            .unwrap_or_default();
        self.sent.insert(chunk, heads);
    }

    /// Send `request` on the route of chunk `chunk`: the record's own route for chunk
    /// [`FIRST`], `derived:chunk.<n>@<base>` for the chunks above it.
    fn send_on(
        &mut self,
        chunk: ChunkNo,
        request: CollabRequest,
    ) -> Result<(), collaboration::CollabSendError> {
        if chunk == FIRST {
            self.manager.try_send(request)
        } else {
            self.manager
                .try_send_to_route(self.chunk_route(chunk), request)
        }
    }

    /// The route chunk `chunk` of the record travels on: the record's own for [`FIRST`], the
    /// chunk's record's above it ([`chunk::record_id`]).
    #[must_use]
    pub fn chunk_route(&self, chunk: ChunkNo) -> RouteSpec {
        if chunk == FIRST {
            return self.route.clone();
        }
        RouteSpec::record(
            self.route.app_id.clone(),
            self.route.cluster_id,
            chunk::record_id(self.route.record.as_deref().unwrap_or(""), chunk),
        )
    }

    /// Join the route of every chunk the record's manifest names: a chunk's changes reach only
    /// the replicas that opened it, so a chunk this replica holds or wants is one it joined.
    fn join_chunks(&mut self) {
        for chunk in self.chunks.placement().chunks() {
            if !self.joined.insert(chunk) {
                continue;
            }
            self.manager.join_route(
                self.chunk_route(chunk),
                collaboration::record::RecordKind::new(crate::RECORD_KIND),
            );
        }
    }

    /// Hand the workspace the whole of chunk `chunk` on its route, as the answer to a late joiner
    /// the broker asked this replica to bring up to date. One the collab client does not take is
    /// said: the joiner asks again while no holder answers.
    fn send_snapshot(&mut self, chunk: ChunkNo) {
        let Some(doc) = self.chunks.mirror(chunk) else {
            return;
        };
        let request = CollabRequest::document_snapshot(doc.committed_document().save());
        let sent = match self.send_on(chunk, request) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(chunk, %error, "workspace: a snapshot for a late joiner was not sent");
                false
            }
        };
        let _ = sent;
    }

    /// Commit every change `instance` queued for the workspace, in order, as a step each, and send
    /// them on the record's route ([`Self::share`]); the instance hears each answered. Returns
    /// the tickets committed.
    ///
    /// A change is the workspace's once it is in the record: the collab client not taking it yet
    /// never refuses it. An invocation of anything but the host's `commit` is answered refused:
    /// this path runs only what the instance planned itself.
    ///
    /// # Errors
    /// The first change that could not be applied to the record; it is answered refused, and
    /// what is still queued stays queued.
    pub fn commit(&mut self, instance: &mut Instance) -> Result<Vec<u64>, RecordError> {
        let committed = self.commit_queued(instance);
        if let Some(why) = self.share() {
            instance.unshared(why);
        }
        instance.sync_chunks(&self.chunks);
        committed
    }

    fn commit_queued(&mut self, instance: &mut Instance) -> Result<Vec<u64>, RecordError> {
        let mut committed = Vec::new();
        while let Some((invocation, _origin)) = instance.take_invocation() {
            let address = invocation.address.clone();
            if address.resource != HOST_RESOURCE || address.action != COMMIT {
                instance.answered(
                    &address,
                    Some("the workspace path runs only the instance's own commits".to_owned()),
                );
                continue;
            }
            let ticket = invocation
                .arguments
                .get("ticket")
                .and_then(Json::as_u64)
                .unwrap_or_default();
            match self.commit_ticket(instance, ticket) {
                Ok(()) => {
                    instance.answered(&address, None);
                    committed.push(ticket);
                }
                Err(error) => {
                    instance.answered(&address, Some(error.to_string()));
                    return Err(error);
                }
            }
        }
        Ok(committed)
    }

    fn commit_ticket(&mut self, instance: &Instance, ticket: u64) -> Result<(), RecordError> {
        let host = instance.host().ok_or(RecordError::NoHost)?;
        let action = host
            .store()
            .committed(ticket)
            .ok_or(RecordError::NoAction(ticket))?;
        let invoker = ActionInvoker::actor(self.person.clone());
        // The stage of the person's the change is one of, when it is one of a burst of typing;
        // every other change is a step of its own.
        let gesture = action
            .authorship
            .gesture
            .unwrap_or_else(tilo_actions::ActionId::new);
        self.apply_in_gesture(&action, &invoker, None, Some(gesture))?;
        Ok(())
    }

    /// Apply `action`, which the door built for `invoker` (a person or an agent task the broker
    /// admitted), to the workspace record as one step of its own, and send it on the record's route.
    /// `ambient` is the task or run it was invoked in. The instance is brought up to date.
    pub fn apply_invoked(
        &mut self,
        instance: &mut Instance,
        action: &DynAction,
        invoker: &ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<(), RecordError> {
        self.apply(action, invoker, ambient)?;
        if let Some(why) = self.share() {
            instance.unshared(why);
        }
        instance.sync_chunks(&self.chunks);
        Ok(())
    }

    fn apply(
        &mut self,
        action: &DynAction,
        invoker: &ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<(), RecordError> {
        self.apply_in_gesture(action, invoker, ambient, None)
    }

    /// As [`Self::apply`], the change a part of `gesture` when there is one: the changes of one
    /// gesture are one undo step.
    fn apply_in_gesture(
        &mut self,
        action: &DynAction,
        invoker: &ActionInvoker,
        ambient: Option<&ActionContext>,
        gesture: Option<tilo_actions::ActionId>,
    ) -> Result<(), RecordError> {
        let mut context = ActionContext::for_resource(
            action.resources().first().copied().unwrap_or("workspace"),
            &action.kind,
            &action.kind,
        );
        context.gesture = gesture;
        // One invocation names every change the action makes, in whichever chunk each is
        // written: the workspace takes them as one action and one undo takes them all back. A
        // run names its own, so the caller's change and its callees' are one action.
        let invocation = action
            .authorship
            .invocation
            .as_deref()
            .map_or_else(InvocationId::new, crate::action::invocation_id);
        let context = context.with_source_invocation(Some(invocation));
        self.apply_across(action, &context, invoker, ambient)
    }

    /// Apply `action`, its changes recorded with `context`, where its edits belong: each chunk
    /// the action writes holds its part of it, as one signed change of that chunk, and every part
    /// names the one invocation the action ran in. The caller sends them ([`Self::share`]), as
    /// one workspace commit when they are more than one.
    fn apply_across(
        &mut self,
        action: &DynAction,
        context: &ActionContext,
        invoker: &ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<(), RecordError> {
        let parts = self.parts_of(action);
        let mut wrote: Vec<(ChunkNo, Vec<ChangeHash>)> = Vec::new();
        for (chunk, edits) in parts {
            // The part is what its change records, so a peer replaying it on the chunk it was
            // written in reproduces the same change.
            let part = DynAction {
                edits,
                ..action.clone()
            };
            let encoded = part
                .encode()
                .map_err(|error| RecordError::Refused(error.to_string()))?;
            match self.apply_on(
                chunk,
                &part,
                encoded,
                context.clone(),
                invoker.clone(),
                ambient,
            ) {
                Ok(before) => wrote.push((chunk, before)),
                Err(error) => {
                    // An action the workspace does not take leaves nothing in any chunk, not
                    // even the parts it had written before the one it refused.
                    self.take_back(wrote);
                    return Err(error);
                }
            }
        }
        if wrote.iter().any(|(chunk, _)| *chunk == FIRST) {
            // A run the action started, filled or moved is written in the record's manifest: the
            // chunks it names now are the ones this replica holds.
            self.chunks.reread_placement();
        }
        self.join_chunks();
        Ok(())
    }

    /// Take the chunks `written`, with the heads each held before the action, back to what they
    /// held before it: an action refused partway leaves nothing in any chunk.
    fn take_back(&mut self, written: Vec<(ChunkNo, Vec<ChangeHash>)>) {
        for (chunk, before) in written {
            let Some(doc) = self.chunks.mirror(chunk) else {
                continue;
            };
            let actor = doc.get_actor().clone();
            match doc.committed_fork_at(&before) {
                Ok(mut taken) => {
                    taken.set_actor(actor);
                    self.chunks.insert(chunk, taken);
                }
                Err(error) => tracing::error!(
                    app = %self.route.app_id,
                    chunk,
                    %error,
                    "workspace: an action it did not take could not be taken back"
                ),
            }
        }
        self.chunks.reread_placement();
    }

    /// The edits of `action` by the chunk they are written in: the chunk an edit names, else the
    /// one the record's manifest places its resource in.
    fn parts_of(&self, action: &DynAction) -> BTreeMap<ChunkNo, Vec<DynEdit>> {
        let mut parts: BTreeMap<ChunkNo, Vec<DynEdit>> = BTreeMap::new();
        for edit in &action.edits {
            let chunk = edit
                .chunk()
                .unwrap_or_else(|| self.chunks.chunk(edit.resource()));
            parts.entry(chunk).or_default().push(edit.clone());
        }
        parts
    }

    /// Apply `action`, encoded as `encoded`, to the chunk `chunk` as one step of its own,
    /// recorded with `context`; the heads the chunk held before it. The caller sends it on the
    /// chunk's route ([`Self::share`]).
    fn apply_on(
        &mut self,
        chunk: ChunkNo,
        action: &dyn tilo_actions::Action,
        encoded: EncodedAction,
        mut context: ActionContext,
        invoker: ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<Vec<ChangeHash>, RecordError> {
        if let Some(ambient) = ambient {
            context = context.enriched_by(ambient);
        }
        let author = invoker.actor.clone();
        let context = context.with_invoker(invoker);
        let before = {
            let doc = self.open_chunk(chunk);
            let before = doc.get_heads();
            tilo_actions::apply_local_with_context(
                doc,
                action,
                encoded,
                Some(author.as_bytes()),
                context,
            )
            .map_err(|error| RecordError::Refused(error.to_string()))?;
            // The change is a step of whoever made it, by the invoker its entry names: their
            // Undo takes it back.
            doc.commit();
            before
        };
        self.keep_only_if_the_workspace_takes(chunk, &before)?;
        Ok(before)
    }

    /// Keep what chunk `chunk` holds past `before` only if the workspace takes it, as every
    /// receiver decides its claims against the app's surface
    /// ([`collaboration::app_surface::refusal`]): a change the workspace refuses is taken out of
    /// the chunk again, with all of it, so it is never kept here, built on and written to disk
    /// while the workspace holds none of it.
    fn keep_only_if_the_workspace_takes(
        &mut self,
        chunk: ChunkNo,
        before: &[ChangeHash],
    ) -> Result<(), RecordError> {
        let Some(why) = self.refusal_of(chunk, before) else {
            return Ok(());
        };
        tracing::warn!(app = %self.route.app_id, chunk, %why, "workspace: a change the workspace refuses was not kept");
        let kept = {
            let Some(doc) = self.chunks.mirror(chunk) else {
                return Err(RecordError::Refused(format!(
                    "{why}; and chunk {chunk} is not held"
                )));
            };
            let actor = doc.get_actor().clone();
            let mut kept = doc.committed_fork_at(before).map_err(|error| {
                RecordError::Refused(format!("{why}; and it could not be taken out: {error}"))
            })?;
            kept.set_actor(actor);
            kept
        };
        self.chunks.insert(chunk, kept);
        Err(RecordError::Refused(format!(
            "the workspace refuses it: {why}"
        )))
    }

    /// Why the workspace refuses what chunk `chunk` holds past `before`, when it does.
    fn refusal_of(&self, chunk: ChunkNo, before: &[ChangeHash]) -> Option<String> {
        let change = self
            .chunks
            .mirror(chunk)?
            .committed_document()
            .save_after(before);
        if change.is_empty() {
            return None;
        }
        let workspace = self.route.cluster_id.to_string();
        let record_id = self.chunk_record(chunk);
        let record = collab_types::Record {
            room: &workspace,
            app: &self.route.app_id,
            record: &record_id,
        };
        collaboration::app_surface::refusal(
            &record,
            &collab_types::DocumentMutation::Incremental(change),
        )
    }

    /// Take in what the record's route brought and bring `instance` up to date with the
    /// record. Whether the record changed.
    pub fn receive(&mut self, instance: &mut Instance) -> bool {
        self.receive_and(instance, |_| {})
    }

    /// [`Self::receive`], handing every event that is no change of the record (an invocation,
    /// presence, the route's state) to `other`, in the order they came.
    pub fn receive_and(&mut self, instance: &mut Instance, other: impl FnMut(CollabEvent)) -> bool {
        let changed = self.take_in(other);
        if changed {
            instance.sync_chunks(&self.chunks);
        }
        changed
    }

    /// The heads of every chunk this replica holds now: what [`Self::changes_since`] starts from.
    #[must_use]
    pub fn marks(&self) -> Stamps {
        self.chunks
            .docs()
            .map(|(chunk, doc)| (chunk, doc.committed_heads()))
            .collect()
    }

    /// What this replica holds past `marks`, as Automerge changes: what a change applied here
    /// wrote, in whichever chunk, for its invoker to read back without waiting on the workspace.
    #[must_use]
    pub fn changes_since(&self, marks: &Stamps) -> Vec<u8> {
        let mut changes = Vec::new();
        for (chunk, doc) in self.chunks.docs() {
            let from = marks.get(&chunk).map_or(&[][..], Vec::as_slice);
            changes.extend_from_slice(&doc.committed_document().save_after(from));
        }
        changes
    }

    /// Take in what the record's route brought, with no instance to bring up to date: a record
    /// held only to apply changes at its origin. Events that are no change of the record are
    /// dropped. Whether the record changed.
    pub fn take_in_record(&mut self) -> bool {
        self.take_in(|_| {})
    }

    /// Apply `action`, which the door built for `invoker` (an agent task the broker admitted),
    /// to the workspace record as one step of its own in `ambient`, and send it on the record's
    /// route, with no instance: the record's origin applying a change.
    pub fn apply_change(
        &mut self,
        action: &DynAction,
        invoker: &ActionInvoker,
        ambient: Option<&ActionContext>,
    ) -> Result<(), RecordError> {
        self.apply(action, invoker, ambient)?;
        match self.share() {
            Some(why) => Err(RecordError::Refused(why)),
            None => Ok(()),
        }
    }

    fn take_in(&mut self, mut other: impl FnMut(CollabEvent)) -> bool {
        let mut changed = false;
        let mut record_changed = false;
        let mut routes = self.taken_routes();
        for event in self.manager.poll() {
            let (envelope, decrypted_payload) = match event {
                CollabEvent::Document {
                    envelope,
                    decrypted_payload,
                    ..
                } => (envelope, decrypted_payload),
                CollabEvent::DocumentSnapshotRequested { route } => {
                    // The broker chose this connection as the one that answers a late joiner of
                    // the record or of one of its chunks: nobody else does, so it is answered
                    // here, with the whole of the document the route names.
                    if let Some((_, chunk)) = routes.iter().find(|(held, _)| *held == route) {
                        self.send_snapshot(*chunk);
                    }
                    continue;
                }
                event => {
                    other(event);
                    continue;
                }
            };
            // The envelope names the route it was sealed for: the record's, or the route of the
            // chunk its content belongs to.
            let mut taken = None;
            for (route, chunk) in routes.clone() {
                let Some(payload) =
                    validated_route_payload_with(&route, &envelope, decrypted_payload.as_deref())
                else {
                    continue;
                };
                let mutation =
                    match decode_routed_mutation(&route, payload, decrypted_payload.as_deref()) {
                        Ok(mutation) => mutation,
                        Err(error) => {
                            tracing::warn!(
                                ?error,
                                "workspace: a change that does not decode was not taken in"
                            );
                            break;
                        }
                    };
                let before = self
                    .chunks
                    .mirror(chunk)
                    .map(Doc::committed_heads)
                    .unwrap_or_default();
                let loaded = match &mutation {
                    DocumentMutation::Incremental(bytes) => {
                        self.open_chunk(chunk).load_incoming(bytes)
                    }
                    DocumentMutation::Snapshot(bytes) => {
                        self.open_chunk(chunk).load_incoming_whole(bytes)
                    }
                };
                match loaded {
                    Ok(applied) => taken = Some((chunk, before, applied)),
                    Err(error) => {
                        tracing::warn!(%error, chunk, "workspace: a change was not taken in");
                    }
                }
                break;
            }
            let Some((chunk, before, applied)) = taken else {
                tracing::warn!("workspace: a change for another route was not taken in");
                continue;
            };
            // What is taken in counts as a change only when the chunk gained one: a change this
            // replica made comes back on its own route (the broker hands every holder what a
            // unit sent), and the chunk already holds it. The chunk's readers are told a change
            // once it holds it, so a change it held already is told to none of them.
            changed |= applied > 0;
            self.note_arrived(chunk, &before);
            if chunk == FIRST && applied > 0 {
                record_changed = true;
                // The record names which chunks its resources are in: a change of it may name
                // ones this replica holds none of yet.
                routes = self.taken_routes();
            }
        }
        if record_changed {
            self.chunks.reread_placement();
            self.join_chunks();
        }
        changed
    }
}

/// Whether `a` and `b` name the same heads, in whatever order.
fn heads_equal(a: &[ChangeHash], b: &[ChangeHash]) -> bool {
    a.len() == b.len() && a.iter().all(|head| b.contains(head))
}

/// What a tilolang host's history verbs say, without the toolkit: a step as the platform's
/// history sentence in `locale`, a person by the handle their changes carry. The UI half renders
/// History in the reader's own words.
#[derive(Clone, Debug)]
pub struct HostWords {
    locale: String,
}

impl HostWords {
    /// The words of `locale` (`en-US`, `de-DE`).
    #[must_use]
    pub fn new(locale: impl Into<String>) -> Self {
        Self {
            locale: locale.into(),
        }
    }
}

impl ResolveAuthor for HostWords {
    fn resolve(&self, actor_hex: &str) -> HistoryAuthor {
        let name = actor_handle(actor_hex).unwrap_or_else(|| UNKNOWN_AUTHOR_NAME.to_owned());
        HistoryAuthor::new(name, stable_hue(actor_hex), short_id(actor_hex))
    }
}

impl HistoryWords for HostWords {
    fn sentence(&self, entry: &ActionContext) -> String {
        tilo_history::Reader::new(&self.locale).sentence(entry)
    }
}

impl crate::instance::HistoryAsk {
    /// The history verb that carries it out on the record.
    #[must_use]
    pub fn edit(&self) -> HistoryEdit {
        let undo = |run: Option<&String>, step: Option<&String>| HistoryEdit::Undo {
            steps: None,
            run: run.cloned(),
            by: None,
            to: None,
            step: step.cloned(),
        };
        match self {
            Self::Undo => undo(None, None),
            Self::Redo => HistoryEdit::Redo { steps: None },
            Self::UndoStep(change) => undo(None, Some(change)),
            Self::UndoRun(run) => undo(Some(run), None),
        }
    }
}

/// A history verb applied to a workspace record: the edit [`verbs`] applies, decoding the record's
/// recorded actions with the workspace's registry ([`register`]) and speaking the record's words.
#[derive(Debug)]
pub struct HostHistoryEdit {
    edit: HistoryEdit,
    words: HostWords,
}

impl<'de> serde::Deserialize<'de> for HostHistoryEdit {
    /// A history verb a peer recorded, replayed here: what it says is never shown, so it speaks
    /// the platform's source language.
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Self {
            edit: HistoryEdit::deserialize(deserializer)?,
            words: HostWords::new("en-US"),
        })
    }
}

impl tilo_actions::Action for HostHistoryEdit {
    fn change_kind(&self) -> tilo_actions::ChangeKind {
        self.edit.change_kind()
    }

    fn describe(&self, doc: &tilo_actions::Doc) -> Option<tilo_actions::Described> {
        Some(self.edit.described(doc))
    }

    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
        self.edit.apply_registered(doc, register, &self.words)
    }
}

/// Register what a workspace record's changes record: the runtime's planned actions and the history
/// verbs.
pub fn register(registry: &mut ActionRegistry) {
    registry.register::<DynAction>(ACTION_TYPE);
    registry.register::<HostHistoryEdit>(EDIT_TYPE);
}
